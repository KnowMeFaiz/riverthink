use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Application clock: system time plus an optional offset that exists only so a
/// demo (`RIVERTHINK_DEV_CLOCK=1`) can fast-forward through weekly selections
/// and generation changes. The offset lives in memory and resets on restart.
#[derive(Default)]
pub struct Clock {
    offset: AtomicI64,
}

impl Clock {
    pub fn new() -> Clock {
        Clock::default()
    }

    pub fn now(&self) -> i64 {
        let real = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        real + self.offset.load(Ordering::Relaxed)
    }

    pub fn advance(&self, secs: i64) {
        self.offset.fetch_add(secs, Ordering::Relaxed);
    }

    pub fn offset(&self) -> i64 {
        self.offset.load(Ordering::Relaxed)
    }
}
