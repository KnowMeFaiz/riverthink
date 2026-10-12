//! Application services: orchestrate domain rules + SQL. Every function takes
//! `now` explicitly so tests can simulate months in milliseconds.

pub mod auth;
pub mod election;
pub mod ideology;
pub mod moderation;
pub mod notes;
pub mod vote;

/// Count of Unicode characters (SQLite `length()` on TEXT counts characters too).
pub(crate) fn chars(s: &str) -> usize {
    s.chars().count()
}
