//! DEBT-01: Schema validation integration test gate.
//!
//! This test is the CI gate for catching FromRow column-name mismatches at test time.
//! It exercises every model type's `FromRow` implementation by doing a full round-trip:
//! insert via the store trait method -> SELECT * -> verify all fields are populated.
//!
//! **Why this approach (Path B) instead of sqlx offline mode (Path A):**
//! The project uses runtime `query_as::<_, T>(sql)` rather than compile-time `query_as!`
//! macros, so `cargo sqlx prepare` cannot introspect the queries. Adding the `sqlx`
//! umbrella crate with `macros` feature would introduce `sqlx-sqlite` -> `libsqlite3-sys`,
//! conflicting with the `rusqlite bundled` in `hyphae-core`. This test catches the same
//! class of bugs (column-name typos in FromRow impls) at CI time.
//!
//! **Running locally:**
//! ```sh
//! export DATABASE_URL=postgres://dimension:dimension@localhost/dimension_store
//! cargo test -p dimension-store --test schema_validation
//! ```
//!
//! **CI integration (add to .github/workflows/ci.yml):**
//! ```yaml
//! - name: Schema validation (DEBT-01 gate)
//!   env:
//!     DATABASE_URL: postgres://test:test@localhost:5432/dimension_test
//!   run: cargo test -p dimension-store --test schema_validation -- --nocapture
//! ```

use std::env;

use bytes::Bytes;
use uuid::Uuid;

use dimension_store::{
    Artifact, ArtifactStore, NewSessionEvent, PgSessionStore, SessionEvent, SessionEventType,
    SessionStore, UserRole, UserStore, Volume, VolumeStore,
};

/// Create an isolated schema for a single test run.
async fn setup_store() -> (PgSessionStore, String) {
    let database_url = env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://dimension:dimension@localhost/dimension_store".to_string());

    let schema = format!("test_sv_{}", Uuid::new_v4().simple());

    let setup_pool = sqlx_postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("failed to connect for schema setup");

    sqlx_core::query::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&setup_pool)
        .await
        .expect("failed to create test schema");

    drop(setup_pool);

    let url_with_schema = format!("{database_url}?options=-csearch_path%3D{schema}");
    let store = PgSessionStore::connect(&url_with_schema)
        .await
        .expect("PgSessionStore::connect failed");

    (store, schema)
}

