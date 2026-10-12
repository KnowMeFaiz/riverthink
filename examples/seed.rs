//! Demo seeder: fills a SEPARATE database with simulated accounts, ideologies and votes.
//!   RIVERTHINK_DB=demo.db cargo run --release --example seed
//! Never point it at the real database. All demo passwords are `demo12345`.
use riverthink::config::Config;
use riverthink::service::{auth, ideology, vote};
use riverthink::{db, model::User};

async fn user(pool: &db::Db, cfg: &Config, now: i64, name: &str, ch: &str) -> User {
    auth::register(pool, cfg, now, name, "demo12345", ch, None).await.unwrap();
    let t = auth::login(pool, cfg, now, name, "demo12345").await.unwrap();
    auth::session_user(pool, now, &t).await.unwrap().unwrap()
}

#[tokio::main]
async fn main() {
    let path = std::env::var("RIVERTHINK_DB").unwrap_or_else(|_| "demo.db".into());
    if std::path::Path::new(&path).exists() {
        eprintln!("{path} already exists; refusing to touch it. Delete it or pick another RIVERTHINK_DB.");
        std::process::exit(1);
    }
    let pool = db::connect(&path).await.unwrap();
    let cfg = Config { require_phone: false, ..Config::default() };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    let names = [
        ("E", ["Technocracy Plus", "Open Commons", "Meritocracy Lab"]),
        ("Z", ["Monarki Digital", "Federasi Bebas", "Neo Populis"]),
        ("D", ["Dinosaurus", "Green Circuit", "Mutual Aid Net"]),
    ];
    for (ch, ides) in names {
        let mut founders = vec![];
        for (i, n) in ides.iter().enumerate() {
            let u = user(&pool, &cfg, now, &format!("sim_{}_f{}", ch.to_lowercase(), i + 1), ch).await;
            let id = ideology::create(&pool, &cfg, now, &u, n, "Ideologi simulasi untuk demo siklus election.").await.unwrap();
            founders.push(id);
        }
        // 9 voters per channel, skewed 5/3/1 so the ranking is deterministic.
        for v in 0..9 {
            let u = user(&pool, &cfg, now, &format!("sim_{}_v{}", ch.to_lowercase(), v + 1), ch).await;
            let pick = if v < 5 { 0 } else if v < 8 { 1 } else { 2 };
            vote::cast(&pool, &cfg, now, u.id, founders[pick]).await.unwrap();
        }
    }
    println!("seeded {path}: 36 sim accounts, 9 ideologies, 27 votes");
}
