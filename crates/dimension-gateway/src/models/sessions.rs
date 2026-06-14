use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Serialize)]
pub struct ListSessionsResponse {
    pub sessions: Vec<SessionSummaryResponse>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SessionSummaryResponse {
    pub id: Uuid,
    pub bundle_id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub message_count: i64,
}

#[derive(Debug, Serialize)]
pub struct SessionHistoryResponse {
    pub messages: Vec<HistoryEntryResponse>,
    pub total_messages: i64,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct HistoryEntryResponse {
    pub role: String,
    pub content: String,
    pub timestamp: DateTime<Utc>,
    pub is_complete: bool,
}

#[derive(Debug, Deserialize)]
pub struct ListSessionsQuery {
    pub bundle_id: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct HistoryQuery {
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

