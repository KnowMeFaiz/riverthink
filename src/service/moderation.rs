//! Reporting, content/account/ideology restriction, appeals, audit log.
//! Moderation authority belongs to accounts with `role = 'admin'` only; it is
//! never derived from ideology popularity or Final Nine status.

use sqlx::{FromRow, SqliteConnection};

use crate::db::{Db, immediate};
use crate::error::{AppError, AppResult};
use crate::model::User;
use crate::service::chars;

const MAX_REPORTS_PER_DAY: i64 = 20;

fn kind_of(kind: &str) -> AppResult<&'static str> {
    match kind {
        "note" => Ok("note"),
        "ideology" => Ok("ideology"),
        "user" => Ok("user"),
        _ => Err(AppError::Invalid("Jenis target tidak dikenal.".into())),
    }
}

async fn target_exists(conn: &mut SqliteConnection, kind: &str, id: i64) -> AppResult<bool> {
    let n: i64 = match kind {
        "note" => sqlx::query_scalar("SELECT COUNT(*) FROM notes WHERE id = ?1"),
        "ideology" => sqlx::query_scalar("SELECT COUNT(*) FROM ideologies WHERE id = ?1"),
        _ => sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE id = ?1"),
    }
    .bind(id)
    .fetch_one(conn)
    .await?;
    Ok(n > 0)
}

pub async fn report(pool: &Db, now: i64, reporter: &User, kind: &str, target_id: i64, reason: &str) -> AppResult<()> {
    let kind = kind_of(kind)?;
    let reason = reason.trim();
    if !(3..=500).contains(&chars(reason)) {
        return Err(AppError::Invalid("Alasan laporan 3-500 karakter.".into()));
    }
    if kind == "user" && target_id == reporter.id {
        return Err(AppError::Invalid("Tidak bisa melaporkan diri sendiri.".into()));
    }
    let mut tx = immediate(pool).await?;
    let status: Option<String> = sqlx::query_scalar("SELECT status FROM users WHERE id = ?1")
        .bind(reporter.id)
        .fetch_optional(&mut *tx)
        .await?;
    if status.as_deref() != Some("active") {
        return Err(AppError::Forbidden("Akunmu sedang dibatasi, tidak bisa melapor.".into()));
    }
    if !target_exists(&mut tx, kind, target_id).await? {
        return Err(AppError::NotFound);
    }
    let today: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM reports WHERE reporter_id = ?1 AND created_at >= ?2")
        .bind(reporter.id)
        .bind(now - 86_400)
        .fetch_one(&mut *tx)
        .await?;
    if today >= MAX_REPORTS_PER_DAY {
        return Err(AppError::RateLimited);
    }
    sqlx::query("INSERT INTO reports (reporter_id, target_kind, target_id, reason, created_at) VALUES (?1, ?2, ?3, ?4, ?5)")
        .bind(reporter.id)
        .bind(kind)
        .bind(target_id)
        .bind(reason)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(|e| match e {
            sqlx::Error::Database(d) if d.is_unique_violation() => {
                AppError::Conflict("Kamu sudah melaporkan ini.".into())
            }
            e => e.into(),
        })?;
    tx.commit().await?;
    Ok(())
}

/// Set the moderation state of one target. Returns rows changed.
async fn set_state(conn: &mut SqliteConnection, kind: &str, id: i64, restricted: bool) -> AppResult<u64> {
    let r = match (kind, restricted) {
        ("note", true) => sqlx::query("UPDATE notes SET status = 'hidden' WHERE id = ?1"),
        ("note", false) => sqlx::query("UPDATE notes SET status = 'visible' WHERE id = ?1"),
        ("ideology", true) => sqlx::query("UPDATE ideologies SET status = 'restricted' WHERE id = ?1"),
        ("ideology", false) => sqlx::query("UPDATE ideologies SET status = 'active' WHERE id = ?1"),
        // Admin accounts cannot be restricted through moderation.
        ("user", true) => sqlx::query("UPDATE users SET status = 'restricted' WHERE id = ?1 AND role != 'admin'"),
        ("user", false) => sqlx::query("UPDATE users SET status = 'active' WHERE id = ?1"),
        _ => return Err(AppError::Invalid("Jenis target tidak dikenal.".into())),
    }
    .bind(id)
    .execute(conn)
    .await?;
    Ok(r.rows_affected())
}

async fn audit(conn: &mut SqliteConnection, actor: i64, now: i64, action: &str, kind: &str, id: i64, detail: &str) -> AppResult<()> {
    sqlx::query("INSERT INTO audit_log (actor_id, action, target_kind, target_id, detail, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)")
        .bind(actor)
        .bind(action)
        .bind(kind)
        .bind(id)
        .bind(detail)
        .bind(now)
        .execute(conn)
        .await?;
    Ok(())
}

