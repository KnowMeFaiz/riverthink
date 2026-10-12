//! HTTP layer: router, extractors, security middleware, rate limiting.
//! Handlers stay thin; all rules live in `service` and `domain`.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};

use axum::extract::{DefaultBodyLimit, FromRequestParts, Request};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::Router;
use tower_http::compression::CompressionLayer;

use crate::clock::Clock;
use crate::config::Config;
use crate::db::Db;
use crate::domain::channel::Channel;
use crate::error::{AppError, AppResult};
use crate::model::User;
use crate::service::auth;
use crate::service::ideology::{self, CapacityInfo, IdeologyRow};

mod admin;
pub mod html;
mod pages;

/// Per-channel ideology lists plus Rule A capacity, for the public board page.
pub type Board = Vec<(Channel, Vec<IdeologyRow>, CapacityInfo)>;

/// The board is recomputed at most once per this many seconds; the ideology
/// detail page and the vote action itself are always live.
const BOARD_TTL_SECS: i64 = 5;

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub cfg: Arc<Config>,
    pub clock: Arc<Clock>,
    pub limiter: Arc<Limiter>,
    board: Arc<Mutex<Option<(i64, Arc<Board>)>>>,
}

impl AppState {
    pub fn new(db: Db, cfg: Config) -> AppState {
        AppState {
            db,
            cfg: Arc::new(cfg),
            clock: Arc::new(Clock::new()),
            limiter: Arc::new(Limiter::default()),
            board: Arc::new(Mutex::new(None)),
        }
    }

    pub(crate) async fn board(&self) -> AppResult<Arc<Board>> {
        let now = self.clock.now();
        if let Some((t, b)) = self.board.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            if now - *t < BOARD_TTL_SECS && now >= *t {
                return Ok(b.clone());
            }
        }
        let mut board = Board::new();
        for ch in Channel::ALL {
            let rows = ideology::list_by_channel(&self.db, &self.cfg, now, ch).await?;
            let info = ideology::capacity_info_pool(&self.db, &self.cfg, now, ch).await?;
            board.push((ch, rows, info));
        }
        let board = Arc::new(board);
        *self.board.lock().unwrap_or_else(|e| e.into_inner()) = Some((now, board.clone()));
        Ok(board)
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(pages::feed))
        .route("/notes", post(pages::post_note))
        .route("/tag/{tag}", get(pages::tag))
        .route("/c/{code}", get(pages::room))
        .route("/register", get(pages::register_form).post(pages::register))
        .route("/login", get(pages::login_form).post(pages::login))
        .route("/logout", post(pages::logout))
        .route("/ideologies", get(pages::ideologies).post(pages::create_ideology))
        .route("/ideologies/{id}", get(pages::ideology_detail))
        .route("/ideologies/{id}/vote", post(pages::cast_vote))
        .route("/final-nine", get(pages::final_nine))
        .route("/election", get(pages::election))
        .route("/me", get(pages::me))
        .route("/me/delete", post(pages::delete_me))
        .route("/report", post(pages::report))
        .route("/appeal", post(pages::appeal))
        .route("/rules", get(pages::rules))
        .route("/healthz", get(pages::healthz))
        .route("/s.css", get(pages::css))
        .route("/admin", get(admin::overview))
        .route("/admin/act", post(admin::act))
        .route("/admin/report/{id}/dismiss", post(admin::dismiss))
        .route("/admin/appeal/{id}/{verdict}", post(admin::resolve_appeal))
        .route("/admin/dev/advance", post(admin::dev_advance))
        .fallback(pages::not_found)
        .layer(middleware::from_fn(csrf))
        .layer(middleware::from_fn(security_headers))
        .layer(CompressionLayer::new())
        .layer(DefaultBodyLimit::max(32 * 1024))
        .with_state(state)
}

// ---- errors -------------------------------------------------------------------

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = match &self {
            AppError::Invalid(_) => StatusCode::BAD_REQUEST,
            AppError::Unauthorized => return Redirect::to("/login").into_response(),
            AppError::Forbidden(_) => StatusCode::FORBIDDEN,
            AppError::NotFound => StatusCode::NOT_FOUND,
            AppError::Conflict(_) => StatusCode::CONFLICT,
            AppError::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            AppError::Internal(detail) => {
                eprintln!("internal error: {detail}");
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };
        (status, html::error_page(&self.to_string())).into_response()
    }
}

// ---- sessions / extractors ----------------------------------------------------------

pub(crate) const COOKIE: &str = "rt_session";

pub(crate) fn cookie_value<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    header.split(';').find_map(|p| p.trim().strip_prefix(name)?.strip_prefix('='))
}

pub(crate) fn token_from(headers: &HeaderMap) -> Option<&str> {
    headers.get(header::COOKIE).and_then(|v| v.to_str().ok()).and_then(|c| cookie_value(c, COOKIE))
}

