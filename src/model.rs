use sqlx::FromRow;

use crate::domain::channel::Channel;

#[derive(Clone, Debug, FromRow)]
pub struct User {
    pub id: i64,
    pub username: String,
    pub channel: String,
    pub role: String,
    pub status: String,
    pub created_at: i64,
    pub last_active_at: i64,
}

impl User {
    pub fn channel(&self) -> Channel {
        // The DB CHECK constraint guarantees a valid code.
        Channel::parse(&self.channel).unwrap_or(Channel::E)
    }
    pub fn is_admin(&self) -> bool {
        self.role == "admin"
    }
    pub fn is_restricted(&self) -> bool {
        self.status == "restricted"
    }
}
