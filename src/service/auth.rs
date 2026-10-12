use std::sync::OnceLock;

use argon2::password_hash::{PasswordHasher, PasswordVerifier, phc::PasswordHash};
use argon2::Argon2;
use sha2::{Digest, Sha256};

use crate::config::Config;
use crate::db::{Db, immediate};
use crate::domain::channel::Channel;
use crate::error::{AppError, AppResult};
use crate::model::User;

const USER_COLS: &str = "u.id, u.username, u.channel, u.role, u.status, u.created_at, u.last_active_at";

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn random_token() -> String {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).expect("OS randomness unavailable");
    to_hex(&b)
}

fn token_hash(token: &str) -> String {
    to_hex(&Sha256::digest(token.as_bytes()))
}

fn hash_password(pw: &str) -> AppResult<String> {
    Argon2::default()
        .hash_password(pw.as_bytes())
        .map(|h| h.to_string())
        .map_err(|e| AppError::Internal(format!("hash: {e}")))
}

fn verify_password(pw: &str, hash: &str) -> bool {
    PasswordHash::new(hash)
        .map(|p| Argon2::default().verify_password(pw.as_bytes(), &p).is_ok())
        .unwrap_or(false)
}

/// Used to spend comparable time when the username does not exist.
fn dummy_hash() -> &'static str {
    static H: OnceLock<String> = OnceLock::new();
    H.get_or_init(|| hash_password("riverthink-dummy-password").unwrap_or_default())
}

fn valid_username(u: &str) -> bool {
    (3..=24).contains(&u.chars().count()) && u.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Normalises a phone number to digits with country code (leading 0 = Indonesia, +62).
pub fn normalize_phone(raw: &str) -> Option<String> {
    let digits: String = raw.chars().filter(|c| c.is_ascii_digit()).collect();
    if raw.chars().any(|c| !(c.is_ascii_digit() || " +-()".contains(c))) {
        return None;
    }
    let n = match digits.strip_prefix('0') {
        Some(rest) => format!("62{rest}"),
        None => digits,
    };
    (9..=15).contains(&n.len()).then_some(n)
}

fn phone_hash(cfg: &Config, normalized: &str) -> String {
    let mut h = Sha256::new();
    h.update(cfg.phone_pepper.as_bytes());
    h.update(b"|phone|");
    h.update(normalized.as_bytes());
    to_hex(&h.finalize())
}

pub async fn register(
    pool: &Db,
    cfg: &Config,
    now: i64,
    username: &str,
    password: &str,
    channel: &str,
    phone: Option<&str>,
) -> AppResult<i64> {
    let username = username.trim();
    if !valid_username(username) {
        return Err(AppError::Invalid(
            "Username 3-24 karakter: huruf, angka, atau underscore.".into(),
        ));
    }
    if !(8..=128).contains(&password.chars().count()) {
        return Err(AppError::Invalid("Password minimal 8 karakter (maks 128).".into()));
    }
    let channel = Channel::parse(channel)
        .ok_or_else(|| AppError::Invalid("Pilih channel E, Z, atau D.".into()))?;
    let phone_hash = match phone.map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => Some(phone_hash(
            cfg,
            &normalize_phone(p).ok_or_else(|| AppError::Invalid("Nomor telepon tidak valid.".into()))?,
        )),
        None if cfg.require_phone => {
            return Err(AppError::Invalid("Nomor telepon wajib diisi.".into()));
        }
        None => None,
    };

    let pw = password.to_owned();
    let hash = tokio::task::spawn_blocking(move || hash_password(&pw))
        .await
        .map_err(|e| AppError::Internal(e.to_string()))??;

    let mut tx = immediate(pool).await?;
    let id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO users (username, password_hash, channel, created_at, last_active_at, phone_hash)
         VALUES (?1, ?2, ?3, ?4, ?4, ?5) RETURNING id",
    )
    .bind(username)
    .bind(hash)
    .bind(channel.code())
    .bind(now)
    .bind(phone_hash)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| match e {
        sqlx::Error::Database(d) if d.is_unique_violation() => {
            AppError::Conflict("Username sudah dipakai.".into())
        }
        sqlx::Error::Database(d) if d.message().contains("phone limit") => AppError::Conflict(
            "Nomor ini sudah dipakai akun lain. Hapus akun lama dulu untuk mengganti.".into(),
        ),
        e => e.into(),
    })?;
    tx.commit().await?;
    Ok(id)
}

