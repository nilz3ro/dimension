//! Shared test utilities for dimension-gateway tests.
//!
//! Provides mock implementations of store traits for use in handler unit tests.
//! These mocks return sensible defaults and never touch a real database.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use async_trait::async_trait;
use uuid::Uuid;

use dimension_store::{
    HistoryPage, Message, MessageRole, NewMessage, NewSessionEvent, Session,
    SessionEvent, SessionEventType, SessionSummary, StoreError,
};

/// In-memory mock session store for tests. Supports create and get
/// so that tests creating sessions can later look them up.
pub struct MockSessionStore {
    sessions: Mutex<HashMap<Uuid, Session>>,
}

impl Default for MockSessionStore {
    fn default() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
        }
    }
}

impl MockSessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn new_arc() -> Arc<dyn dimension_store::SessionStore> {
        Arc::new(Self::new())
    }
}

#[async_trait]
impl dimension_store::SessionStore for MockSessionStore {
    async fn create_session(&self, user_id: Uuid, bundle_id: &str) -> Result<Session, StoreError> {
        let session = Session {
            id: Uuid::new_v4(),
            user_id: Some(user_id),
            bundle_id: bundle_id.to_string(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            metadata: serde_json::Value::Object(serde_json::Map::new()),
        };
        self.sessions.lock().unwrap().insert(session.id, session.clone());
        Ok(session)
    }

    async fn create_session_with_id(&self, id: Uuid, user_id: Uuid, bundle_id: &str) -> Result<Session, StoreError> {
        let session = Session {
            id,
            user_id: Some(user_id),
            bundle_id: bundle_id.to_string(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            metadata: serde_json::Value::Object(serde_json::Map::new()),
        };
        self.sessions.lock().unwrap().insert(session.id, session.clone());
        Ok(session)
    }

    async fn get_session(&self, session_id: Uuid, user_id: Uuid) -> Result<Option<Session>, StoreError> {
        let sessions = self.sessions.lock().unwrap();
        Ok(sessions.get(&session_id).filter(|s| s.user_id == Some(user_id)).cloned())
    }

    async fn append_message(&self, session_id: Uuid, _msg: NewMessage) -> Result<Message, StoreError> {
        Ok(Message {
            id: Uuid::new_v4(),
            session_id,
            role: MessageRole::User,
            content: String::new(),
            is_complete: true,
            created_at: chrono::Utc::now(),
        })
    }

    async fn get_history(&self, _session_id: Uuid, _user_id: Uuid, _cursor: Option<&str>, _limit: u32) -> Result<HistoryPage, StoreError> {
        Ok(HistoryPage {
            messages: vec![],
            next_cursor: None,
        })
    }

    async fn touch(&self, _session_id: Uuid) -> Result<(), StoreError> { Ok(()) }

    async fn delete_session(&self, session_id: Uuid) -> Result<(), StoreError> {
        self.sessions.lock().unwrap().remove(&session_id);
        Ok(())
    }

    async fn soft_delete_session(&self, session_id: Uuid, _user_id: Uuid) -> Result<(), StoreError> {
        self.sessions.lock().unwrap().remove(&session_id);
        Ok(())
    }

    async fn list_sessions(&self, _user_id: Uuid, _bundle_id: Option<&str>, _cursor: Option<&str>, _limit: i64) -> Result<(Vec<SessionSummary>, Option<String>), StoreError> {
        Ok((vec![], None))
    }

    async fn count_active_sessions(&self, _user_id: Uuid) -> Result<i64, StoreError> { Ok(0) }

    async fn count_session_messages(&self, _session_id: Uuid) -> Result<i64, StoreError> { Ok(0) }

    async fn append_event(&self, _event: NewSessionEvent) -> Result<SessionEvent, StoreError> {
        Ok(SessionEvent {
            id: Uuid::new_v4(),
            session_id: Uuid::nil(),
            event_type: SessionEventType::Message,
            role: None,
            content: String::new(),
            created_at: chrono::Utc::now(),
        })
    }

    async fn get_events(&self, _session_id: Uuid, _limit: Option<i64>) -> Result<Vec<SessionEvent>, StoreError> {
        Ok(vec![])
    }

    async fn get_events_after(&self, _session_id: Uuid, _after_id: Uuid, _limit: Option<i64>) -> Result<Vec<SessionEvent>, StoreError> {
        Ok(vec![])
    }

    async fn get_metadata(&self, _session_id: Uuid) -> Result<serde_json::Value, StoreError> {
        Ok(serde_json::Value::Object(serde_json::Map::new()))
    }

    async fn update_metadata(&self, _session_id: Uuid, _patch: serde_json::Value) -> Result<serde_json::Value, StoreError> {
        Ok(serde_json::Value::Object(serde_json::Map::new()))
    }

    async fn admin_list_sessions(&self, _bundle_id: Option<&str>) -> Result<Vec<SessionSummary>, StoreError> {
        Ok(vec![])
    }

    async fn admin_get_session(&self, session_id: Uuid) -> Result<Option<Session>, StoreError> {
        let sessions = self.sessions.lock().unwrap();
        Ok(sessions.get(&session_id).cloned())
    }

    async fn admin_get_history(&self, _session_id: Uuid) -> Result<Vec<Message>, StoreError> {
        Ok(vec![])
    }

    async fn admin_search_sessions(
        &self,
        _query: Option<&str>,
        _bundle: Option<&str>,
        _from: Option<chrono::DateTime<chrono::Utc>>,
        _to: Option<chrono::DateTime<chrono::Utc>>,
        _limit: i64,
    ) -> Result<Vec<dimension_store::SessionSearchResult>, StoreError> {
        Ok(vec![])
    }

    async fn admin_soft_delete_session(&self, session_id: Uuid) -> Result<(), StoreError> {
        self.sessions.lock().unwrap().remove(&session_id);
        Ok(())
    }
}
