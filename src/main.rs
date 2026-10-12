use std::net::SocketAddr;
use std::time::Duration;

use riverthink::config::Config;
use riverthink::service::{auth, election};
use riverthink::web::{self, AppState};

fn usage() -> ! {
    eprintln!(
        "RiverThink\n\nUsage:\n  riverthink              run the server\n  riverthink admin-grant <username>   make an existing account an admin\n\n\
         Environment: RIVERTHINK_DB, RIVERTHINK_BIND, RIVERTHINK_SMIN, RIVERTHINK_DEV_CLOCK=1, RIVERTHINK_COOKIE_SECURE=1, RIVERTHINK_TRUST_PROXY=1"
    );
    std::process::exit(2)
}

// Two worker threads are plenty for a single-writer SQLite app and keep RSS low.
#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let cfg = Config::from_env();
    let args: Vec<String> = std::env::args().skip(1).collect();

    let db = match riverthink::db::connect(&cfg.db_path).await {
        Ok(d) => d,
        Err(e) => {
            eprintln!("cannot open database {}: {e}", cfg.db_path);
            std::process::exit(1);
        }
    };

    match args.first().map(String::as_str) {
        None => {}
        Some("admin-grant") => {
            let Some(name) = args.get(1) else { usage() };
            match auth::grant_admin(&db, name).await {
                Ok(()) => println!("{name} is now an admin"),
                Err(e) => {
                    eprintln!("failed: {e}");
                    std::process::exit(1);
                }
            }
            return;
        }
        Some(_) => usage(),
    }

    let state = AppState::new(db, cfg.clone());

    // Scheduler: advances elections / generations. Idempotent, so a missed
    // minute (or a restart after downtime) simply catches up on the next tick.
    {
        let st = state.clone();
        tokio::spawn(async move {
            loop {
                match election::tick(&st.db, st.clock.now()).await {
                    Ok(r) if r != Default::default() => eprintln!("scheduler: {r:?}"),
                    Ok(_) => {}
                    Err(e) => eprintln!("scheduler error: {e}"),
                }
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        });
    }

    let listener = match tokio::net::TcpListener::bind(&cfg.bind).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("cannot bind {}: {e}", cfg.bind);
            std::process::exit(1);
        }
    };
    eprintln!("RiverThink listening on http://{}  (db: {})", cfg.bind, cfg.db_path);
    if cfg.dev_clock {
        eprintln!("WARNING: RIVERTHINK_DEV_CLOCK is on (demo clock controls enabled for admins)");
    }
    let app = web::router(state).into_make_service_with_connect_info::<SocketAddr>();
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    if let Err(e) = axum::serve(listener, app).with_graceful_shutdown(shutdown).await {
        eprintln!("server error: {e}");
        std::process::exit(1);
    }
}
