//! Election scheduler and generation transitions (docs/DECISIONS.md D-06..D-09).
//!
//! `tick` is idempotent and safe to call concurrently: each step runs in its
//! own `BEGIN IMMEDIATE` transaction and re-evaluates what is due *inside*
//! that transaction, so two callers can never apply the same step twice, and a
//! failed finalization rolls back completely (no partially replaced Final Nine).

use sqlx::{FromRow, SqliteConnection};

use crate::db::{Db, immediate};
use crate::domain::calendar::{
    ELECTION_LEN, WEEK, election_start_for, generation_end, stage_due, votes_locked,
};
use crate::domain::channel::Channel;
use crate::domain::election::{Candidate, select};
use crate::error::AppResult;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    /// Start the election for `target` (genesis when target == 1).
    Create { target: i64, starts_at: i64, ends_at: i64 },
    /// Run selection `stage` of election `election_id`.
    Advance { election_id: i64, stage: i64 },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TickReport {
    pub elections_started: u32,
    pub selections_run: u32,
    pub generations_finalized: u32,
}

#[derive(FromRow)]
struct ElectionRow {
    id: i64,
    target_generation: i64,
    starts_at: i64,
    ends_at: i64,
    stage: i64,
    status: String,
}

/// Pure read: what (if anything) is due at `now`.
async fn next_action(conn: &mut SqliteConnection, now: i64) -> AppResult<Option<Action>> {
    let last: Option<ElectionRow> = sqlx::query_as(
        "SELECT id, target_generation, starts_at, ends_at, stage, status
           FROM elections ORDER BY target_generation DESC LIMIT 1",
    )
    .fetch_optional(&mut *conn)
    .await?;

    Ok(match last {
        // Very first run: the genesis election starts now and runs four weeks.
        None => Some(Action::Create { target: 1, starts_at: now, ends_at: now + ELECTION_LEN }),
        Some(e) if e.status == "running" => {
            let due = i64::from(stage_due(e.starts_at, now));
            (e.stage < due).then_some(Action::Advance { election_id: e.id, stage: e.stage + 1 })
        }
        Some(e) => {
            let gen_end: i64 = sqlx::query_scalar("SELECT ends_at FROM generations WHERE number = ?1")
                .bind(e.target_generation)
                .fetch_one(&mut *conn)
                .await?;
            let start = election_start_for(gen_end);
            (now >= start).then_some(Action::Create {
                target: e.target_generation + 1,
                starts_at: start,
                ends_at: gen_end,
            })
        }
    })
}

/// Apply everything that is due at `now`. Returns what happened.
pub async fn tick(pool: &Db, now: i64) -> AppResult<TickReport> {
    let mut report = TickReport::default();
    loop {
        // Cheap read first so an idle minute never takes the write lock.
        {
            let mut c = pool.acquire().await?;
            if next_action(&mut c, now).await?.is_none() {
                break;
            }
        }
        let mut tx = immediate(pool).await?;
        // Authoritative re-check: another caller may have done the step already.
        let Some(action) = next_action(&mut tx, now).await? else {
            tx.commit().await?;
            continue;
        };
        match action {
            Action::Create { target, starts_at, ends_at } => {
                create_election(&mut tx, target, starts_at, ends_at).await?;
                report.elections_started += 1;
            }
            Action::Advance { election_id, stage } => {
                let finalized = run_selection(&mut tx, election_id, stage, now).await?;
                report.selections_run += 1;
                if finalized {
                    report.generations_finalized += 1;
                }
            }
        }
        tx.commit().await?;
    }
    sqlx::query("DELETE FROM sessions WHERE expires_at < ?1")
        .bind(now)
        .execute(pool)
        .await?;
    Ok(report)
}

async fn audit(conn: &mut SqliteConnection, now: i64, action: &str, kind: &str, id: i64, detail: &str) -> AppResult<()> {
    sqlx::query("INSERT INTO audit_log (actor_id, action, target_kind, target_id, detail, at) VALUES (NULL, ?1, ?2, ?3, ?4, ?5)")
        .bind(action)
        .bind(kind)
        .bind(id)
        .bind(detail)
        .bind(now)
        .execute(conn)
        .await?;
    Ok(())
}

async fn create_election(conn: &mut SqliteConnection, target: i64, starts_at: i64, ends_at: i64) -> AppResult<()> {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO elections (target_generation, starts_at, ends_at) VALUES (?1, ?2, ?3) RETURNING id",
    )
    .bind(target)
    .bind(starts_at)
    .bind(ends_at)
    .fetch_one(&mut *conn)
    .await?;
    // Regular elections freeze the field now. The genesis election (nothing
    // exists yet at launch) uses week 1 as the founding window and freezes
    // at its first selection instead (see `run_selection`).
    if target > 1 {
        freeze_field(conn, id, target).await?;
    }
    audit(conn, starts_at, "election.started", "election", id, &format!("target generation {target}")).await
}

