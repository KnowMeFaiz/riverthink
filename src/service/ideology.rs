use sqlx::{FromRow, SqliteConnection};

use crate::config::Config;
use crate::db::{Db, immediate};
use crate::domain::capacity::capacity;
use crate::domain::channel::Channel;
use crate::error::{AppError, AppResult};
use crate::model::User;
use crate::service::chars;

/// The generation an ideology created *now* is founded in: the latest finalized
/// generation, or 1 during the genesis election (docs/DECISIONS.md D-08).
pub async fn founding_generation(conn: &mut SqliteConnection) -> AppResult<i64> {
    Ok(sqlx::query_scalar("SELECT COALESCE(MAX(number), 1) FROM generations")
        .fetch_one(conn)
        .await?)
}

#[derive(Clone, Debug)]
pub struct CapacityInfo {
    pub generation: i64,
    pub used: i64,
    pub cap: u32,
    pub active_users: i64,
}

impl CapacityInfo {
    pub fn is_full(&self) -> bool {
        self.used >= i64::from(self.cap)
    }
}

/// Rule A for one channel, evaluated from live data.
pub async fn capacity_info(
    conn: &mut SqliteConnection,
    cfg: &Config,
    now: i64,
    channel: Channel,
) -> AppResult<CapacityInfo> {
    let generation = founding_generation(&mut *conn).await?;
    let active_users: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM users WHERE channel = ?1 AND status = 'active' AND last_active_at >= ?2",
    )
    .bind(channel.code())
    .bind(now - cfg.active_window_secs)
    .fetch_one(&mut *conn)
    .await?;
    let used: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM ideologies WHERE channel = ?1 AND founded_generation = ?2",
    )
    .bind(channel.code())
    .bind(generation)
    .fetch_one(&mut *conn)
    .await?;
    let cap = capacity(u32::try_from(active_users).unwrap_or(u32::MAX), cfg.s_min);
    Ok(CapacityInfo { generation, used, cap, active_users })
}

pub async fn capacity_info_pool(pool: &Db, cfg: &Config, now: i64, channel: Channel) -> AppResult<CapacityInfo> {
    let mut c = pool.acquire().await?;
    capacity_info(&mut c, cfg, now, channel).await
}

fn normalize_name(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub async fn create(
    pool: &Db,
    cfg: &Config,
    now: i64,
    user: &User,
    name: &str,
    description: &str,
) -> AppResult<i64> {
    let name = normalize_name(name);
    let description = description.trim();
    if !(3..=40).contains(&chars(&name)) || name.chars().any(|c| c.is_control()) {
        return Err(AppError::Invalid("Nama ideologi 3-40 karakter.".into()));
    }
    if !(10..=2000).contains(&chars(description)) {
        return Err(AppError::Invalid("Deskripsi dan prinsip 10-2000 karakter.".into()));
    }

    let mut tx = immediate(pool).await?;
    let status: Option<String> = sqlx::query_scalar("SELECT status FROM users WHERE id = ?1")
        .bind(user.id)
        .fetch_optional(&mut *tx)
        .await?;
    match status.as_deref() {
        Some("active") => {}
        Some(_) => return Err(AppError::Forbidden("Akunmu sedang dibatasi, tidak bisa membuat ideologi.".into())),
        None => return Err(AppError::Unauthorized),
    }

    let channel = user.channel();
    let info = capacity_info(&mut tx, cfg, now, channel).await?;
    if info.is_full() {
        return Err(AppError::Conflict(format!(
            "Kuota pendirian ideologi channel #{} untuk generasi {} penuh ({}/{}). \
             Kuota dihitung dari 10% pengguna aktif channel; coba lagi di generasi berikutnya.",
            channel.code(),
            info.generation,
            info.used,
            info.cap
        )));
    }
    let mine: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM ideologies WHERE creator_id = ?1 AND founded_generation = ?2",
    )
    .bind(user.id)
    .bind(info.generation)
    .fetch_one(&mut *tx)
    .await?;
    if mine > 0 {
        return Err(AppError::Conflict(
            "Kamu sudah mendirikan satu ideologi di generasi ini (batas 1 per akun per generasi).".into(),
        ));
    }

    let id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO ideologies (name, creator_id, description, channel, founded_generation, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6) RETURNING id",
    )
    .bind(&name)
    .bind(user.id)
    .bind(description)
    .bind(channel.code())
    .bind(info.generation)
    .bind(now)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| match e {
        sqlx::Error::Database(d) if d.is_unique_violation() => {
            AppError::Conflict("Nama ideologi itu sudah dipakai.".into())
        }
        e => e.into(),
    })?;
    tx.commit().await?;
    Ok(id)
}

#[derive(Clone, Debug, FromRow)]
pub struct IdeologyRow {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub channel: String,
    pub founded_generation: i64,
    pub status: String,
    pub created_at: i64,
    pub creator_id: i64,
    pub creator: String,
    /// Active accounts currently supporting this ideology. Not a verified
    /// unique-human count (docs/DECISIONS.md D-10).
    pub support: i64,
    /// Visible Ambinote entries in the activity window by current supporters.
    pub activity: i64,
    pub last_nine_gen: Option<i64>,
    pub in_nine_now: i64,
}

impl IdeologyRow {
    pub fn channel(&self) -> Channel {
        Channel::parse(&self.channel).unwrap_or(Channel::E)
    }
}

macro_rules! ideology_select {
    () => {
        "SELECT i.id, i.name, i.description, i.channel, i.founded_generation, i.status, i.created_at,
                i.creator_id, u.username AS creator,
                (SELECT COUNT(*) FROM votes v JOIN users s ON s.id = v.user_id
                  WHERE v.ideology_id = i.id AND s.status = 'active') AS support,
                (SELECT COUNT(*) FROM notes n JOIN votes v ON v.user_id = n.author_id
                  WHERE v.ideology_id = i.id AND n.status = 'visible' AND n.created_at >= ?1) AS activity,
                (SELECT MAX(f.generation) FROM final_nine f WHERE f.ideology_id = i.id) AS last_nine_gen,
                EXISTS(SELECT 1 FROM final_nine f WHERE f.ideology_id = i.id
                        AND f.generation = (SELECT MAX(number) FROM generations)) AS in_nine_now
           FROM ideologies i JOIN users u ON u.id = i.creator_id"
    };
}

pub async fn list_by_channel(pool: &Db, cfg: &Config, now: i64, channel: Channel) -> AppResult<Vec<IdeologyRow>> {
    Ok(sqlx::query_as::<_, IdeologyRow>(concat!(
        ideology_select!(),
        " WHERE i.channel = ?2 ORDER BY support DESC, i.created_at ASC, i.id ASC"
    ))
    .bind(now - cfg.active_window_secs)
    .bind(channel.code())
    .fetch_all(pool)
    .await?)
}

pub async fn get(pool: &Db, cfg: &Config, now: i64, id: i64) -> AppResult<Option<IdeologyRow>> {
    Ok(sqlx::query_as::<_, IdeologyRow>(concat!(ideology_select!(), " WHERE i.id = ?2"))
        .bind(now - cfg.active_window_secs)
        .bind(id)
        .fetch_optional(pool)
        .await?)
}
