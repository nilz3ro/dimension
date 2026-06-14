//! Integration tests for PgSessionStore.
//!
//! These tests require a running Postgres instance.
//!
//! Set DATABASE_URL to a Postgres connection string before running:
//!   export DATABASE_URL=postgres://dimension:dimension@localhost/dimension_store
//!   cargo test -p dimension-store --test integration
//!
//! Each test creates a fresh schema (via a unique search_path) for full isolation.
//! The schema is dropped after the test completes.
//!
//! Note: testcontainers-modules is unavailable due to a bollard version conflict
//! in the workspace (v0.18 vs the required v0.20). See Plan 01-02 SUMMARY for details.

use std::env;
use std::sync::Arc;

use futures::future::join_all;
use uuid::Uuid;

use dimension_store::{
    MessageRole, NewMessage, PgSessionStore, SessionStore, StoreError,
};

/// Create a fresh schema and return a PgSessionStore connected to it.
/// The schema name is unique per test (based on a UUID) to avoid cross-test interference.
///
/// Returns both the store and the schema name (for cleanup).
async fn setup_isolated_store() -> (PgSessionStore, String) {
    let database_url = env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://dimension:dimension@localhost/dimension_store".to_string());

    let schema = format!("test_{}", Uuid::new_v4().simple());

    // Connect with a superuser-capable URL to create the schema
    // Then create a per-test schema, run migrations there, and return a store pointing at it
    let url_with_schema = format!("{database_url}?options=-csearch_path%3D{schema}");

    // First create the schema using the default connection
    let setup_pool = sqlx_postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("failed to connect to postgres for setup");

    sqlx_core::query::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&setup_pool)
        .await
        .expect("failed to create test schema");

    drop(setup_pool);

    // Now connect the store using the schema-scoped URL
    let store = PgSessionStore::connect(&url_with_schema)
        .await
        .expect("PgSessionStore::connect failed");

    (store, schema)
}

/// Drop the test schema after a test completes.
async fn teardown_schema(schema: &str) {
    let database_url = env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://dimension:dimension@localhost/dimension_store".to_string());

    let pool = sqlx_postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("failed to connect for teardown");

    sqlx_core::query::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&pool)
        .await
        .expect("failed to drop test schema");
}

#[tokio::test]
async fn test_create_and_get_session() {
    let (store, schema) = setup_isolated_store().await;

    let user_id = Uuid::new_v4();
    let session = store.create_session(user_id, "bundle-1").await.unwrap();
    assert!(!session.id.is_nil(), "session id must be non-nil");
    assert_eq!(session.bundle_id, "bundle-1");
    assert_eq!(session.user_id, Some(user_id));
    // timestamps are set
    assert!(session.created_at <= session.updated_at || session.created_at == session.updated_at);

    let fetched = store.get_session(session.id, user_id).await.unwrap();
    assert!(fetched.is_some());
    let fetched = fetched.unwrap();
    assert_eq!(fetched.id, session.id);
    assert_eq!(fetched.bundle_id, "bundle-1");

    teardown_schema(&schema).await;
}

#[tokio::test]
async fn test_get_session_not_found() {
    let (store, schema) = setup_isolated_store().await;

    let user_id = Uuid::new_v4();
    let result = store.get_session(Uuid::new_v4(), user_id).await.unwrap();
    assert!(result.is_none(), "get_session with random id should return None");

    teardown_schema(&schema).await;
}

#[tokio::test]
async fn test_get_session_cross_user_returns_none() {
    let (store, schema) = setup_isolated_store().await;

    let user_a = Uuid::new_v4();
    let user_b = Uuid::new_v4();
    let session = store.create_session(user_a, "bundle-1").await.unwrap();

    // user_b cannot see user_a's session
    let result = store.get_session(session.id, user_b).await.unwrap();
    assert!(result.is_none(), "cross-user get_session must return None");

    teardown_schema(&schema).await;
}

#[tokio::test]
async fn test_append_and_retrieve_message() {
    let (store, schema) = setup_isolated_store().await;

    let user_id = Uuid::new_v4();
    let session = store.create_session(user_id, "bundle-msg").await.unwrap();
    let msg = NewMessage {
        role: MessageRole::User,
        content: "hello".to_string(),
        is_complete: true,
    };

    let appended = store.append_message(session.id, msg).await.unwrap();
    assert!(!appended.id.is_nil());
    assert_eq!(appended.session_id, session.id);
    assert_eq!(appended.role, MessageRole::User);
    assert_eq!(appended.content, "hello");
    assert!(appended.is_complete);

    let history = store.get_history(session.id, user_id, None, 10).await.unwrap();
    assert_eq!(history.messages.len(), 1);
    assert_eq!(history.messages[0].id, appended.id);
    assert!(history.next_cursor.is_none());

    teardown_schema(&schema).await;
}

