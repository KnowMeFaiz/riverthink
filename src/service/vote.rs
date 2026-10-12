use sqlx::SqliteConnection;

use crate::config::Config;
use crate::db::{Db, immediate};
use crate::domain::calendar::{format_wib, votes_locked};
use crate::domain::vote::{VoteDecision, decide};
use crate::error::{AppError, AppResult};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VoteOutcome {
    Cast,
    Moved,
}

/// True while a running election is in week 4 (or awaiting finalization).
pub async fn locked(conn: &mut SqliteConnection, now: i64) -> AppResult<bool> {
    let r: Option<i64> = sqlx::query_scalar(
        "SELECT starts_at FROM elections WHERE status = 'running' ORDER BY target_generation DESC LIMIT 1",
    )
    .fetch_optional(conn)
    .await?;
    Ok(r.is_some_and(|s| votes_locked(s, now)))
}

/// Cast or move the user's single vote. The whole check-and-write runs inside
/// one `BEGIN IMMEDIATE` transaction, and `votes.user_id` is the primary key,
/// so concurrent requests can never produce two votes for one account.
pub async fn cast(pool: &Db, cfg: &Config, now: i64, user_id: i64, ideology_id: i64) -> AppResult<VoteOutcome> {
    let mut tx = immediate(pool).await?;

    let user: Option<(String, String)> = sqlx::query_as("SELECT status, channel FROM users WHERE id = ?1")
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await?;
    let (ustatus, uchan) = user.ok_or(AppError::Unauthorized)?;
    if ustatus != "active" {
        return Err(AppError::Forbidden("Akunmu sedang dibatasi, tidak bisa memberi dukungan.".into()));
    }

    let ide: Option<(String, String)> = sqlx::query_as("SELECT status, channel FROM ideologies WHERE id = ?1")
        .bind(ideology_id)
        .fetch_optional(&mut *tx)
        .await?;
    let (istatus, ichan) = ide.ok_or(AppError::NotFound)?;
    if istatus != "active" {
        return Err(AppError::Conflict("Ideologi ini sedang dibatasi moderator.".into()));
    }
    if ichan != uchan {
        return Err(AppError::Forbidden(format!(
            "Channel asalmu #{uchan}; dukungan hanya bisa diberikan ke ideologi di channel itu (diskusi tetap bebas di mana pun)."
        )));
    }

    let current: Option<(i64, i64)> = sqlx::query_as("SELECT ideology_id, cast_at FROM votes WHERE user_id = ?1")
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await?;
    let lock = locked(&mut tx, now).await?;

    match decide(current, ideology_id, now, lock, cfg.vote_cooldown_secs) {
        VoteDecision::Allow => {}
        VoteDecision::AlreadySupporting => {
            return Err(AppError::Conflict("Kamu sudah mendukung ideologi ini.".into()));
        }
        VoteDecision::Cooldown { until } => {
            return Err(AppError::Conflict(format!(
                "Pindah dukungan baru bisa lagi pada {}.",
                format_wib(until)
            )));
        }
        VoteDecision::LockedFinalWeek => {
            return Err(AppError::Conflict(
                "Vote dikunci selama minggu final election sampai hasil diumumkan.".into(),
            ));
        }
    }

    sqlx::query(
        "INSERT INTO votes (user_id, ideology_id, channel, cast_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (user_id) DO UPDATE SET ideology_id = excluded.ideology_id, cast_at = excluded.cast_at",
    )
    .bind(user_id)
    .bind(ideology_id)
    .bind(&uchan)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO vote_log (user_id, from_ideology_id, to_ideology_id, at) VALUES (?1, ?2, ?3, ?4)")
        .bind(user_id)
        .bind(current.map(|c| c.0))
        .bind(ideology_id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(if current.is_some() { VoteOutcome::Moved } else { VoteOutcome::Cast })
}

#[derive(Clone, Debug)]
pub struct MyVote {
    pub ideology_id: i64,
    pub name: String,
    pub cast_at: i64,
}

pub async fn current(pool: &Db, user_id: i64) -> AppResult<Option<MyVote>> {
    let r: Option<(i64, String, i64)> = sqlx::query_as(
        "SELECT v.ideology_id, i.name, v.cast_at FROM votes v JOIN ideologies i ON i.id = v.ideology_id WHERE v.user_id = ?1",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    Ok(r.map(|(ideology_id, name, cast_at)| MyVote { ideology_id, name, cast_at }))
}

/// Read-only preview for the UI. `cast` stays the single authority.
pub async fn preview(pool: &Db, cfg: &Config, now: i64, user_id: i64, ideology_id: i64) -> AppResult<VoteDecision> {
    let current: Option<(i64, i64)> = sqlx::query_as("SELECT ideology_id, cast_at FROM votes WHERE user_id = ?1")
        .bind(user_id)
        .fetch_optional(pool)
        .await?;
    let mut c = pool.acquire().await?;
    let lock = locked(&mut c, now).await?;
    Ok(decide(current, ideology_id, now, lock, cfg.vote_cooldown_secs))
}
