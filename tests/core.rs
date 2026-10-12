//! Integration tests for the critical invariants (proposal sections 21, 24, 26).
//! Time is simulated: `now` is passed explicitly, so months pass in milliseconds.

use riverthink::config::Config;
use riverthink::db::{self, Db};
use riverthink::domain::calendar::*;
use riverthink::domain::channel::Channel;
use riverthink::error::AppError;
use riverthink::model::User;
use riverthink::service::election::{self, NineRow};
use riverthink::service::{auth, ideology, moderation, notes, vote};
use tempfile::TempDir;

struct Env {
    pool: Db,
    cfg: Config,
    _dir: TempDir,
}

async fn env(s_min: u32) -> Env {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("t.db");
    let pool = db::connect(path.to_str().unwrap()).await.unwrap();
    Env { pool, cfg: Config { s_min, require_phone: false, ..Config::default() }, _dir: dir }
}

fn t0() -> i64 {
    wib_ts(2026, 10, 10, 18, 0)
}

async fn get_user(pool: &Db, id: i64) -> User {
    sqlx::query_as("SELECT id, username, channel, role, status, created_at, last_active_at FROM users WHERE id = ?1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn mk_user(e: &Env, name: &str, ch: Channel, now: i64) -> User {
    let id = auth::register(&e.pool, &e.cfg, now, name, "password123", ch.code(), None).await.unwrap();
    get_user(&e.pool, id).await
}

async fn mk_ideology(e: &Env, creator: &User, name: &str, now: i64) -> i64 {
    ideology::create(&e.pool, &e.cfg, now, creator, name, "Prinsip dan tujuan ideologi ini cukup panjang.")
        .await
        .unwrap()
}

async fn make_admin(e: &Env, name: &str, now: i64) -> User {
    let u = mk_user(e, name, Channel::E, now).await;
    auth::grant_admin(&e.pool, name).await.unwrap();
    get_user(&e.pool, u.id).await
}

/// 4 ideologies per channel; `supports[channel][k]` fresh voters back ideology k.
async fn seed(e: &Env, now: i64, supports: [[usize; 4]; 3]) -> [[i64; 4]; 3] {
    let mut ids = [[0i64; 4]; 3];
    for (ci, ch) in Channel::ALL.iter().enumerate() {
        let c = ch.code().to_lowercase();
        for k in 0..4 {
            let creator = mk_user(e, &format!("c{c}{k}"), *ch, now).await;
            let id = mk_ideology(e, &creator, &format!("Ideologi {}{k}", ch.code()), now).await;
            ids[ci][k] = id;
            for v in 0..supports[ci][k] {
                let u = mk_user(e, &format!("v{c}{k}x{v}"), *ch, now).await;
                vote::cast(&e.pool, &e.cfg, now, u.id, id).await.unwrap();
            }
        }
    }
    ids
}

fn nine_ids(nine: &[NineRow], ch: Channel) -> Vec<i64> {
    nine.iter().filter(|r| r.channel == ch.code()).map(|r| r.ideology_id).collect()
}

async fn count(pool: &Db, sql: &'static str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
}

// ---- accounts ---------------------------------------------------------------

#[tokio::test]
async fn register_login_and_validation() {
    let e = env(6).await;
    let u = mk_user(&e, "Faiz_01", Channel::E, t0()).await;
    // Usernames are case-insensitive unique.
    let dup = auth::register(&e.pool, &e.cfg, t0(), "faiz_01", "password123", "Z", None).await;
    assert!(matches!(dup, Err(AppError::Conflict(_))));
    assert!(matches!(auth::register(&e.pool, &e.cfg, t0(), "ab", "password123", "E", None).await, Err(AppError::Invalid(_))));
    assert!(matches!(auth::register(&e.pool, &e.cfg, t0(), "okname", "short", "E", None).await, Err(AppError::Invalid(_))));
    assert!(matches!(auth::register(&e.pool, &e.cfg, t0(), "okname", "password123", "Q", None).await, Err(AppError::Invalid(_))));

    assert!(auth::login(&e.pool, &e.cfg, t0(), "faiz_01", "wrongpass1").await.is_err());
    assert!(auth::login(&e.pool, &e.cfg, t0(), "nobody", "password123").await.is_err());
    let token = auth::login(&e.pool, &e.cfg, t0(), "FAIZ_01", "password123").await.unwrap();
    let me = auth::session_user(&e.pool, t0() + 10, &token).await.unwrap().unwrap();
    assert_eq!(me.id, u.id);
    // Expired and logged-out sessions stop working.
    assert!(auth::session_user(&e.pool, t0() + e.cfg.session_ttl_secs + 1, &token).await.unwrap().is_none());
    auth::logout(&e.pool, &token).await.unwrap();
    assert!(auth::session_user(&e.pool, t0() + 10, &token).await.unwrap().is_none());
}

// ---- Rule A capacity ---------------------------------------------------------

#[tokio::test]
async fn capacity_rule_a_is_per_channel_and_limits_founding() {
    let e = env(2).await; // S_min = 2
    let now = t0();
    let e1 = mk_user(&e, "e_one", Channel::E, now).await;
    let e2 = mk_user(&e, "e_two", Channel::E, now).await;
    let e3 = mk_user(&e, "e_three", Channel::E, now).await;
    mk_ideology(&e, &e1, "Alpha Order", now).await;
    mk_ideology(&e, &e2, "Beta Order", now).await;
    // E is full (2/2); the quota is independent of the other channels.
    let r = ideology::create(&e.pool, &e.cfg, now, &e3, "Gamma Order", "Deskripsi yang cukup panjang.").await;
    assert!(matches!(r, Err(AppError::Conflict(ref m)) if m.contains("penuh")), "{r:?}");
    let z1 = mk_user(&e, "z_one", Channel::Z, now).await;
    mk_ideology(&e, &z1, "Zeta Order", now).await;

    let info = ideology::capacity_info_pool(&e.pool, &e.cfg, now, Channel::E).await.unwrap();
    assert_eq!((info.used, info.cap, info.generation), (2, 2, 1));
}

#[tokio::test]
async fn capacity_grows_with_active_users() {
    let e = env(2).await;
    let now = t0();
    // 40 active E users -> floor(0.10 * 40) = 4 slots.
    let mut users = Vec::new();
    for i in 0..40 {
        users.push(mk_user(&e, &format!("eu{i:02}"), Channel::E, now).await);
    }
    let info = ideology::capacity_info_pool(&e.pool, &e.cfg, now, Channel::E).await.unwrap();
    assert_eq!((info.active_users, info.cap), (40, 4));
    for (i, u) in users.iter().take(4).enumerate() {
        mk_ideology(&e, u, &format!("Founder {i}"), now).await;
    }
    let r = ideology::create(&e.pool, &e.cfg, now, &users[4], "Founder 5", "Deskripsi yang cukup panjang.").await;
    assert!(matches!(r, Err(AppError::Conflict(_))));
    // Inactive users stop counting: 31 days later only S_min remains.
    let later = now + 31 * DAY;
    let info = ideology::capacity_info_pool(&e.pool, &e.cfg, later, Channel::E).await.unwrap();
    assert_eq!((info.active_users, info.cap), (0, 2));
}

#[tokio::test]
async fn one_ideology_per_account_per_generation_and_unique_names() {
    let e = env(6).await;
    let now = t0();
    let a = mk_user(&e, "founder_a", Channel::D, now).await;
    let b = mk_user(&e, "founder_b", Channel::D, now).await;
    mk_ideology(&e, &a, "Capitalist Technocracy", now).await;
    let again = ideology::create(&e.pool, &e.cfg, now, &a, "Another One", "Deskripsi yang cukup panjang.").await;
    assert!(matches!(again, Err(AppError::Conflict(_))));
    // Name uniqueness ignores case and extra spaces.
    let dup = ideology::create(&e.pool, &e.cfg, now, &b, "  capitalist   TECHNOCRACY ", "Deskripsi yang cukup panjang.").await;
    assert!(matches!(dup, Err(AppError::Conflict(_))), "{dup:?}");
}

// ---- voting ------------------------------------------------------------------

#[tokio::test]
async fn one_account_one_vote_with_cooldown() {
    let e = env(6).await;
    let now = t0();
    let creator = mk_user(&e, "creator_e", Channel::E, now).await;
    let a = mk_ideology(&e, &creator, "Alpha", now).await;
    let c2 = mk_user(&e, "creator_e2", Channel::E, now).await;
    let b = mk_ideology(&e, &c2, "Beta", now).await;
    let z = mk_user(&e, "creator_z", Channel::Z, now).await;
    let other = mk_ideology(&e, &z, "Other Channel", now).await;

    let voter = mk_user(&e, "voter_e", Channel::E, now).await;
    assert_eq!(vote::cast(&e.pool, &e.cfg, now, voter.id, a).await.unwrap(), vote::VoteOutcome::Cast);
    assert!(matches!(vote::cast(&e.pool, &e.cfg, now, voter.id, a).await, Err(AppError::Conflict(_))));
    // Moving within the cooldown is rejected...
    let early = now + 7 * DAY - 1;
    assert!(matches!(vote::cast(&e.pool, &e.cfg, early, voter.id, b).await, Err(AppError::Conflict(_))));
    // ...and allowed once it expires.
    let ok = now + 7 * DAY;
    assert_eq!(vote::cast(&e.pool, &e.cfg, ok, voter.id, b).await.unwrap(), vote::VoteOutcome::Moved);
    // Cross-channel votes are refused.
    let r = vote::cast(&e.pool, &e.cfg, ok + 8 * DAY, voter.id, other).await;
    assert!(matches!(r, Err(AppError::Forbidden(_))));

    assert_eq!(count(&e.pool, "SELECT COUNT(*) FROM votes").await, 1);
    assert_eq!(count(&e.pool, "SELECT COUNT(*) FROM vote_log").await, 2);
    let rows = ideology::list_by_channel(&e.pool, &e.cfg, ok, Channel::E).await.unwrap();
    assert_eq!(rows.iter().map(|r| r.support).sum::<i64>(), 1);
    assert_eq!(rows.iter().find(|r| r.id == b).unwrap().support, 1);
}

#[tokio::test]
async fn votes_are_frozen_in_final_week_of_election() {
    let e = env(6).await;
    let now = t0();
    let creator = mk_user(&e, "creator_e", Channel::E, now).await;
    let a = mk_ideology(&e, &creator, "Alpha", now).await;
    election::tick(&e.pool, now).await.unwrap(); // genesis election starts at `now`
    let v1 = mk_user(&e, "voter_one", Channel::E, now).await;
    let v2 = mk_user(&e, "voter_two", Channel::E, now).await;

    let just_before = now + 3 * WEEK - 1;
    vote::cast(&e.pool, &e.cfg, just_before, v1.id, a).await.unwrap();
    let locked = now + 3 * WEEK;
    // First-time votes and moves are both rejected in week 4.
    assert!(matches!(vote::cast(&e.pool, &e.cfg, locked, v2.id, a).await, Err(AppError::Conflict(ref m)) if m.contains("dikunci")));
    // Even after ends_at, until the scheduler finalizes the election.
    assert!(matches!(vote::cast(&e.pool, &e.cfg, now + ELECTION_LEN + 5, v2.id, a).await, Err(AppError::Conflict(_))));
    // Once finalized, voting reopens.
    election::tick(&e.pool, now + ELECTION_LEN).await.unwrap();
    vote::cast(&e.pool, &e.cfg, now + ELECTION_LEN + 5, v2.id, a).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_votes_never_create_invalid_state() {
    let e = env(6).await;
    let now = t0();
    let mut ids = Vec::new();
    for i in 0..3 {
        let c = mk_user(&e, &format!("founder{i}"), Channel::E, now).await;
        ids.push(mk_ideology(&e, &c, &format!("Ideologi {i}"), now).await);
    }
    let mut voters = Vec::new();
    for i in 0..30 {
        voters.push(mk_user(&e, &format!("voter{i:02}"), Channel::E, now).await);
    }

    // Every voter fires 6 simultaneous requests at different ideologies.
    let mut handles = Vec::new();
    for (vi, u) in voters.iter().enumerate() {
        for k in 0..6 {
            let (pool, cfg, uid, target) = (e.pool.clone(), e.cfg.clone(), u.id, ids[(vi + k) % 3]);
            handles.push(tokio::spawn(async move { vote::cast(&pool, &cfg, now, uid, target).await }));
        }
    }
    let (mut ok, mut conflicts) = (0, 0);
    for h in handles {
        match h.await.unwrap() {
            Ok(_) => ok += 1,
            Err(AppError::Conflict(_)) => conflicts += 1,
            Err(other) => panic!("unexpected error under contention: {other:?}"),
        }
    }
    // Exactly one request per voter wins; the rest are clean rejections.
    assert_eq!(ok, 30);
    assert_eq!(conflicts, 30 * 5);
    assert_eq!(count(&e.pool, "SELECT COUNT(*) FROM votes").await, 30);
    assert_eq!(count(&e.pool, "SELECT COUNT(DISTINCT user_id) FROM votes").await, 30);
    assert_eq!(count(&e.pool, "SELECT COUNT(*) FROM vote_log").await, 30);
    let rows = ideology::list_by_channel(&e.pool, &e.cfg, now, Channel::E).await.unwrap();
    assert_eq!(rows.iter().map(|r| r.support).sum::<i64>(), 30);
}

// ---- elections and generations ---------------------------------------------

#[tokio::test]
async fn genesis_election_runs_four_weekly_selections_and_seats_three_per_channel() {
    let e = env(6).await;
    let now = t0();
    // Supports per channel: ideology 3 has the least and must miss the Final Nine.
    let ids = seed(&e, now, [[5, 4, 3, 1], [2, 6, 4, 1], [3, 3, 7, 2]]).await;

    let r = election::tick(&e.pool, now).await.unwrap();
    assert_eq!(r.elections_started, 1);
    assert!(election::current_nine(&e.pool).await.unwrap().is_empty()); // launch only
    // Nothing is due until week 1 ends.
    assert_eq!(election::tick(&e.pool, now + WEEK - 1).await.unwrap(), Default::default());

    for week in 1..=3 {
        let r = election::tick(&e.pool, now + week * WEEK).await.unwrap();
        assert_eq!((r.selections_run, r.generations_finalized), (1, 0), "week {week}");
    }
    let st = election::status(&e.pool, now + 3 * WEEK).await.unwrap();
    let run = st.running.unwrap();
    assert_eq!((run.stage, run.votes_locked, run.entries.len()), (3, true, 12));

    let r = election::tick(&e.pool, now + 4 * WEEK).await.unwrap();
    assert_eq!((r.selections_run, r.generations_finalized), (1, 1));
    // Idempotent: nothing more to do.
    assert_eq!(election::tick(&e.pool, now + 4 * WEEK).await.unwrap(), Default::default());

    let nine = election::current_nine(&e.pool).await.unwrap();
    assert_eq!(nine.len(), 9);
    assert_eq!(nine_ids(&nine, Channel::E), vec![ids[0][0], ids[0][1], ids[0][2]]);
    assert_eq!(nine_ids(&nine, Channel::Z), vec![ids[1][1], ids[1][2], ids[1][0]]);
    assert_eq!(nine_ids(&nine, Channel::D), vec![ids[2][2], ids[2][0], ids[2][1]]);
    assert!(nine.iter().all(|r| r.generation == 1));

    let g = election::current_generation(&e.pool).await.unwrap().unwrap();
    assert_eq!(g.number, 1);
    assert_eq!(g.starts_at, now + ELECTION_LEN);
    assert_eq!(g.ends_at, generation_end(now + ELECTION_LEN));
    // 12 entries x 4 selections' worth of audit rows exist for the first selection.
    assert_eq!(count(&e.pool, "SELECT COUNT(*) FROM election_stage_results WHERE stage = 1").await, 12);
    // Next election starts four weeks before generation 1 ends.
    let st = election::status(&e.pool, now + 4 * WEEK).await.unwrap();
    assert_eq!(st.next_election_at, Some(election_start_for(g.ends_at)));
}

#[tokio::test]
async fn incumbent_survives_or_is_replaced_across_generations_without_gaps() {
    let e = env(6).await;
    let now = t0();
    let ids = seed(&e, now, [[5, 4, 3, 1], [2, 6, 4, 1], [3, 3, 7, 2]]).await;
    election::tick(&e.pool, now).await.unwrap();
    election::tick(&e.pool, now + 4 * WEEK).await.unwrap(); // catches up all four selections
    let gen1 = election::current_nine(&e.pool).await.unwrap();
    assert_eq!(gen1.len(), 9);
    let g1 = election::current_generation(&e.pool).await.unwrap().unwrap();

    // A newcomer is founded in generation 1, after the genesis election.
    let late = g1.starts_at + 10 * DAY;
    let newcomer_user = mk_user(&e, "newcomer_e", Channel::E, late).await;
    let newcomer = mk_ideology(&e, &newcomer_user, "Newcomer Order", late).await;
    assert_eq!(count(&e.pool, "SELECT founded_generation FROM ideologies WHERE name = 'Newcomer Order'").await, 1);

    // Supporters of E's third seat (ids[0][2]) and the 4th-place ideology move to the newcomer
    // once their cooldown has passed.
    let move_at = g1.starts_at + 60 * DAY;
    let backers: Vec<i64> = sqlx::query_scalar("SELECT user_id FROM votes WHERE ideology_id IN (?1, ?2)")
        .bind(ids[0][2])
        .bind(ids[0][3])
        .fetch_all(&e.pool)
        .await
        .unwrap();
    assert_eq!(backers.len(), 4);
    for uid in backers {
        vote::cast(&e.pool, &e.cfg, move_at, uid, newcomer).await.unwrap();
    }

    // Walk the calendar day by day through the whole generation: the Final Nine
    // must never be empty, and the generation number must never go backwards.
    let mut last_gen = 1;
    let mut day = g1.starts_at;
    while day <= g1.ends_at + 2 * DAY {
        election::tick(&e.pool, day).await.unwrap();
        let nine = election::current_nine(&e.pool).await.unwrap();
        assert_eq!(nine.len(), 9, "empty/partial Final Nine at {}", format_wib(day));
        assert!(nine[0].generation >= last_gen);
        last_gen = nine[0].generation;
        day += DAY;
    }
    assert_eq!(last_gen, 2);

    let gen2 = election::current_nine(&e.pool).await.unwrap();
    // Z and D are unchanged: those ideologies survived into generation 2.
    assert_eq!(nine_ids(&gen2, Channel::Z), nine_ids(&gen1, Channel::Z));
    assert_eq!(nine_ids(&gen2, Channel::D), nine_ids(&gen1, Channel::D));
    // E: newcomer (4 supporters) displaced the former third seat; no automatic incumbent immunity.
    let e2 = nine_ids(&gen2, Channel::E);
    assert!(e2.contains(&newcomer));
    assert!(!e2.contains(&ids[0][2]));
    assert!(e2.contains(&ids[0][0]) && e2.contains(&ids[0][1]));
    assert_eq!(e2.len(), 3);

    // History is preserved and unchanged.
    let hist = election::history(&e.pool).await.unwrap();
    assert_eq!(hist.len(), 2);
    assert_eq!(hist[1].rows.iter().map(|r| r.ideology_id).collect::<Vec<_>>(),
               gen1.iter().map(|r| r.ideology_id).collect::<Vec<_>>());
    // The displaced ideology still exists with its record.
    let kept = ideology::get(&e.pool, &e.cfg, day, ids[0][2]).await.unwrap().unwrap();
    assert_eq!((kept.in_nine_now, kept.last_nine_gen), (0, Some(1)));
    // Generation 2 started exactly when generation 1 ended.
    let g2 = election::current_generation(&e.pool).await.unwrap().unwrap();
    assert_eq!((g2.number, g2.starts_at), (2, g1.ends_at));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_ticks_finalize_exactly_once() {
    let e = env(6).await;
    let now = t0();
    seed(&e, now, [[3, 2, 1, 0], [3, 2, 1, 0], [3, 2, 1, 0]]).await;
    election::tick(&e.pool, now).await.unwrap();
    let end = now + ELECTION_LEN;
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let pool = e.pool.clone();
            tokio::spawn(async move { election::tick(&pool, end).await })
        })
        .collect();
    let mut selections = 0;
    let mut finalized = 0;
    for h in handles {
        let r = h.await.unwrap().unwrap();
        selections += r.selections_run;
        finalized += r.generations_finalized;
    }
    assert_eq!((selections, finalized), (4, 1));
    assert_eq!(count(&e.pool, "SELECT COUNT(*) FROM generations").await, 1);
    assert_eq!(count(&e.pool, "SELECT COUNT(*) FROM final_nine").await, 9);
    assert_eq!(count(&e.pool, "SELECT COUNT(*) FROM election_stage_results WHERE stage = 4").await, 3 * 4);
}

#[tokio::test]
async fn zero_support_never_takes_a_seat_and_vacancies_are_allowed() {
    let e = env(6).await;
    let now = t0();
    seed(&e, now, [[2, 0, 0, 0], [1, 1, 0, 0], [0, 0, 0, 0]]).await;
    election::tick(&e.pool, now).await.unwrap();
    election::tick(&e.pool, now + ELECTION_LEN).await.unwrap();
    let nine = election::current_nine(&e.pool).await.unwrap();
    assert_eq!(nine_ids(&nine, Channel::E).len(), 1);
    assert_eq!(nine_ids(&nine, Channel::Z).len(), 2);
    assert_eq!(nine_ids(&nine, Channel::D).len(), 0);
    // A generation still starts even when a channel has no winner.
    assert_eq!(election::current_generation(&e.pool).await.unwrap().unwrap().number, 1);
}

// ---- database constraints ----------------------------------------------------

#[tokio::test]
async fn database_rejects_invalid_final_nine_and_immutable_fields() {
    let e = env(6).await;
    let now = t0();
    let ids = seed(&e, now, [[3, 2, 1, 1], [3, 2, 1, 1], [3, 2, 1, 1]]).await;
    election::tick(&e.pool, now).await.unwrap();
    election::tick(&e.pool, now + ELECTION_LEN).await.unwrap();

    // A 4th slot in a channel is structurally impossible.
    let r = sqlx::query("INSERT INTO final_nine (generation, channel, slot, ideology_id, support) VALUES (1, 'E', 4, ?1, 0)")
        .bind(ids[0][3])
        .execute(&e.pool)
        .await;
    assert!(r.is_err());
    // An ideology cannot sit in another channel's seat.
    let r = sqlx::query("INSERT INTO final_nine (generation, channel, slot, ideology_id, support) VALUES (1, 'Z', 3, ?1, 0)")
        .bind(ids[0][3])
        .execute(&e.pool)
        .await;
    assert!(r.is_err());
    // History is append-only.
    assert!(sqlx::query("UPDATE final_nine SET support = 99").execute(&e.pool).await.is_err());
    assert!(sqlx::query("DELETE FROM final_nine").execute(&e.pool).await.is_err());
    // Home channel and founding generation are immutable.
    assert!(sqlx::query("UPDATE users SET channel = 'Z' WHERE channel = 'E'").execute(&e.pool).await.is_err());
    assert!(sqlx::query("UPDATE ideologies SET founded_generation = 9").execute(&e.pool).await.is_err());
    // A vote row must match the voter's channel.
    let zuser: i64 = sqlx::query_scalar("SELECT id FROM users WHERE channel = 'Z' LIMIT 1").fetch_one(&e.pool).await.unwrap();
    let r = sqlx::query("INSERT OR REPLACE INTO votes (user_id, ideology_id, channel, cast_at) VALUES (?1, ?2, 'E', 0)")
        .bind(zuser)
        .bind(ids[0][0])
        .execute(&e.pool)
        .await;
    assert!(r.is_err());
    assert_eq!(count(&e.pool, "SELECT COUNT(*) FROM final_nine").await, 9);
}

// ---- Ambinote and moderation --------------------------------------------------

#[tokio::test]
async fn ambinote_tags_filtering_and_rate_limit() {
    let e = env(6).await;
    let now = t0();
    let u = mk_user(&e, "writer", Channel::Z, now).await;
    let id1 = notes::post(&e.pool, now, &u, "Ide #AgriTech", "Sensor tanah murah untuk petani #robotics", None).await.unwrap();
    let too_fast = notes::post(&e.pool, now + 5, &u, "Kedua", "isi", None).await;
    assert!(matches!(too_fast, Err(AppError::Conflict(_))));
    let id2 = notes::post(&e.pool, now + 20, &u, "Kedua #cyber", "isi", None).await.unwrap();

    let all = notes::list(&e.pool, None, None, None, 20).await.unwrap();
    assert_eq!(all.iter().map(|n| n.id).collect::<Vec<_>>(), vec![id2, id1]);
    let agri = notes::list(&e.pool, Some("agritech"), None, None, 20).await.unwrap();
    assert_eq!(agri.len(), 1);
    assert_eq!(agri[0].tag_list().collect::<Vec<_>>().len(), 2);
    // Pagination by id.
    let page2 = notes::list(&e.pool, None, None, Some(id2), 20).await.unwrap();
    assert_eq!(page2.len(), 1);

    // The author's supported ideology shows up as the small label.
    let c = mk_user(&e, "creator_z", Channel::Z, now).await;
    let ide = mk_ideology(&e, &c, "Capitalist Technocracy", now).await;
    vote::cast(&e.pool, &e.cfg, now, u.id, ide).await.unwrap();
    let all = notes::list(&e.pool, None, None, None, 20).await.unwrap();
    assert_eq!(all[0].ideology.as_deref(), Some("Capitalist Technocracy"));
    assert_eq!(all[0].channel, "Z");
}

#[tokio::test]
async fn moderation_flow_report_hide_restrict_appeal_audit() {
    let e = env(6).await;
    let now = t0();
    let admin = make_admin(&e, "boss", now).await;
    let author = mk_user(&e, "author", Channel::E, now).await;
    let reporter = mk_user(&e, "reporter", Channel::D, now).await;
    let note = notes::post(&e.pool, now, &author, "Spam", "beli sekarang", None).await.unwrap();

    moderation::report(&e.pool, now, &reporter, "note", note, "spam").await.unwrap();
    let dup = moderation::report(&e.pool, now, &reporter, "note", note, "spam").await;
    assert!(matches!(dup, Err(AppError::Conflict(_))));
    assert_eq!(moderation::admin_overview(&e.pool).await.unwrap().reports.len(), 1);

    // Only admins moderate; popularity or Final Nine status gives no such power.
    let denied = moderation::act(&e.pool, now, &reporter, "note", note, "hide", "").await;
    assert!(matches!(denied, Err(AppError::Forbidden(_))));
    moderation::act(&e.pool, now, &admin, "note", note, "hide", "spam jelas").await.unwrap();
    assert!(notes::list(&e.pool, None, None, None, 20).await.unwrap().is_empty());
    assert!(moderation::admin_overview(&e.pool).await.unwrap().reports.is_empty()); // actioned

    // Appeal -> granted -> note visible again.
    moderation::appeal(&e.pool, now + 60, &author, "note", note, "ini bukan spam").await.unwrap();
    let again = moderation::appeal(&e.pool, now + 61, &author, "note", note, "tolong").await;
    assert!(matches!(again, Err(AppError::Conflict(_))));
    let ov = moderation::admin_overview(&e.pool).await.unwrap();
    moderation::resolve_appeal(&e.pool, now + 120, &admin, ov.appeals[0].id, true, "oke").await.unwrap();
    assert_eq!(notes::list(&e.pool, None, None, None, 20).await.unwrap().len(), 1);

    // Restricting an account: cannot post/vote/create; its vote stops counting; admins are immune.
    let creator = mk_user(&e, "creator_e", Channel::E, now).await;
    let ide = mk_ideology(&e, &creator, "Alpha", now).await;
    vote::cast(&e.pool, &e.cfg, now, author.id, ide).await.unwrap();
    assert_eq!(ideology::get(&e.pool, &e.cfg, now, ide).await.unwrap().unwrap().support, 1);
    moderation::act(&e.pool, now + 200, &admin, "user", author.id, "restrict", "harassment").await.unwrap();
    assert_eq!(ideology::get(&e.pool, &e.cfg, now, ide).await.unwrap().unwrap().support, 0);
    let a = get_user(&e.pool, author.id).await;
    assert!(matches!(notes::post(&e.pool, now + 900, &a, "x", "y", None).await, Err(AppError::Forbidden(_))));
    assert!(matches!(vote::cast(&e.pool, &e.cfg, now + 9 * DAY, a.id, ide).await, Err(AppError::Forbidden(_))));
    let self_restrict = moderation::act(&e.pool, now, &admin, "user", admin.id, "restrict", "").await;
    assert!(matches!(self_restrict, Err(AppError::Forbidden(_))));

    // A restricted ideology cannot receive votes and is dropped from selection.
    moderation::act(&e.pool, now + 300, &admin, "ideology", ide, "restrict", "").await.unwrap();
    let fresh = mk_user(&e, "fresh_voter", Channel::E, now).await;
    assert!(matches!(vote::cast(&e.pool, &e.cfg, now, fresh.id, ide).await, Err(AppError::Conflict(_))));

    let audit = moderation::admin_overview(&e.pool).await.unwrap().audit;
    let actions: Vec<_> = audit.iter().map(|a| a.action.as_str()).collect();
    for want in ["note.hide", "appeal.granted", "user.restrict", "ideology.restrict"] {
        assert!(actions.contains(&want), "missing audit action {want}: {actions:?}");
    }
}

#[tokio::test]
async fn channel_rooms_are_open_to_everyone_while_votes_stay_locked_to_home_channel() {
    let e = env(6).await;
    let now = t0();
    let ue = mk_user(&e, "room_e", Channel::E, now).await;
    let uz = mk_user(&e, "room_z", Channel::Z, now).await;

    // Home channel does not restrict where one may write or read.
    let a = notes::post(&e.pool, now, &ue, "Halo Z", "", Some(Channel::Z)).await.unwrap();
    let b = notes::post(&e.pool, now + 20, &ue, "Halo D", "", Some(Channel::D)).await.unwrap();
    let c = notes::post(&e.pool, now, &uz, "Halo E", "", Some(Channel::E)).await.unwrap();
    let g = notes::post(&e.pool, now + 40, &ue, "Umum #robotics", "", None).await.unwrap();

    let ids = |v: Vec<notes::NoteView>| v.into_iter().map(|n| n.id).collect::<Vec<_>>();
    assert_eq!(ids(notes::list(&e.pool, None, Some(Channel::Z), None, 20).await.unwrap()), vec![a]);
    assert_eq!(ids(notes::list(&e.pool, None, Some(Channel::D), None, 20).await.unwrap()), vec![b]);
    assert_eq!(ids(notes::list(&e.pool, None, Some(Channel::E), None, 20).await.unwrap()), vec![c]);
    // The general Ambinote stream shows everything, newest first.
    assert_eq!(ids(notes::list(&e.pool, None, None, None, 20).await.unwrap()), vec![g, c, b, a]);
    // Room and tag filters combine.
    assert_eq!(ids(notes::list(&e.pool, Some("robotics"), None, None, 20).await.unwrap()), vec![g]);
    assert!(notes::list(&e.pool, Some("robotics"), Some(Channel::Z), None, 20).await.unwrap().is_empty());

    // Bubble colour still follows the author's home channel, not the room.
    let in_z = notes::list(&e.pool, None, Some(Channel::Z), None, 20).await.unwrap();
    assert_eq!((in_z[0].channel.as_str(), in_z[0].room.as_deref()), ("E", Some("Z")));

    // ...but supporting an ideology remains locked to the home channel.
    let founder = mk_user(&e, "founder_z", Channel::Z, now).await;
    let zide = mk_ideology(&e, &founder, "Zed Order", now).await;
    let r = vote::cast(&e.pool, &e.cfg, now, ue.id, zide).await;
    assert!(matches!(r, Err(AppError::Forbidden(_))), "{r:?}");
}

#[tokio::test]
async fn phone_limit_two_accounts_and_deleting_frees_the_slot() {
    let e = env(6).await;
    let cfg = Config { require_phone: true, ..e.cfg.clone() };
    let now = t0();
    let reg = |name: &'static str, phone: &'static str| {
        let (pool, cfg) = (e.pool.clone(), cfg.clone());
        async move { auth::register(&pool, &cfg, now, name, "password123", "E", Some(phone)).await }
    };
    // Same number in different notations normalises to one key.
    let a = reg("phone_a", "0812-3456-7890").await.unwrap();
    let second = reg("phone_b", "+62 812 3456 7890").await;
    assert!(matches!(second, Err(AppError::Conflict(_))), "{second:?}");
    assert!(matches!(reg("phone_d", "").await, Err(AppError::Invalid(_))));
    assert!(matches!(reg("phone_d", "abc").await, Err(AppError::Invalid(_))));
    reg("phone_e", "081299990000").await.unwrap(); // another number is unaffected

    // Wrong password cannot delete; right password frees the slot, withdraws the vote.
    let founder = mk_user(&e, "founder_e", Channel::E, now).await;
    let ide = mk_ideology(&e, &founder, "Phone Order", now).await;
    vote::cast(&e.pool, &e.cfg, now, a, ide).await.unwrap();
    assert!(auth::delete_account(&e.pool, now, a, "salahsalah").await.is_err());
    auth::delete_account(&e.pool, now, a, "password123").await.unwrap();
    assert_eq!(ideology::get(&e.pool, &e.cfg, now, ide).await.unwrap().unwrap().support, 0);
    assert!(auth::login(&e.pool, &e.cfg, now, "phone_a", "password123").await.is_err());
    reg("phone_b", "6281234567890").await.unwrap(); // slot is free again
}