/// Drop the test schema after the test.
async fn teardown(schema: &str) {
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

/// Build an opendal in-memory operator (no external services required).
fn memory_operator() -> opendal::Operator {
    let builder = opendal::services::Memory::default();
    opendal::Operator::new(builder)
        .expect("memory operator should always build")
        .finish()
}

// ── Volume FromRow validation ─────────────────────────────────────────────────

/// Validates Volume FromRow: create + list + get all exercise the Volume struct mapping.
/// Catches column-name mismatches in Volume::from_row (id, user_id, size_bytes,
/// session_id, worker_id, created_at, updated_at).
#[tokio::test]
async fn test_volume_fromrow_all_fields() {
    let (store, schema) = setup_store().await;
    let (user, _key) = store.create_user("vol-test-user", UserRole::User).await.unwrap();

    // Create: exercises INSERT ... RETURNING -> Volume::from_row
    let volume: Volume = store.create_volume(user.id, 1_073_741_824).await.unwrap();

    assert!(!volume.id.is_nil(), "volume.id should be non-nil");
    assert_eq!(volume.user_id, user.id, "volume.user_id mismatch");
    assert_eq!(volume.size_bytes, 1_073_741_824, "volume.size_bytes mismatch");
    assert!(volume.session_id.is_none(), "new volume should have no session_id");
    assert!(volume.worker_id.is_none(), "new volume should have no worker_id");
    assert!(volume.created_at.timestamp() > 0, "volume.created_at should be set");
    assert!(volume.updated_at.timestamp() > 0, "volume.updated_at should be set");

    // Get: exercises SELECT ... Volume::from_row with all fields present
    let fetched: Option<Volume> = store.get_volume(volume.id).await.unwrap();
    let fetched = fetched.expect("get_volume should return the created volume");
    assert_eq!(fetched.id, volume.id, "fetched volume.id mismatch");
    assert_eq!(fetched.user_id, volume.user_id, "fetched volume.user_id mismatch");
    assert_eq!(fetched.size_bytes, volume.size_bytes, "fetched volume.size_bytes mismatch");
    assert!(fetched.session_id.is_none());
    assert!(fetched.worker_id.is_none());

    // Attach: exercises UPDATE ... RETURNING with session_id now set
    let session = store.create_session(user.id, "test-bundle").await.unwrap();
    let attached: Volume = store.attach_volume(volume.id, session.id).await.unwrap();
    assert_eq!(attached.session_id, Some(session.id), "attached volume.session_id should be set");

    // Set worker: verify worker_id field survives round-trip
    store.set_worker(volume.id, Some("worker-abc")).await.unwrap();
    let with_worker = store.get_volume(volume.id).await.unwrap().expect("volume should still exist");
    assert_eq!(
        with_worker.worker_id.as_deref(),
        Some("worker-abc"),
        "volume.worker_id should be 'worker-abc' after set_worker"
    );

    // List: exercises SELECT * -> Volume::from_row in list context
    let list: Vec<Volume> = store.list_volumes_for_user(user.id).await.unwrap();
    assert_eq!(list.len(), 1, "list_volumes_for_user should return 1 volume");
    assert_eq!(list[0].id, volume.id);

    teardown(&schema).await;
}

// ── Volume: find_volume_for_session ─────────────────────────────────────────

/// Validates find_volume_for_session returns Some(Volume) when a volume
/// is attached to the given session_id.
#[tokio::test]
async fn test_find_volume_for_session_some() {
    let (store, schema) = setup_store().await;
    let (user, _key) = store.create_user("fvfs-user", UserRole::User).await.unwrap();
    let session = store.create_session(user.id, "fvfs-bundle").await.unwrap();

    let volume = store.create_volume(user.id, 1_073_741_824).await.unwrap();
    let _attached = store.attach_volume(volume.id, session.id).await.unwrap();

    let found = store.find_volume_for_session(session.id).await.unwrap();
    assert!(found.is_some(), "find_volume_for_session should return Some when attached");
    let found = found.unwrap();
    assert_eq!(found.id, volume.id, "returned volume id should match");
    assert_eq!(found.session_id, Some(session.id), "returned volume should be attached to session");

    teardown(&schema).await;
}

/// Validates find_volume_for_session returns None when no volume is attached.
#[tokio::test]
async fn test_find_volume_for_session_none() {
    let (store, schema) = setup_store().await;
    let (user, _key) = store.create_user("fvfs-none-user", UserRole::User).await.unwrap();
    let session = store.create_session(user.id, "fvfs-none-bundle").await.unwrap();

    let found = store.find_volume_for_session(session.id).await.unwrap();
    assert!(found.is_none(), "find_volume_for_session should return None when no volume attached");

    teardown(&schema).await;
}

// ── Volume: touch_volume ────────────────────────────────────────────────────

/// Validates touch_volume updates the last_accessed timestamp.
#[tokio::test]
async fn test_touch_volume_updates_last_accessed() {
    let (store, schema) = setup_store().await;
    let (user, _key) = store.create_user("touch-user", UserRole::User).await.unwrap();

    let volume = store.create_volume(user.id, 512 * 1024 * 1024).await.unwrap();
    let original_last_accessed = volume.last_accessed;

    // Small delay to ensure timestamp differs
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    store.touch_volume(volume.id).await.unwrap();

    let updated = store.get_volume(volume.id).await.unwrap().expect("volume should exist");
    assert!(
        updated.last_accessed > original_last_accessed,
        "touch_volume should advance last_accessed (was {:?}, now {:?})",
        original_last_accessed,
        updated.last_accessed
    );
    // updated_at should NOT have changed (touch_volume only updates last_accessed)
    assert_eq!(
        updated.updated_at, volume.updated_at,
        "touch_volume should not change updated_at"
    );

    teardown(&schema).await;
}

// ── Volume: last_accessed in FromRow ────────────────────────────────────────

/// Validates that the Volume model's last_accessed field survives a full
/// FromRow round-trip (create -> get -> verify field is populated).
#[tokio::test]
async fn test_volume_last_accessed_fromrow() {
    let (store, schema) = setup_store().await;
    let (user, _key) = store.create_user("la-user", UserRole::User).await.unwrap();

    let volume = store.create_volume(user.id, 256 * 1024 * 1024).await.unwrap();
    assert!(volume.last_accessed.timestamp() > 0, "last_accessed should be set on create");

    let fetched = store.get_volume(volume.id).await.unwrap().expect("volume should exist");
    assert_eq!(
        fetched.last_accessed, volume.last_accessed,
        "last_accessed should survive get_volume round-trip"
    );

    let list = store.list_volumes_for_user(user.id).await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(
        list[0].last_accessed, volume.last_accessed,
        "last_accessed should survive list_volumes_for_user round-trip"
    );

    teardown(&schema).await;
}

// ── Artifact FromRow validation ───────────────────────────────────────────────

/// Validates Artifact FromRow: put + list + get exercise all Artifact fields.
/// Catches column-name mismatches in Artifact::from_row (id, session_id, user_id,
/// object_key, size_bytes, content_type, checksum, created_at).
#[tokio::test]
async fn test_artifact_fromrow_all_fields() {
    let (store, schema) = setup_store().await;
    let (user, _key) = store.create_user("art-test-user", UserRole::User).await.unwrap();
    let session = store.create_session(user.id, "art-bundle").await.unwrap();

    let operator = memory_operator();
    let data = Bytes::from_static(b"hello artifact data");

    // put_artifact: exercises INSERT ... RETURNING -> Artifact::from_row
    let artifact: Artifact = store
        .put_artifact(&operator, session.id, user.id, "test/file.txt", data, Some("text/plain"))
        .await
        .unwrap();

    assert!(!artifact.id.is_nil(), "artifact.id should be non-nil");
    assert_eq!(artifact.session_id, session.id, "artifact.session_id mismatch");
    assert_eq!(artifact.user_id, user.id, "artifact.user_id mismatch");
    assert_eq!(artifact.object_key, "test/file.txt", "artifact.object_key mismatch");
    assert_eq!(artifact.size_bytes, 19, "artifact.size_bytes mismatch (should be 19 bytes)");
    assert_eq!(
        artifact.content_type.as_deref(),
        Some("text/plain"),
        "artifact.content_type mismatch"
    );
    assert!(
        artifact.checksum.is_some(),
        "artifact.checksum should be populated (SHA-256)"
    );
    // Verify checksum is a 64-char hex string (SHA-256)
    let checksum = artifact.checksum.as_ref().unwrap();
    assert_eq!(checksum.len(), 64, "artifact.checksum should be 64-char hex (SHA-256)");
    assert!(
        checksum.chars().all(|c| c.is_ascii_hexdigit()),
        "artifact.checksum should be hex digits only"
    );
    assert!(artifact.created_at.timestamp() > 0, "artifact.created_at should be set");

    // get_artifact: exercises SELECT * -> Artifact::from_row
    let fetched: Option<Artifact> = store
        .get_artifact(session.id, user.id, "test/file.txt")
        .await
        .unwrap();
    let fetched = fetched.expect("get_artifact should return the stored artifact");
    assert_eq!(fetched.id, artifact.id);
    assert_eq!(fetched.session_id, artifact.session_id);
    assert_eq!(fetched.user_id, artifact.user_id);
    assert_eq!(fetched.object_key, artifact.object_key);
    assert_eq!(fetched.size_bytes, artifact.size_bytes);
    assert_eq!(fetched.content_type, artifact.content_type);
    assert_eq!(fetched.checksum, artifact.checksum);

    // list_artifacts_for_session: exercises SELECT * -> Vec<Artifact>
    let list: Vec<Artifact> = store
        .list_artifacts_for_session(session.id, user.id)
        .await
        .unwrap();
    assert_eq!(list.len(), 1, "list_artifacts_for_session should return 1 artifact");
    assert_eq!(list[0].id, artifact.id);

    // Artifact with no content_type: verifies nullable fields map correctly
    let artifact_no_ct: Artifact = store
        .put_artifact(&operator, session.id, user.id, "test/no-ct.bin", Bytes::from_static(b"x"), None)
        .await
        .unwrap();
    assert!(
        artifact_no_ct.content_type.is_none(),
        "artifact.content_type should be None when not provided"
    );

    teardown(&schema).await;
}

// ── SessionEvent FromRow validation ──────────────────────────────────────────

/// Validates SessionEvent FromRow: append + get exercise all SessionEvent fields.
/// Catches column-name mismatches in SessionEvent::from_row (id, session_id,
/// event_type, role, content, created_at) and SessionEventType decode.
#[tokio::test]
async fn test_session_event_fromrow_all_fields() {
    let (store, schema) = setup_store().await;
    let (user, _key) = store.create_user("evt-test-user", UserRole::User).await.unwrap();
    let session = store.create_session(user.id, "evt-bundle").await.unwrap();

    // Append a message event (with role)
    let new_event = NewSessionEvent {
        session_id: session.id,
        event_type: SessionEventType::Message,
        role: Some("user".to_string()),
        content: "hello from session event".to_string(),
    };
    let event: SessionEvent = store.append_event(new_event).await.unwrap();

    assert!(!event.id.is_nil(), "event.id should be non-nil");
    assert_eq!(event.session_id, session.id, "event.session_id mismatch");
    assert_eq!(event.event_type, SessionEventType::Message, "event.event_type mismatch");
    assert_eq!(
        event.role.as_deref(),
        Some("user"),
        "event.role should be 'user'"
    );
    assert_eq!(event.content, "hello from session event", "event.content mismatch");
    assert!(event.created_at.timestamp() > 0, "event.created_at should be set");

    // Append a tool_call event (no role)
    let tool_event = NewSessionEvent {
        session_id: session.id,
        event_type: SessionEventType::ToolCall,
        role: None,
        content: r#"{"name":"bash","input":{"cmd":"ls"}}"#.to_string(),
    };
    let tool_ev: SessionEvent = store.append_event(tool_event).await.unwrap();
    assert_eq!(tool_ev.event_type, SessionEventType::ToolCall, "tool_call event_type mismatch");
    assert!(tool_ev.role.is_none(), "tool_call event should have no role");

    // Append a tool_result event
    let result_event = NewSessionEvent {
        session_id: session.id,
        event_type: SessionEventType::ToolResult,
        role: None,
        content: "file1.txt\nfile2.txt".to_string(),
    };
    let result_ev: SessionEvent = store.append_event(result_event).await.unwrap();
    assert_eq!(
        result_ev.event_type,
        SessionEventType::ToolResult,
        "tool_result event_type mismatch"
    );

    // get_events: exercises SELECT * -> Vec<SessionEvent> with all three event types
    let events: Vec<SessionEvent> = store.get_events(session.id, None).await.unwrap();
    assert_eq!(events.len(), 3, "get_events should return all 3 appended events");

    // Verify ordering: chronological (ASC)
    assert_eq!(events[0].id, event.id, "first event should be the message event");
    assert_eq!(events[1].id, tool_ev.id, "second event should be the tool_call event");
    assert_eq!(events[2].id, result_ev.id, "third event should be the tool_result event");

    // Verify event_type round-trip for all three variants
    assert_eq!(events[0].event_type, SessionEventType::Message);
    assert_eq!(events[1].event_type, SessionEventType::ToolCall);
    assert_eq!(events[2].event_type, SessionEventType::ToolResult);

    // get_events with limit: verifies limit parameter works
    let limited: Vec<SessionEvent> = store.get_events(session.id, Some(2)).await.unwrap();
    assert_eq!(limited.len(), 2, "get_events with limit=2 should return 2 events");

    teardown(&schema).await;
}

// ── Cross-model round-trip ────────────────────────────────────────────────────

/// Full round-trip test exercising all Phase 14 model types in a single scenario:
/// user -> session -> volume -> artifact -> session_event.
///
/// This is the primary DEBT-01 gate: if any column name in any FromRow impl is wrong,
/// this test will catch it at CI time.
#[tokio::test]
async fn test_full_schema_roundtrip() {
    let (store, schema) = setup_store().await;

    // 1. User
    let (user, _key) = store.create_user("roundtrip-user", UserRole::Admin).await.unwrap();
    assert_eq!(user.role, UserRole::Admin);

    // 2. Session
    let session = store.create_session(user.id, "roundtrip-bundle").await.unwrap();
    assert_eq!(session.bundle_id, "roundtrip-bundle");

    // 3. Volume — create, verify all fields, attach to session
    let volume = store.create_volume(user.id, 512 * 1024 * 1024).await.unwrap();
    assert_eq!(volume.user_id, user.id);
    assert_eq!(volume.size_bytes, 536_870_912);
    assert!(volume.session_id.is_none());

    let attached_volume = store.attach_volume(volume.id, session.id).await.unwrap();
    assert_eq!(attached_volume.session_id, Some(session.id));

    // 4. Artifact — put with memory operator, verify all fields
    let operator = memory_operator();
    let data = Bytes::from(vec![0u8; 1024]); // 1 KB
    let artifact = store
        .put_artifact(&operator, session.id, user.id, "roundtrip/data.bin", data, Some("application/octet-stream"))
        .await
        .unwrap();
    assert_eq!(artifact.size_bytes, 1024);
    assert_eq!(artifact.content_type.as_deref(), Some("application/octet-stream"));
    assert!(artifact.checksum.is_some());

    // 5. SessionEvent — all three event types
    let msg_event = store
        .append_event(NewSessionEvent {
            session_id: session.id,
            event_type: SessionEventType::Message,
            role: Some("assistant".to_string()),
            content: "I will help you.".to_string(),
        })
        .await
        .unwrap();

    let _tool_call = store
        .append_event(NewSessionEvent {
            session_id: session.id,
            event_type: SessionEventType::ToolCall,
            role: None,
            content: r#"{"name":"read_file","input":{"path":"/etc/hostname"}}"#.to_string(),
        })
        .await
        .unwrap();

    let _tool_result = store
        .append_event(NewSessionEvent {
            session_id: session.id,
            event_type: SessionEventType::ToolResult,
            role: None,
            content: "dimension-worker-01".to_string(),
        })
        .await
        .unwrap();

    // Verify all events came back correctly
    let all_events = store.get_events(session.id, None).await.unwrap();
    assert_eq!(all_events.len(), 3);
    assert_eq!(all_events[0].id, msg_event.id);
    assert_eq!(all_events[0].role.as_deref(), Some("assistant"));

    // Verify volume detach clears session_id
    store.detach_volume(volume.id).await.unwrap();
    let detached = store.get_volume(volume.id).await.unwrap().unwrap();
    assert!(detached.session_id.is_none(), "detached volume should have no session_id");

    teardown(&schema).await;
}
