use crate::domain::calendar::DAY;

#[derive(Clone, Debug)]
pub struct Config {
    pub db_path: String,
    pub bind: String,
    /// `S_min` of Rule A (docs/DECISIONS.md D-05).
    pub s_min: u32,
    /// A user counts as "active" (the `U_i` of Rule A) if seen within this window.
    pub active_window_secs: i64,
    pub vote_cooldown_secs: i64,
    pub session_ttl_secs: i64,
    /// Demo/test only: lets an admin shift the application clock forward.
    pub dev_clock: bool,
    pub cookie_secure: bool,
    /// Behind a reverse proxy: take the client IP from X-Forwarded-For (rate limiting only).
    pub trust_proxy: bool,
    /// Key for hashing phone numbers (set `RIVERTHINK_PHONE_PEPPER` in production).
    pub phone_pepper: String,
    /// Registration needs a phone number (off in unit tests that build users directly).
    pub require_phone: bool,
}

impl Config {
    pub fn from_env() -> Config {
        let get = |k: &str| std::env::var(k).ok();
        let flag = |k: &str| matches!(get(k).as_deref(), Some("1") | Some("true"));
        let mut c = Config::default();
        if let Some(v) = get("RIVERTHINK_DB") {
            c.db_path = v;
        }
        if let Some(v) = get("RIVERTHINK_BIND") {
            c.bind = v;
        }
        if let Some(v) = get("RIVERTHINK_SMIN").and_then(|v| v.parse().ok()) {
            c.s_min = v;
        }
        c.dev_clock = flag("RIVERTHINK_DEV_CLOCK");
        c.cookie_secure = flag("RIVERTHINK_COOKIE_SECURE");
        c.trust_proxy = flag("RIVERTHINK_TRUST_PROXY");
        if let Some(v) = get("RIVERTHINK_PHONE_PEPPER") {
            c.phone_pepper = v;
        }
        c.require_phone = !flag("RIVERTHINK_NO_PHONE");
        c
    }
}

impl Default for Config {
    fn default() -> Config {
        Config {
            db_path: "riverthink.db".into(),
            bind: "127.0.0.1:3000".into(),
            s_min: 6,
            active_window_secs: 30 * DAY,
            vote_cooldown_secs: 7 * DAY,
            session_ttl_secs: 30 * DAY,
            dev_clock: false,
            cookie_secure: false,
            trust_proxy: false,
            phone_pepper: "dev-pepper-change-me".into(),
            require_phone: true,
        }
    }
}