pub(crate) fn session_cookie(token: &str, cfg: &Config) -> String {
    format!(
        "{COOKIE}={token}; HttpOnly; SameSite=Lax; Path=/; Max-Age={}{}",
        cfg.session_ttl_secs,
        if cfg.cookie_secure { "; Secure" } else { "" }
    )
}

pub(crate) fn clear_cookie() -> String {
    format!("{COOKIE}=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0")
}

/// The logged-in user, if any.
pub struct MaybeUser(pub Option<User>);

impl FromRequestParts<AppState> for MaybeUser {
    type Rejection = AppError;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, AppError> {
        let Some(token) = token_from(&parts.headers) else { return Ok(MaybeUser(None)) };
        Ok(MaybeUser(auth::session_user(&state.db, state.clock.now(), token).await?))
    }
}

/// Redirects to /login when there is no session.
pub struct RequireUser(pub User);

impl FromRequestParts<AppState> for RequireUser {
    type Rejection = AppError;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, AppError> {
        let MaybeUser(u) = MaybeUser::from_request_parts(parts, state).await?;
        u.map(RequireUser).ok_or(AppError::Unauthorized)
    }
}

pub struct RequireAdmin(pub User);

impl FromRequestParts<AppState> for RequireAdmin {
    type Rejection = AppError;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, AppError> {
        let RequireUser(u) = RequireUser::from_request_parts(parts, state).await?;
        if u.is_admin() {
            Ok(RequireAdmin(u))
        } else {
            Err(AppError::Forbidden("Halaman ini khusus admin.".into()))
        }
    }
}

// ---- middleware ---------------------------------------------------------------------

/// Reject cross-site form posts: for any state-changing request that carries an
/// Origin (or Referer) header, its host must equal the Host header. Combined
/// with `SameSite=Lax` cookies this is a lightweight CSRF defence, no tokens needed.
async fn csrf(req: Request, next: Next) -> Response {
    if !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS) {
        let host = req.headers().get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("");
        let origin = req
            .headers()
            .get(header::ORIGIN)
            .or_else(|| req.headers().get(header::REFERER))
            .and_then(|v| v.to_str().ok());
        if let Some(o) = origin {
            let o_host = o.split("://").nth(1).and_then(|r| r.split('/').next()).unwrap_or("");
            if o_host != host {
                return (StatusCode::FORBIDDEN, "Asal permintaan tidak valid.").into_response();
            }
        }
    }
    next.run(req).await
}

async fn security_headers(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    h.insert("x-frame-options", HeaderValue::from_static("DENY"));
    h.insert("referrer-policy", HeaderValue::from_static("same-origin"));
    h.insert(
        "content-security-policy",
        HeaderValue::from_static("default-src 'self'; frame-ancestors 'none'; form-action 'self'; base-uri 'none'"),
    );
    res
}

// ---- rate limiting -----------------------------------------------------------------------

/// Fixed-window limiter keyed by (ip, action). In-memory and per-process; good
/// enough for a single-instance MVP (known limitation: resets on restart).
#[derive(Default)]
pub struct Limiter {
    hits: Mutex<HashMap<(IpAddr, &'static str), (i64, u32)>>,
}

impl Limiter {
    /// Returns true if the action is allowed (and counts it).
    pub fn allow(&self, ip: IpAddr, key: &'static str, limit: u32, window_secs: i64, now: i64) -> bool {
        let mut map = self.hits.lock().unwrap_or_else(|e| e.into_inner());
        if map.len() > 10_000 {
            map.retain(|_, (start, _)| now - *start < window_secs);
        }
        let e = map.entry((ip, key)).or_insert((now, 0));
        if now - e.0 >= window_secs {
            *e = (now, 0);
        }
        if e.1 >= limit {
            return false;
        }
        e.1 += 1;
        true
    }
}

pub(crate) fn client_ip(headers: &HeaderMap, addr: SocketAddr, trust_proxy: bool) -> IpAddr {
    if trust_proxy {
        if let Some(ip) = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
            .and_then(|v| v.trim().parse().ok())
        {
            return ip;
        }
    }
    addr.ip()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_parsing_is_exact() {
        assert_eq!(cookie_value("a=1; rt_session=abc; b=2", "rt_session"), Some("abc"));
        assert_eq!(cookie_value("rt_session2=zzz", "rt_session"), None);
        assert_eq!(cookie_value("x=1", "rt_session"), None);
    }

    #[test]
    fn limiter_windows() {
        let l = Limiter::default();
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        for _ in 0..3 {
            assert!(l.allow(ip, "login", 3, 60, 1000));
        }
        assert!(!l.allow(ip, "login", 3, 60, 1030));
        assert!(l.allow(ip, "register", 3, 60, 1030)); // separate bucket
        assert!(l.allow(ip, "login", 3, 60, 1060)); // new window
    }
}
