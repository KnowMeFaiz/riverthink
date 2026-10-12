//! Reproducible benchmark. Run with:
//!     cargo run --release --example bench -- [users] [ideologies_per_channel] [notes]
//! Defaults: 3000 users, 100 ideologies per channel, 5000 notes.
//! Everything runs in one process against a fresh temporary SQLite file, over real
//! loopback HTTP. The report prints hardware and configuration so results are comparable.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use riverthink::config::Config;
use riverthink::db;
use riverthink::domain::calendar::{ELECTION_LEN, wib_ts};
use riverthink::service::{election, ideology, vote};
use riverthink::web::{self, AppState};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

fn arg(i: usize, d: usize) -> usize {
    std::env::args().nth(i).and_then(|v| v.parse().ok()).unwrap_or(d)
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    v[((v.len() as f64 - 1.0) * p).round() as usize]
}

fn rss_mb() -> f64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("VmRSS")).map(|l| l.to_string()))
        .and_then(|l| l.split_whitespace().nth(1).and_then(|n| n.parse::<f64>().ok()))
        .map(|kb| kb / 1024.0)
        .unwrap_or(0.0)
}

fn cpu_model() -> String {
    std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("model name")).and_then(|l| l.split(':').nth(1)).map(|v| v.trim().to_string()))
        .unwrap_or_else(|| "unknown".into())
}

async fn http_get(stream: &mut TcpStream, path: &str, buf: &mut Vec<u8>) -> std::io::Result<usize> {
    stream.write_all(format!("GET {path} HTTP/1.1\r\nHost: bench\r\n\r\n").as_bytes()).await?;
    buf.clear();
    let mut tmp = [0u8; 16 * 1024];
    let (mut head_end, mut need) = (None, 0usize);
    loop {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        buf.extend_from_slice(&tmp[..n]);
        if head_end.is_none() {
            if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&buf[..p]).to_ascii_lowercase();
                let len = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:").and_then(|v| v.trim().parse::<usize>().ok()))
                    .unwrap_or(0);
                head_end = Some(p + 4);
                need = p + 4 + len;
            }
        }
        if head_end.is_some() && buf.len() >= need {
            return Ok(buf.len());
        }
    }
}