/// Returns the raw session token (to be put in the cookie).
pub async fn login(
    pool: &Db,
    cfg: &Config,
    now: i64,
    username: &str,
    password: &str,
) -> AppResult<String> {
    let bad = || AppError::Invalid("Username atau password salah.".into());
    let row: Option<(i64, String)> =
        sqlx::query_as("SELECT id, password_hash FROM users WHERE username = ?1")
            .bind(username.trim())
            .fetch_optional(pool)
            .await?;

    let pw = password.to_owned();
    let (found, hash) = match row {
        Some((id, h)) => (Some(id), h),
        None => (None, dummy_hash().to_owned()),
    };
    let ok = tokio::task::spawn_blocking(move || verify_password(&pw, &hash))
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let user_id = match (found, ok) {
        (Some(id), true) => id,
        _ => return Err(bad()),
    };

    let token = random_token();
    let mut tx = immediate(pool).await?;
    sqlx::query("INSERT INTO sessions (token_hash, user_id, created_at, expires_at) VALUES (?1, ?2, ?3, ?4)")
        .bind(token_hash(&token))
        .bind(user_id)
        .bind(now)
        .bind(now + cfg.session_ttl_secs)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE users SET last_active_at = ?1 WHERE id = ?2")
        .bind(now)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(token)
}

/// Resolve a cookie token to a user. Refreshes `last_active_at` at most once
/// per hour so reads do not turn into constant writes.
pub async fn session_user(pool: &Db, now: i64, token: &str) -> AppResult<Option<User>> {
    let sql = format!(
        "SELECT {USER_COLS} FROM sessions s JOIN users u ON u.id = s.user_id
         WHERE s.token_hash = ?1 AND s.expires_at > ?2"
    );
    let user: Option<User> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(token_hash(token))
        .bind(now)
        .fetch_optional(pool)
        .await?;
    let Some(mut user) = user else { return Ok(None) };
    if now - user.last_active_at >= 3600 {
        sqlx::query("UPDATE users SET last_active_at = ?1 WHERE id = ?2")
            .bind(now)
            .bind(user.id)
            .execute(pool)
            .await?;
        user.last_active_at = now;
    }
    Ok(Some(user))
}

pub async fn logout(pool: &Db, token: &str) -> AppResult<()> {
    sqlx::query("DELETE FROM sessions WHERE token_hash = ?1")
        .bind(token_hash(token))
        .execute(pool)
        .await?;
    Ok(())
}

/// Promote an existing account to admin (used by the `admin-grant` CLI command).
pub async fn grant_admin(pool: &Db, username: &str) -> AppResult<()> {
    let n = sqlx::query("UPDATE users SET role = 'admin' WHERE username = ?1")
        .bind(username.trim())
        .execute(pool)
        .await?
        .rows_affected();
    if n == 0 {
        return Err(AppError::NotFound);
    }
    Ok(())
}

/// Deletes an account after password confirmation. The row is anonymised, not removed, so
/// notes and founded ideologies keep their integrity: the username becomes `deleted_<id>`,
/// login is impossible, the vote is withdrawn, sessions end and the phone slot is freed.
/// Admins cannot delete themselves this way (revoke the role via CLI first).
pub async fn delete_account(pool: &Db, now: i64, user_id: i64, password: &str) -> AppResult<()> {
    let row: Option<(String, String, Option<i64>)> =
        sqlx::query_as("SELECT password_hash, role, deleted_at FROM users WHERE id = ?1")
            .bind(user_id)
            .fetch_optional(pool)
            .await?;
    let (hash, role, deleted) = row.ok_or(AppError::NotFound)?;
    if deleted.is_some() {
        return Err(AppError::NotFound);
    }
    if role == "admin" {
        return Err(AppError::Forbidden("Akun admin tidak bisa dihapus dari web.".into()));
    }
    let pw = password.to_owned();
    let ok = tokio::task::spawn_blocking(move || verify_password(&pw, &hash))
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    if !ok {
        return Err(AppError::Invalid("Password salah.".into()));
    }
    let mut tx = immediate(pool).await?;
    sqlx::query("DELETE FROM votes WHERE user_id = ?1").bind(user_id).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM sessions WHERE user_id = ?1").bind(user_id).execute(&mut *tx).await?;
    sqlx::query(
        "UPDATE users SET username = 'deleted_' || id, password_hash = '!', phone_hash = NULL,
                status = 'restricted', deleted_at = ?2 WHERE id = ?1",
    )
    .bind(user_id)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}