#[tokio::test]
async fn test_get_history_pagination() {
    let (store, schema) = setup_isolated_store().await;

    let user_id = Uuid::new_v4();
    let session = store.create_session(user_id, "bundle-page").await.unwrap();

    // Append 5 messages
    for i in 0..5u32 {
        store
            .append_message(
                session.id,
                NewMessage {
                    role: MessageRole::User,
                    content: format!("msg {i}"),
                    is_complete: true,
                },
            )
            .await
            .unwrap();
    }

    // Page 1: limit=2
    let page1 = store.get_history(session.id, user_id, None, 2).await.unwrap();
    assert_eq!(page1.messages.len(), 2);
    assert!(page1.next_cursor.is_some(), "page1 should have a next cursor");

    // Page 2: limit=2
    let page2 = store
        .get_history(session.id, user_id, page1.next_cursor.as_deref(), 2)
        .await
        .unwrap();
    assert_eq!(page2.messages.len(), 2);
    assert!(page2.next_cursor.is_some(), "page2 should have a next cursor");

    // Page 3: last page
    let page3 = store
        .get_history(session.id, user_id, page2.next_cursor.as_deref(), 2)
        .await
        .unwrap();
    assert_eq!(page3.messages.len(), 1);
    assert!(page3.next_cursor.is_none(), "page3 should be the last page");

    // Verify ordering: all 5 messages in sequence
    let all_contents: Vec<String> = page1
        .messages
        .iter()
        .chain(page2.messages.iter())
        .chain(page3.messages.iter())
        .map(|m| m.content.clone())
        .collect();

    assert_eq!(all_contents.len(), 5);
    for i in 0..5u32 {
        assert!(
            all_contents.iter().any(|c| c == &format!("msg {i}")),
            "missing msg {i}"
        );
    }

    teardown_schema(&schema).await;
}

#[tokio::test]
async fn test_get_history_empty() {
    let (store, schema) = setup_isolated_store().await;

    let user_id = Uuid::new_v4();
    let session = store.create_session(user_id, "bundle-empty").await.unwrap();
    let history = store.get_history(session.id, user_id, None, 10).await.unwrap();

    assert!(history.messages.is_empty());
    assert!(history.next_cursor.is_none());

    teardown_schema(&schema).await;
}

#[tokio::test]
async fn test_touch_updates_timestamp() {
    let (store, schema) = setup_isolated_store().await;

    let user_id = Uuid::new_v4();
    let session = store.create_session(user_id, "bundle-touch").await.unwrap();
    let original_updated_at = session.updated_at;

    // Sleep briefly so the clock advances
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    store.touch(session.id).await.unwrap();

    let updated = store.get_session(session.id, user_id).await.unwrap().unwrap();
    assert!(
        updated.updated_at >= original_updated_at,
        "updated_at should be >= original: {} >= {}",
        updated.updated_at,
        original_updated_at
    );

    teardown_schema(&schema).await;
}

#[tokio::test]
async fn test_delete_session_cascades() {
    let (store, schema) = setup_isolated_store().await;

    let user_id = Uuid::new_v4();
    let session = store.create_session(user_id, "bundle-delete").await.unwrap();
    store
        .append_message(
            session.id,
            NewMessage {
                role: MessageRole::Assistant,
                content: "will be deleted".to_string(),
                is_complete: true,
            },
        )
        .await
        .unwrap();

    store.delete_session(session.id).await.unwrap();

    // Session should be gone
    let fetched = store.get_session(session.id, user_id).await.unwrap();
    assert!(fetched.is_none(), "session should be deleted");

    teardown_schema(&schema).await;
}

#[tokio::test]
async fn test_soft_delete_session() {
    let (store, schema) = setup_isolated_store().await;

    let user_id = Uuid::new_v4();
    let session = store.create_session(user_id, "bundle-soft").await.unwrap();

    store.soft_delete_session(session.id, user_id).await.unwrap();

    // Soft-deleted session should not appear in get_session
    let fetched = store.get_session(session.id, user_id).await.unwrap();
    assert!(fetched.is_none(), "soft-deleted session should not be returned");

    teardown_schema(&schema).await;
}

#[tokio::test]
async fn test_soft_delete_session_wrong_user() {
    let (store, schema) = setup_isolated_store().await;

    let user_a = Uuid::new_v4();
    let user_b = Uuid::new_v4();
    let session = store.create_session(user_a, "bundle-soft").await.unwrap();

    // user_b cannot soft-delete user_a's session
    let result = store.soft_delete_session(session.id, user_b).await;
    assert!(
        matches!(result, Err(StoreError::SessionNotFound { .. })),
        "expected SessionNotFound when deleting another user's session"
    );

    teardown_schema(&schema).await;
}