/// Freeze the candidate field: every active ideology that exists right now.
/// Ideologies founded later cannot join this election (no retroactive changes).
async fn freeze_field(conn: &mut SqliteConnection, election_id: i64, target: i64) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO election_entries (election_id, ideology_id, channel, was_incumbent)
         SELECT ?1, i.id, i.channel,
                EXISTS(SELECT 1 FROM final_nine f WHERE f.ideology_id = i.id AND f.generation = ?2)
           FROM ideologies i WHERE i.status = 'active'",
    )
    .bind(election_id)
    .bind(target - 1)
    .execute(conn)
    .await?;
    Ok(())
}

/// Run weekly selection `stage` for all three channels. Stage 4 finalizes the
/// election and starts the new generation, all inside the caller's transaction.
/// Returns true when the generation was finalized.
async fn run_selection(conn: &mut SqliteConnection, election_id: i64, stage: i64, now: i64) -> AppResult<bool> {
    let (target, ends_at): (i64, i64) =
        sqlx::query_as("SELECT target_generation, ends_at FROM elections WHERE id = ?1")
            .bind(election_id)
            .fetch_one(&mut *conn)
            .await?;

    if stage == 1 && target == 1 {
        freeze_field(conn, election_id, target).await?;
    }

    for channel in Channel::ALL {
        // Remaining candidates with their live support (active accounts only).
        let rows: Vec<(i64, i64, String, i64)> = sqlx::query_as(
            "SELECT e.ideology_id, i.created_at, i.status,
                    (SELECT COUNT(*) FROM votes v JOIN users u ON u.id = v.user_id
                      WHERE v.ideology_id = e.ideology_id AND u.status = 'active') AS support
               FROM election_entries e JOIN ideologies i ON i.id = e.ideology_id
              WHERE e.election_id = ?1 AND e.channel = ?2 AND e.eliminated_stage IS NULL",
        )
        .bind(election_id)
        .bind(channel.code())
        .fetch_all(&mut *conn)
        .await?;

        let mut candidates = Vec::with_capacity(rows.len());
        for (ideology_id, created_at, status, support) in rows {
            if status != "active" {
                // Restricted by a moderator since the election started.
                eliminate(conn, election_id, ideology_id, stage).await?;
            } else {
                candidates.push(Candidate { ideology_id, support, created_at });
            }
        }

        let ranked = select(stage as u8, candidates);
        for r in &ranked {
            sqlx::query(
                "INSERT INTO election_stage_results (election_id, stage, ideology_id, channel, support, rank, advanced)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )
            .bind(election_id)
            .bind(stage)
            .bind(r.ideology_id)
            .bind(channel.code())
            .bind(r.support)
            .bind(r.rank as i64)
            .bind(i64::from(r.advanced))
            .execute(&mut *conn)
            .await?;
            if !r.advanced {
                eliminate(conn, election_id, r.ideology_id, stage).await?;
            }
        }

        if stage == 4 {
            if !ranked.iter().any(|r| r.advanced) {
                continue;
            }
            // Generation row is created once, before the first seat.
            ensure_generation(conn, target, ends_at).await?;
            for (slot, r) in ranked.iter().filter(|r| r.advanced).enumerate() {
                sqlx::query(
                    "INSERT INTO final_nine (generation, channel, slot, ideology_id, support) VALUES (?1, ?2, ?3, ?4, ?5)",
                )
                .bind(target)
                .bind(channel.code())
                .bind(slot as i64 + 1)
                .bind(r.ideology_id)
                .bind(r.support)
                .execute(&mut *conn)
                .await?;
            }
        }
    }

    sqlx::query("UPDATE elections SET stage = ?1 WHERE id = ?2")
        .bind(stage)
        .bind(election_id)
        .execute(&mut *conn)
        .await?;

    if stage == 4 {
        // A generation exists even if no channel produced a winner.
        ensure_generation(conn, target, ends_at).await?;
        sqlx::query("UPDATE elections SET status = 'finalized' WHERE id = ?1")
            .bind(election_id)
            .execute(&mut *conn)
            .await?;
        audit(conn, now, "generation.started", "generation", target, "final nine finalized").await?;
        return Ok(true);
    }
    audit(conn, now, "election.selection", "election", election_id, &format!("selection {stage}")).await?;
    Ok(false)
}

async fn eliminate(conn: &mut SqliteConnection, election_id: i64, ideology_id: i64, stage: i64) -> AppResult<()> {
    sqlx::query("UPDATE election_entries SET eliminated_stage = ?1 WHERE election_id = ?2 AND ideology_id = ?3")
        .bind(stage)
        .bind(election_id)
        .bind(ideology_id)
        .execute(conn)
        .await?;
    Ok(())
}

/// The new generation begins exactly when its election ends and lasts six
/// calendar months (Asia/Jakarta).
async fn ensure_generation(conn: &mut SqliteConnection, number: i64, starts_at: i64) -> AppResult<()> {
    sqlx::query("INSERT OR IGNORE INTO generations (number, starts_at, ends_at) VALUES (?1, ?2, ?3)")
        .bind(number)
        .bind(starts_at)
        .bind(generation_end(starts_at))
        .execute(conn)
        .await?;
    Ok(())
}

// ---- Read models ------------------------------------------------------------

#[derive(Clone, Debug, FromRow)]
pub struct NineRow {
    pub generation: i64,
    pub channel: String,
    pub slot: i64,
    pub ideology_id: i64,
    pub name: String,
    /// Support snapshot at the final selection.
    pub support_final: i64,
    /// Live support right now.
    pub support_now: i64,
}

macro_rules! nine_select {
    () => {
        "SELECT f.generation, f.channel, f.slot, f.ideology_id, i.name, f.support AS support_final,
                (SELECT COUNT(*) FROM votes v JOIN users u ON u.id = v.user_id
                  WHERE v.ideology_id = f.ideology_id AND u.status = 'active') AS support_now
           FROM final_nine f JOIN ideologies i ON i.id = f.ideology_id"
    };
}

#[derive(Clone, Debug, FromRow)]
pub struct Generation {
    pub number: i64,
    pub starts_at: i64,
    pub ends_at: i64,
}

pub async fn current_generation(pool: &Db) -> AppResult<Option<Generation>> {
    Ok(sqlx::query_as("SELECT number, starts_at, ends_at FROM generations ORDER BY number DESC LIMIT 1")
        .fetch_optional(pool)
        .await?)
}

/// The official Final Nine of the latest finalized generation.
pub async fn current_nine(pool: &Db) -> AppResult<Vec<NineRow>> {
    Ok(sqlx::query_as(concat!(
        nine_select!(),
        " WHERE f.generation = (SELECT MAX(number) FROM generations) ORDER BY f.channel, f.slot"
    ))
    .fetch_all(pool)
    .await?)
}

#[derive(Clone, Debug)]
pub struct GenerationHistory {
    pub generation: Generation,
    pub rows: Vec<NineRow>,
}

/// Every generation's Final Nine, newest first.
pub async fn history(pool: &Db) -> AppResult<Vec<GenerationHistory>> {
    let gens: Vec<Generation> =
        sqlx::query_as("SELECT number, starts_at, ends_at FROM generations ORDER BY number DESC")
            .fetch_all(pool)
            .await?;
    let rows: Vec<NineRow> = sqlx::query_as(concat!(
        nine_select!(),
        " ORDER BY f.generation DESC, f.channel, f.slot"
    ))
    .fetch_all(pool)
    .await?;
    Ok(gens
        .into_iter()
        .map(|g| {
            let rs = rows.iter().filter(|r| r.generation == g.number).cloned().collect();
            GenerationHistory { generation: g, rows: rs }
        })
        .collect())
}

#[derive(Clone, Debug, FromRow)]
pub struct EntryView {
    pub ideology_id: i64,
    pub name: String,
    pub channel: String,
    pub support_now: i64,
    pub eliminated_stage: Option<i64>,
    pub was_incumbent: i64,
}

#[derive(Clone, Debug)]
pub struct RunningElection {
    pub target_generation: i64,
    pub starts_at: i64,
    pub ends_at: i64,
    /// Completed selections (0..=3 while running).
    pub stage: i64,
    pub votes_locked: bool,
    pub next_selection_at: i64,
    pub entries: Vec<EntryView>,
}

#[derive(Clone, Debug)]
pub struct ElectionStatus {
    pub running: Option<RunningElection>,
    /// When the next election will start, if none is running.
    pub next_election_at: Option<i64>,
}

pub async fn status(pool: &Db, now: i64) -> AppResult<ElectionStatus> {
    let last: Option<ElectionRow> = sqlx::query_as(
        "SELECT id, target_generation, starts_at, ends_at, stage, status
           FROM elections ORDER BY target_generation DESC LIMIT 1",
    )
    .fetch_optional(pool)
    .await?;
    let Some(e) = last else {
        return Ok(ElectionStatus { running: None, next_election_at: None });
    };
    if e.status == "running" {
        let entries: Vec<EntryView> = sqlx::query_as(
            "SELECT e.ideology_id, i.name, e.channel, e.eliminated_stage, e.was_incumbent,
                    (SELECT COUNT(*) FROM votes v JOIN users u ON u.id = v.user_id
                      WHERE v.ideology_id = e.ideology_id AND u.status = 'active') AS support_now
               FROM election_entries e JOIN ideologies i ON i.id = e.ideology_id
              WHERE e.election_id = ?1
              ORDER BY e.channel, (e.eliminated_stage IS NOT NULL), support_now DESC, e.ideology_id",
        )
        .bind(e.id)
        .fetch_all(pool)
        .await?;
        return Ok(ElectionStatus {
            running: Some(RunningElection {
                target_generation: e.target_generation,
                starts_at: e.starts_at,
                ends_at: e.ends_at,
                stage: e.stage,
                votes_locked: votes_locked(e.starts_at, now),
                next_selection_at: e.starts_at + (e.stage + 1) * WEEK,
                entries,
            }),
            next_election_at: None,
        });
    }
    let gen_end: Option<i64> = sqlx::query_scalar("SELECT ends_at FROM generations WHERE number = ?1")
        .bind(e.target_generation)
        .fetch_optional(pool)
        .await?;
    Ok(ElectionStatus { running: None, next_election_at: gen_end.map(election_start_for) })
}
