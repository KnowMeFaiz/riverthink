use sqlx::FromRow;

use crate::db::{Db, immediate};
use crate::domain::channel::Channel;
use crate::error::{AppError, AppResult};
use crate::model::User;
use crate::service::chars;

const MIN_SECONDS_BETWEEN_NOTES: i64 = 15;

/// `#tag` tokens from text: lowercase ASCII, max 24 chars, max 5 tags,
/// no duplicates. Normalizing here prevents duplicate tags that differ only
/// by capitalization.
pub fn extract_tags(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        let Some(rest) = word.strip_prefix('#') else { continue };
        let tag: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .collect::<String>()
            .to_ascii_lowercase();
        if !tag.is_empty() && tag.len() <= 24 && !out.contains(&tag) {
            out.push(tag);
            if out.len() == 5 {
                break;
            }
        }
    }
    out
}

pub fn normalize_tag(t: &str) -> Option<String> {
    let t = t.trim().trim_start_matches('#').to_ascii_lowercase();
    let ok = !t.is_empty()
        && t.len() <= 24
        && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    ok.then_some(t)
}

/// Post an Ambinote entry. `room` = the open discussion room (#E/#Z/#D) it goes into,
/// or `None` for the general Ambinote. Any user may post in any room.
pub async fn post(pool: &Db, now: i64, user: &User, title: &str, body: &str, room: Option<Channel>) -> AppResult<i64> {
    let title = title.trim();
    let body = body.trim();
    if !(1..=120).contains(&chars(title)) {
        return Err(AppError::Invalid("Judul 1-120 karakter.".into()));
    }
    if chars(body) > 2000 {
        return Err(AppError::Invalid("Isi maksimal 2000 karakter.".into()));
    }
    if title.chars().any(|c| c.is_control()) || body.chars().any(|c| c.is_control() && c != '\n' && c != '\r') {
        return Err(AppError::Invalid("Teks mengandung karakter yang tidak diizinkan.".into()));
    }
    let mut tags = extract_tags(title);
    for t in extract_tags(body) {
        if !tags.contains(&t) && tags.len() < 5 {
            tags.push(t);
        }
    }

    let mut tx = immediate(pool).await?;
    let status: Option<String> = sqlx::query_scalar("SELECT status FROM users WHERE id = ?1")
        .bind(user.id)
        .fetch_optional(&mut *tx)
        .await?;
    match status.as_deref() {
        Some("active") => {}
        Some(_) => return Err(AppError::Forbidden("Akunmu sedang dibatasi, tidak bisa memposting.".into())),
        None => return Err(AppError::Unauthorized),
    }
    let last: Option<i64> = sqlx::query_scalar("SELECT MAX(created_at) FROM notes WHERE author_id = ?1")
        .bind(user.id)
        .fetch_one(&mut *tx)
        .await?;
    if last.is_some_and(|t| now - t < MIN_SECONDS_BETWEEN_NOTES) {
        return Err(AppError::Conflict("Terlalu cepat. Tunggu sebentar sebelum memposting lagi.".into()));
    }
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO notes (author_id, title, body, created_at, room) VALUES (?1, ?2, ?3, ?4, ?5) RETURNING id",
    )
    .bind(user.id)
    .bind(title)
    .bind(body)
    .bind(now)
    .bind(room.map(|c| c.code()))
    .fetch_one(&mut *tx)
    .await?;
    for t in &tags {
        sqlx::query("INSERT INTO note_tags (note_id, tag) VALUES (?1, ?2)")
            .bind(id)
            .bind(t)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(id)
}

#[derive(Clone, Debug, FromRow)]
pub struct NoteView {
    pub id: i64,
    pub title: String,
    pub body: String,
    pub created_at: i64,
    pub author_id: i64,
    pub author: String,
    /// The author's home channel (drives the bubble colour).
    pub channel: String,
    /// The room this entry was posted in, if any.
    pub room: Option<String>,
    /// Name of the ideology the author currently supports (shown as the small label).
    pub ideology: Option<String>,
    pub tags: Option<String>,
}

impl NoteView {
    pub fn tag_list(&self) -> impl Iterator<Item = &str> {
        self.tags.as_deref().unwrap_or("").split(' ').filter(|t| !t.is_empty())
    }
}

macro_rules! note_select {
    () => {
        "SELECT n.id, n.title, n.body, n.created_at, n.author_id, u.username AS author, u.channel, n.room,
                i.name AS ideology,
                (SELECT group_concat(tag, ' ') FROM note_tags WHERE note_id = n.id) AS tags
           FROM notes n
           JOIN users u ON u.id = n.author_id
           LEFT JOIN votes v ON v.user_id = n.author_id
           LEFT JOIN ideologies i ON i.id = v.ideology_id AND i.status = 'active'"
    };
}

/// Newest first, keyset-paginated by id (`before` = last id of previous page).
/// Optional filters: a room (#E/#Z/#D) and/or a tag.
pub async fn list(
    pool: &Db,
    tag: Option<&str>,
    room: Option<Channel>,
    before: Option<i64>,
    limit: i64,
) -> AppResult<Vec<NoteView>> {
    Ok(sqlx::query_as::<_, NoteView>(concat!(
        note_select!(),
        " WHERE n.status = 'visible' AND n.id < ?1
            AND (?3 IS NULL OR n.room = ?3)
            AND (?4 IS NULL OR n.id IN (SELECT note_id FROM note_tags WHERE tag = ?4))
          ORDER BY n.id DESC LIMIT ?2"
    ))
    .bind(before.unwrap_or(i64::MAX))
    .bind(limit)
    .bind(room.map(|c| c.code()))
    .bind(tag)
    .fetch_all(pool)
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_are_normalized_and_deduplicated() {
        assert_eq!(extract_tags("Halo #AgriTech dan #agritech #Robotics!"), vec!["agritech", "robotics"]);
    }

    #[test]
    fn tags_limits() {
        let many = "#a #b #c #d #e #f #g";
        assert_eq!(extract_tags(many).len(), 5);
        assert!(extract_tags("# #! #").is_empty());
        let long = format!("#{}", "x".repeat(25));
        assert!(extract_tags(&long).is_empty());
    }

    #[test]
    fn normalize_tag_input() {
        assert_eq!(normalize_tag("#Cyber"), Some("cyber".into()));
        assert_eq!(normalize_tag("bad tag"), None);
        assert_eq!(normalize_tag(""), None);
    }
}