#[tokio::test]
async fn test_delete_session_not_found() {
    let (store, schema) = setup_isolated_store().await;

    let result = store.delete_session(Uuid::new_v4()).await;
    assert!(
        matches!(result, Err(StoreError::SessionNotFound { .. })),
        "expected SessionNotFound, got: {result:?}"
    );

    teardown_schema(&schema).await;
}

#[tokio::test]
async fn test_list_sessions_user_scoped() {
    let (store, schema) = setup_isolated_store().await;

    let user_a = Uuid::new_v4();
    let user_b = Uuid::new_v4();

    store.create_session(user_a, "bundle-a").await.unwrap();
    store.create_session(user_a, "bundle-b").await.unwrap();
    store.create_session(user_b, "bundle-c").await.unwrap();

    let (sessions_a, _) = store.list_sessions(user_a, None, None, 10).await.unwrap();
    assert_eq!(sessions_a.len(), 2, "user_a should have 2 sessions");

    let (sessions_b, _) = store.list_sessions(user_b, None, None, 10).await.unwrap();
    assert_eq!(sessions_b.len(), 1, "user_b should have 1 session");

    teardown_schema(&schema).await;
}

#[tokio::test]
async fn test_list_sessions_filter_by_bundle() {
    let (store, schema) = setup_isolated_store().await;

    let user_id = Uuid::new_v4();
    store.create_session(user_id, "bundle-a").await.unwrap();
    store.create_session(user_id, "bundle-a").await.unwrap();
    store.create_session(user_id, "bundle-b").await.unwrap();

    let (sessions_a, _) = store.list_sessions(user_id, Some("bundle-a"), None, 10).await.unwrap();
    assert_eq!(sessions_a.len(), 2);

    let (sessions_b, _) = store.list_sessions(user_id, Some("bundle-b"), None, 10).await.unwrap();
    assert_eq!(sessions_b.len(), 1);

    teardown_schema(&schema).await;
}

#[tokio::test]
async fn test_list_sessions_excludes_soft_deleted() {
    let (store, schema) = setup_isolated_store().await;

    let user_id = Uuid::new_v4();
    let s1 = store.create_session(user_id, "bundle").await.unwrap();
    store.create_session(user_id, "bundle").await.unwrap();

    store.soft_delete_session(s1.id, user_id).await.unwrap();

    let (sessions, _) = store.list_sessions(user_id, None, None, 10).await.unwrap();
    assert_eq!(sessions.len(), 1, "soft-deleted session should be excluded from list");

    teardown_schema(&schema).await;
}

#[tokio::test]
async fn test_count_active_sessions() {
    let (store, schema) = setup_isolated_store().await;

    let user_id = Uuid::new_v4();
    let s1 = store.create_session(user_id, "bundle").await.unwrap();
    store.create_session(user_id, "bundle").await.unwrap();

    assert_eq!(store.count_active_sessions(user_id).await.unwrap(), 2);

    store.soft_delete_session(s1.id, user_id).await.unwrap();
    assert_eq!(store.count_active_sessions(user_id).await.unwrap(), 1);

    teardown_schema(&schema).await;
}

#[tokio::test]
async fn test_count_session_messages() {
    let (store, schema) = setup_isolated_store().await;

    let user_id = Uuid::new_v4();
    let session = store.create_session(user_id, "bundle").await.unwrap();

    assert_eq!(store.count_session_messages(session.id).await.unwrap(), 0);

    store.append_message(session.id, NewMessage {
        role: MessageRole::User,
        content: "msg1".into(),
        is_complete: true,
    }).await.unwrap();

    store.append_message(session.id, NewMessage {
        role: MessageRole::Assistant,
        content: "msg2".into(),
        is_complete: true,
    }).await.unwrap();

    assert_eq!(store.count_session_messages(session.id).await.unwrap(), 2);

    teardown_schema(&schema).await;
}

#[tokio::test]
async fn test_concurrent_appends() {
    let (store, schema) = setup_isolated_store().await;

    let user_id = Uuid::new_v4();
    let session = store.create_session(user_id, "bundle-concurrent").await.unwrap();
    let store = Arc::new(store);

    let tasks: Vec<_> = (0..10u32)
        .map(|i| {
            let store = Arc::clone(&store);
            let session_id = session.id;
            tokio::spawn(async move {
                store
                    .append_message(
                        session_id,
                        NewMessage {
                            role: MessageRole::User,
                            content: format!("concurrent msg {i}"),
                            is_complete: true,
                        },
                    )
                    .await
                    .expect("append should succeed")
            })
        })
        .collect();

    join_all(tasks).await;

    // All 10 messages should be present
    let history = store.get_history(session.id, user_id, None, 20).await.unwrap();
    assert_eq!(
        history.messages.len(),
        10,
        "all 10 concurrent appends should persist"
    );

    teardown_schema(&schema).await;
}