async fn load(addr: SocketAddr, path: &'static str, conns: usize, secs: u64) -> (f64, f64, f64, f64, usize) {
    let stop = Instant::now() + Duration::from_secs(secs);
    let mut hs = Vec::new();
    for _ in 0..conns {
        hs.push(tokio::spawn(async move {
            let mut s = TcpStream::connect(addr).await.unwrap();
            let (mut lat, mut buf, mut bytes) = (Vec::new(), Vec::new(), 0usize);
            while Instant::now() < stop {
                let t = Instant::now();
                match http_get(&mut s, path, &mut buf).await {
                    Ok(n) => {
                        bytes = n;
                        lat.push(t.elapsed().as_secs_f64() * 1000.0);
                    }
                    Err(_) => break,
                }
            }
            (lat, bytes)
        }));
    }
    let mut all = Vec::new();
    let mut bytes = 0;
    for h in hs {
        let (l, b) = h.await.unwrap();
        all.extend(l);
        bytes = bytes.max(b);
    }
    let rps = all.len() as f64 / secs as f64;
    (rps, pct(&mut all, 0.50), pct(&mut all, 0.95), pct(&mut all, 0.99), bytes)
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let users = arg(1, 3000);
    let per_channel = arg(2, 100);
    let notes = arg(3, 5000);
    let dir = std::env::temp_dir().join(format!("riverthink-bench-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("bench.db");
    let pool = db::connect(path.to_str().unwrap()).await.unwrap();
    let cfg = Config::default();
    let base = wib_ts(2026, 10, 10, 12, 0);
    let rss0 = rss_mb();

    println!("# RiverThink benchmark");
    println!("cpu: {} | logical cores: {} | os: {}", cpu_model(), std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0), std::env::consts::OS);
    println!("server config: 2 tokio workers, SQLite WAL, pool=4, release build (lto=fat)");
    println!("dataset: {users} users, {} ideologies, {notes} notes\n", per_channel * 3);

    // ---- seed (direct SQL; real registration is dominated by argon2 on purpose) ----
    let t = Instant::now();
    let mut tx = db::immediate(&pool).await.unwrap();
    for i in 0..users {
        let ch = ["E", "Z", "D"][i % 3];
        sqlx::query("INSERT INTO users (username, password_hash, channel, created_at, last_active_at) VALUES (?1,'x',?2,?3,?3)")
            .bind(format!("user{i}"))
            .bind(ch)
            .bind(base)
            .execute(&mut *tx)
            .await
            .unwrap();
    }
    for c in 0..3usize {
        let ch = ["E", "Z", "D"][c];
        for k in 0..per_channel {
            // creator = a distinct user of that channel
            let creator = (k * 3 + c + 1) as i64;
            sqlx::query("INSERT INTO ideologies (name, creator_id, description, channel, founded_generation, created_at) VALUES (?1,?2,'deskripsi benchmark panjang',?3,1,?4)")
                .bind(format!("Ideologi {ch}{k}"))
                .bind(creator)
                .bind(ch)
                .bind(base)
                .execute(&mut *tx)
                .await
                .unwrap();
        }
    }
    for n in 0..notes {
        let author = (n % users) as i64 + 1;
        let id: i64 = sqlx::query_scalar("INSERT INTO notes (author_id, title, body, created_at) VALUES (?1, ?2, 'isi catatan benchmark #bench', ?3) RETURNING id")
            .bind(author)
            .bind(format!("Catatan {n} #agritech"))
            .bind(base + n as i64)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        sqlx::query("INSERT INTO note_tags (note_id, tag) VALUES (?1,'bench'),(?1,'agritech')").bind(id).execute(&mut *tx).await.unwrap();
    }
    tx.commit().await.unwrap();
    println!("seed: {:.2}s", t.elapsed().as_secs_f64());

    // ---- concurrent voting ----
    // Every user casts one vote; 64 tasks in flight. Ideologies are picked in the user's channel.
    let ideas: Arc<Vec<Vec<i64>>> = Arc::new({
        let mut v = vec![Vec::new(); 3];
        for (id, ch) in sqlx::query_as::<_, (i64, String)>("SELECT id, channel FROM ideologies ORDER BY id").fetch_all(&pool).await.unwrap() {
            v[["E", "Z", "D"].iter().position(|c| *c == ch).unwrap()].push(id);
        }
        v
    });
    let t = Instant::now();
    let sem = Arc::new(tokio::sync::Semaphore::new(64));
    let mut hs = Vec::new();
    for i in 0..users {
        let (pool, cfg, ideas, sem) = (pool.clone(), cfg.clone(), ideas.clone(), sem.clone());
        hs.push(tokio::spawn(async move {
            let _p = sem.acquire().await.unwrap();
            let list = &ideas[i % 3];
            let target = list[(i / 3 * 7 + i) % list.len()];
            let t = Instant::now();
            let r = vote::cast(&pool, &cfg, base, i as i64 + 1, target).await;
            (r.is_ok(), t.elapsed().as_secs_f64() * 1000.0)
        }));
    }
    let (mut ok, mut lat) = (0, Vec::new());
    for h in hs {
        let (o, l) = h.await.unwrap();
        ok += o as usize;
        lat.push(l);
    }
    let el = t.elapsed().as_secs_f64();
    println!("concurrent voting: {ok}/{users} ok in {el:.2}s = {:.0} votes/s | latency p50 {:.1} ms, p99 {:.1} ms (64 in flight)", ok as f64 / el, pct(&mut lat, 0.5), pct(&mut lat, 0.99));
    let dup: i64 = sqlx::query_scalar("SELECT COUNT(*) - COUNT(DISTINCT user_id) FROM votes").fetch_one(&pool).await.unwrap();
    println!("integrity: duplicate vote rows = {dup}");

    // ---- score calculation ----
    let t = Instant::now();
    let reps = 50;
    for _ in 0..reps {
        for ch in riverthink::domain::channel::Channel::ALL {
            ideology::list_by_channel(&pool, &cfg, base, ch).await.unwrap();
        }
    }
    println!("ideology support/activity calc: {:.2} ms per channel list ({per_channel} ideologies)", t.elapsed().as_secs_f64() * 1000.0 / (reps as f64 * 3.0));

    // ---- generation transition ----
    election::tick(&pool, base).await.unwrap();
    let t = Instant::now();
    let r = election::tick(&pool, base + ELECTION_LEN).await.unwrap();
    println!("genesis election finalize (4 selections, {} ideologies): {:.1} ms | {r:?}", per_channel * 3, t.elapsed().as_secs_f64() * 1000.0);
    let nine: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM final_nine").fetch_one(&pool).await.unwrap();
    println!("integrity: final_nine rows = {nine}");

    // ---- HTTP ----
    let mut state = AppState::new(pool.clone(), cfg.clone());
    // Pretend "now" is just after the seed so notes are fresh.
    state.clock.advance(0);
    let _ = &mut state;
    let app = web::router(state).into_make_service_with_connect_info::<SocketAddr>();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    println!("\nHTTP over loopback, keep-alive, 5 s per scenario (client shares the same CPU):");
    for (path, conns) in [("/", 32usize), ("/ideologies", 32), ("/final-nine", 32), ("/tag/bench", 32)] {
        let (rps, p50, p95, p99, bytes) = load(addr, path, conns, 5).await;
        println!("  GET {path:<12} {conns} conns: {rps:>8.0} req/s | p50 {p50:.2} ms p95 {p95:.2} ms p99 {p99:.2} ms | {:.1} KB/page", bytes as f64 / 1024.0);
    }

    println!("\nmemory: RSS {:.1} MB at start, {:.1} MB at end (includes benchmark client)", rss0, rss_mb());
    let _ = std::fs::remove_dir_all(&dir);
}