/// Admin action: `hide`/`unhide` (note), `restrict`/`unrestrict` (user, ideology).
pub async fn act(pool: &Db, now: i64, admin: &User, kind: &str, target_id: i64, action: &str, note: &str) -> AppResult<()> {
    let kind = kind_of(kind)?;
    let restricted = match (kind, action) {
        ("note", "hide") | ("user", "restrict") | ("ideology", "restrict") => true,
        ("note", "unhide") | ("user", "unrestrict") | ("ideology", "unrestrict") => false,
        _ => return Err(AppError::Invalid("Aksi tidak dikenal.".into())),
    };
    let note = note.trim();
    if chars(note) > 500 {
        return Err(AppError::Invalid("Catatan maksimal 500 karakter.".into()));
    }
    let mut tx = immediate(pool).await?;
    let role: Option<String> = sqlx::query_scalar("SELECT role FROM users WHERE id = ?1")
        .bind(admin.id)
        .fetch_optional(&mut *tx)
        .await?;
    if role.as_deref() != Some("admin") {
        return Err(AppError::Forbidden("Hanya admin yang bisa melakukan ini.".into()));
    }
    if set_state(&mut tx, kind, target_id, restricted).await? == 0 {
        return Err(if target_exists(&mut tx, kind, target_id).await? {
            AppError::Forbidden("Akun admin tidak bisa dibatasi.".into())
        } else {
            AppError::NotFound
        });
    }
    audit(&mut tx, admin.id, now, &format!("{kind}.{action}"), kind, target_id, note).await?;
    if restricted {
        sqlx::query(
            "UPDATE reports SET status = 'actioned', resolved_by = ?1, resolved_at = ?2
              WHERE target_kind = ?3 AND target_id = ?4 AND status = 'open'",
        )
        .bind(admin.id)
        .bind(now)
        .bind(kind)
        .bind(target_id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn dismiss_report(pool: &Db, now: i64, admin: &User, report_id: i64, note: &str) -> AppResult<()> {
    let mut tx = immediate(pool).await?;
    let role: Option<String> = sqlx::query_scalar("SELECT role FROM users WHERE id = ?1")
        .bind(admin.id)
        .fetch_optional(&mut *tx)
        .await?;
    if role.as_deref() != Some("admin") {
        return Err(AppError::Forbidden("Hanya admin yang bisa melakukan ini.".into()));
    }
    let n = sqlx::query(
        "UPDATE reports SET status = 'dismissed', resolved_by = ?1, resolved_at = ?2 WHERE id = ?3 AND status = 'open'",
    )
    .bind(admin.id)
    .bind(now)
    .bind(report_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if n == 0 {
        return Err(AppError::NotFound);
    }
    audit(&mut tx, admin.id, now, "report.dismiss", "report", report_id, note.trim()).await?;
    tx.commit().await?;
    Ok(())
}

/// A user appeals a moderation action taken against them.
pub async fn appeal(pool: &Db, now: i64, user: &User, kind: &str, target_id: i64, message: &str) -> AppResult<()> {
    let kind = kind_of(kind)?;
    let message = message.trim();
    if !(3..=1000).contains(&chars(message)) {
        return Err(AppError::Invalid("Pesan banding 3-1000 karakter.".into()));
    }
    let mut tx = immediate(pool).await?;
    let n: i64 = match kind {
        "user" => sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE id = ?1 AND id = ?2 AND status = 'restricted'"),
        "note" => sqlx::query_scalar("SELECT COUNT(*) FROM notes WHERE id = ?1 AND author_id = ?2 AND status = 'hidden'"),
        _ => sqlx::query_scalar("SELECT COUNT(*) FROM ideologies WHERE id = ?1 AND creator_id = ?2 AND status = 'restricted'"),
    }
    .bind(target_id)
    .bind(user.id)
    .fetch_one(&mut *tx)
    .await?;
    if n == 0 {
        return Err(AppError::Forbidden("Tidak ada tindakan moderasi milikmu yang bisa dibanding untuk ini.".into()));
    }
    sqlx::query("INSERT INTO appeals (user_id, target_kind, target_id, message, created_at) VALUES (?1, ?2, ?3, ?4, ?5)")
        .bind(user.id)
        .bind(kind)
        .bind(target_id)
        .bind(message)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(|e| match e {
            sqlx::Error::Database(d) if d.is_unique_violation() => {
                AppError::Conflict("Bandingmu untuk ini masih diproses.".into())
            }
            e => e.into(),
        })?;
    tx.commit().await?;
    Ok(())
}

/// Admin resolves an appeal; granting reverses the original action.
pub async fn resolve_appeal(pool: &Db, now: i64, admin: &User, appeal_id: i64, grant: bool, note: &str) -> AppResult<()> {
    let mut tx = immediate(pool).await?;
    let role: Option<String> = sqlx::query_scalar("SELECT role FROM users WHERE id = ?1")
        .bind(admin.id)
        .fetch_optional(&mut *tx)
        .await?;
    if role.as_deref() != Some("admin") {
        return Err(AppError::Forbidden("Hanya admin yang bisa melakukan ini.".into()));
    }
    let row: Option<(String, i64)> =
        sqlx::query_as("SELECT target_kind, target_id FROM appeals WHERE id = ?1 AND status = 'open'")
            .bind(appeal_id)
            .fetch_optional(&mut *tx)
            .await?;
    let (kind, target_id) = row.ok_or(AppError::NotFound)?;
    let note = note.trim();
    if grant {
        set_state(&mut tx, &kind, target_id, false).await?;
    }
    sqlx::query(
        "UPDATE appeals SET status = ?1, resolved_by = ?2, resolved_at = ?3, admin_note = ?4 WHERE id = ?5",
    )
    .bind(if grant { "granted" } else { "denied" })
    .bind(admin.id)
    .bind(now)
    .bind(note)
    .bind(appeal_id)
    .execute(&mut *tx)
    .await?;
    audit(
        &mut tx,
        admin.id,
        now,
        if grant { "appeal.granted" } else { "appeal.denied" },
        &kind,
        target_id,
        note,
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

// ---- Read models ------------------------------------------------------------

#[derive(Clone, Debug, FromRow)]
pub struct ReportRow {
    pub id: i64,
    pub reporter: String,
    pub target_kind: String,
    pub target_id: i64,
    pub reason: String,
    pub created_at: i64,
    pub label: Option<String>,
}

#[derive(Clone, Debug, FromRow)]
pub struct AppealRow {
    pub id: i64,
    pub username: String,
    pub target_kind: String,
    pub target_id: i64,
    pub message: String,
    pub created_at: i64,
    pub label: Option<String>,
}

#[derive(Clone, Debug, FromRow)]
pub struct AuditRow {
    pub id: i64,
    pub actor: Option<String>,
    pub action: String,
    pub target_kind: String,
    pub target_id: i64,
    pub detail: String,
    pub at: i64,
}

#[derive(Clone, Debug)]
pub struct AdminOverview {
    pub reports: Vec<ReportRow>,
    pub appeals: Vec<AppealRow>,
    pub audit: Vec<AuditRow>,
}

pub async fn admin_overview(pool: &Db) -> AppResult<AdminOverview> {
    let reports = sqlx::query_as(
        "SELECT r.id, u.username AS reporter, r.target_kind, r.target_id, r.reason, r.created_at,
                CASE r.target_kind
                  WHEN 'note' THEN (SELECT title FROM notes WHERE id = r.target_id)
                  WHEN 'ideology' THEN (SELECT name FROM ideologies WHERE id = r.target_id)
                  ELSE (SELECT username FROM users WHERE id = r.target_id) END AS label
           FROM reports r JOIN users u ON u.id = r.reporter_id
          WHERE r.status = 'open' ORDER BY r.id LIMIT 100",
    )
    .fetch_all(pool)
    .await?;
    let appeals = sqlx::query_as(
        "SELECT a.id, u.username, a.target_kind, a.target_id, a.message, a.created_at,
                CASE a.target_kind
                  WHEN 'note' THEN (SELECT title FROM notes WHERE id = a.target_id)
                  WHEN 'ideology' THEN (SELECT name FROM ideologies WHERE id = a.target_id)
                  ELSE (SELECT username FROM users WHERE id = a.target_id) END AS label
           FROM appeals a JOIN users u ON u.id = a.user_id
          WHERE a.status = 'open' ORDER BY a.id LIMIT 100",
    )
    .fetch_all(pool)
    .await?;
    let audit = sqlx::query_as(
        "SELECT l.id, u.username AS actor, l.action, l.target_kind, l.target_id, l.detail, l.at
           FROM audit_log l LEFT JOIN users u ON u.id = l.actor_id ORDER BY l.id DESC LIMIT 50",
    )
    .fetch_all(pool)
    .await?;
    Ok(AdminOverview { reports, appeals, audit })
}

/// Things the user may appeal.
#[derive(Clone, Debug, Default)]
pub struct MyModeration {
    pub account_restricted: bool,
    pub hidden_notes: Vec<(i64, String)>,
    pub restricted_ideologies: Vec<(i64, String)>,
}

pub async fn my_moderation(pool: &Db, user: &User) -> AppResult<MyModeration> {
    let status: Option<String> = sqlx::query_scalar("SELECT status FROM users WHERE id = ?1")
        .bind(user.id)
        .fetch_optional(pool)
        .await?;
    let hidden_notes = sqlx::query_as("SELECT id, title FROM notes WHERE author_id = ?1 AND status = 'hidden' ORDER BY id DESC LIMIT 50")
        .bind(user.id)
        .fetch_all(pool)
        .await?;
    let restricted_ideologies = sqlx::query_as("SELECT id, name FROM ideologies WHERE creator_id = ?1 AND status = 'restricted'")
        .bind(user.id)
        .fetch_all(pool)
        .await?;
    Ok(MyModeration {
        account_restricted: status.as_deref() == Some("restricted"),
        hidden_notes,
        restricted_ideologies,
    })
}
