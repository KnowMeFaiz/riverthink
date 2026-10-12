use std::fmt;

/// Application error. User-facing messages are Indonesian; they are shown
/// verbatim by the web layer, so never put internals in them.
#[derive(Debug)]
pub enum AppError {
    /// 400 — malformed or out-of-range input.
    Invalid(String),
    /// 401 — login required.
    Unauthorized,
    /// 403 — logged in but not allowed.
    Forbidden(String),
    /// 404
    NotFound,
    /// 409 — valid request that conflicts with current state (duplicate, cooldown, locked...).
    Conflict(String),
    /// 429
    RateLimited,
    /// 500 — logged to stderr, generic message to the user.
    Internal(String),
}

pub type AppResult<T> = Result<T, AppError>;

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AppError::Invalid(m) | AppError::Forbidden(m) | AppError::Conflict(m) => f.write_str(m),
            AppError::Unauthorized => f.write_str("Silakan login dulu."),
            AppError::NotFound => f.write_str("Tidak ditemukan."),
            AppError::RateLimited => f.write_str("Terlalu banyak percobaan. Coba lagi nanti."),
            AppError::Internal(_) => f.write_str("Terjadi kesalahan di server."),
        }
    }
}

impl std::error::Error for AppError {}

impl From<sqlx::Error> for AppError {
    fn from(e: sqlx::Error) -> AppError {
        if let sqlx::Error::Database(db) = &e {
            if db.is_unique_violation() {
                return AppError::Conflict("Data tersebut sudah ada.".into());
            }
        }
        AppError::Internal(e.to_string())
    }
}

impl From<sqlx::migrate::MigrateError> for AppError {
    fn from(e: sqlx::migrate::MigrateError) -> AppError {
        AppError::Internal(e.to_string())
    }
}
