//! Integration tests for UserStore (PgSessionStore).
//!
//! These tests require a running Postgres instance.
//!
//! Set DATABASE_URL to a Postgres connection string before running:
//!   export DATABASE_URL=postgres://dimension:dimension@localhost/dimension_store
//!   cargo test -p dimension-store --test user_store_tests
//!
//! Each test creates a fresh schema for full isolation.

use std::env;

use uuid::Uuid;

use dimension_store::{
    crypto::hash_api_key, PgSessionStore, StoreError, UserRole, UserStore,
};

/// Create a fresh schema and return a PgSessionStore connected to it.
async fn setup_isolated_store() -> (PgSessionStore, String) {
    let database_url = env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://dimension:dimension@localhost/dimension_store".to_string());

    let schema = format!("test_{}", Uuid::new_v4().simple());
    let url_with_schema = format!("{database_url}?options=-csearch_path%3D{schema}");

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

    let store = PgSessionStore::connect(&url_with_schema)
        .await
        .expect("PgSessionStore::connect failed");

    (store, schema)
}

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

// ── Test 1: create_user returns a valid API key ──────────────────────────────

#[tokio::test]
async fn test_create_user_returns_key() {
    let (store, schema) = setup_isolated_store().await;

    let (user, plaintext_key) = store
        .create_user("alice", UserRole::User)
        .await
        .expect("create_user failed");

    // User fields
    assert_eq!(user.name, "alice");
    assert_eq!(user.role, UserRole::User);
    assert!(user.deleted_at.is_none());

    // Key format: "dim_sk_" + 64 hex chars = 71 chars
    assert!(
        plaintext_key.starts_with("dim_sk_"),
        "key should start with dim_sk_, got: {plaintext_key}"
    );
    assert_eq!(
        plaintext_key.len(),
        71,
        "key should be 71 chars, got {} (key: {plaintext_key})",
        plaintext_key.len()
    );

    teardown_schema(&schema).await;
}

// ── Test 2: authenticate_key returns correct identity ───────────────────────

#[tokio::test]
async fn test_authenticate_key() {
    let (store, schema) = setup_isolated_store().await;

    let (user, plaintext_key) = store
        .create_user("bob", UserRole::Admin)
        .await
        .expect("create_user failed");

    let key_hash = hash_api_key(&plaintext_key);
    let auth = store
        .authenticate_key(&key_hash)
        .await
        .expect("authenticate_key failed");

    assert_eq!(auth.user_id, user.id);
    assert_eq!(auth.name, "bob");
    assert_eq!(auth.role, UserRole::Admin);

    teardown_schema(&schema).await;
}

// ── Test 3: revoked key is rejected ─────────────────────────────────────────

#[tokio::test]
async fn test_revoked_key_rejected() {
    let (store, schema) = setup_isolated_store().await;

    let (user, first_key) = store
        .create_user("charlie", UserRole::User)
        .await
        .expect("create_user failed");

    // Create a second key
    let (second_api_key_meta, second_key) = store
        .create_key(user.id, Some("second"))
        .await
        .expect("create_key failed");

    // Find the first key's ID by authenticating with it
    let first_hash = hash_api_key(&first_key);

    // Revoke first key — we need its ID. Get it from the DB via the hash.
    // Since we can't directly expose the ApiKey ID from create_user, we use create_key result.
    // For this test, revoke the first key using its hash lookup:
    // We need to get the first key's ID. Let's query directly.
    let first_key_row: Option<(Uuid,)> = sqlx_core::query_as::query_as::<_, (Uuid,)>(
        "SELECT id FROM api_keys WHERE key_hash = $1",
    )
    .bind(&first_hash)
    .fetch_optional(store.pool())
    .await
    .expect("query failed");

    let first_key_id = first_key_row.expect("first key not found in DB").0;

    store
        .revoke_key(first_key_id)
        .await
        .expect("revoke_key failed");

    // First key should be rejected
    let result = store.authenticate_key(&first_hash).await;
    assert!(
        matches!(result, Err(StoreError::KeyNotFound)),
        "revoked key should return KeyNotFound, got: {result:?}"
    );

    // Second key should still work
    let second_hash = hash_api_key(&second_key);
    let auth = store
        .authenticate_key(&second_hash)
        .await
        .expect("second key should still authenticate");
    assert_eq!(auth.user_id, user.id);
    let _ = second_api_key_meta;

    teardown_schema(&schema).await;
}

// ── Test 4: deleted user's key is rejected ───────────────────────────────────

#[tokio::test]
async fn test_deleted_user_key_rejected() {
    let (store, schema) = setup_isolated_store().await;

    // Need two admins so we can delete one
    let (_admin2, _) = store
        .create_user("admin2", UserRole::Admin)
        .await
        .expect("create admin2 failed");

    let (user, plaintext_key) = store
        .create_user("dave", UserRole::Admin)
        .await
        .expect("create_user failed");

    store
        .soft_delete_user(user.id)
        .await
        .expect("soft_delete_user failed");

    let key_hash = hash_api_key(&plaintext_key);
    let result = store.authenticate_key(&key_hash).await;
    assert!(
        matches!(result, Err(StoreError::KeyNotFound)),
        "deleted user's key should return KeyNotFound, got: {result:?}"
    );

    teardown_schema(&schema).await;
}

// ── Test 5: soft_delete_user removes from list and get ──────────────────────

#[tokio::test]
async fn test_soft_delete_user() {
    let (store, schema) = setup_isolated_store().await;

    // Need two admins so we can delete one
    let (_admin2, _) = store
        .create_user("admin2", UserRole::Admin)
        .await
        .expect("create admin2 failed");

    let (user, _) = store
        .create_user("eve", UserRole::Admin)
        .await
        .expect("create_user failed");

    store
        .soft_delete_user(user.id)
        .await
        .expect("soft_delete_user failed");

    // get_user returns None
    let found = store.get_user(user.id).await.expect("get_user failed");
    assert!(
        found.is_none(),
        "soft-deleted user should not be returned by get_user"
    );

    // list_users does not include deleted user
    let users = store.list_users().await.expect("list_users failed");
    let ids: Vec<Uuid> = users.iter().map(|u| u.id).collect();
    assert!(
        !ids.contains(&user.id),
        "soft-deleted user should not appear in list_users"
    );

    teardown_schema(&schema).await;
}

// ── Test 6: cannot demote the last admin ────────────────────────────────────

#[tokio::test]
async fn test_last_admin_demote_rejected() {
    let (store, schema) = setup_isolated_store().await;

    let (admin, _) = store
        .create_user("frank", UserRole::Admin)
        .await
        .expect("create_user failed");

    let result = store.demote_user(admin.id).await;
    assert!(
        matches!(result, Err(StoreError::LastAdminDemotion)),
        "demoting last admin should return LastAdminDemotion, got: {result:?}"
    );

    teardown_schema(&schema).await;
}

// ── Test 7: cannot delete the last admin ────────────────────────────────────

#[tokio::test]
async fn test_last_admin_delete_rejected() {
    let (store, schema) = setup_isolated_store().await;

    let (admin, _) = store
        .create_user("grace", UserRole::Admin)
        .await
        .expect("create_user failed");

    let result = store.soft_delete_user(admin.id).await;
    assert!(
        matches!(result, Err(StoreError::LastAdminDemotion)),
        "deleting last admin should return LastAdminDemotion, got: {result:?}"
    );

    teardown_schema(&schema).await;
}

// ── Test 8: promote and demote with multiple admins ─────────────────────────

#[tokio::test]
async fn test_promote_and_demote() {
    let (store, schema) = setup_isolated_store().await;

    // Create one admin and one regular user
    let (admin, _) = store
        .create_user("henry", UserRole::Admin)
        .await
        .expect("create admin failed");
    let (regular, _) = store
        .create_user("iris", UserRole::User)
        .await
        .expect("create regular user failed");

    // Promote regular to admin -> 2 admins
    store
        .promote_user(regular.id)
        .await
        .expect("promote_user failed");
    assert_eq!(
        store.admin_count().await.expect("admin_count failed"),
        2,
        "should have 2 admins after promotion"
    );

    // Demote original admin -> 1 admin remains (should succeed)
    store
        .demote_user(admin.id)
        .await
        .expect("demote_user should succeed when other admin exists");
    assert_eq!(
        store.admin_count().await.expect("admin_count failed"),
        1,
        "should have 1 admin after demotion"
    );

    teardown_schema(&schema).await;
}

// ── Test 9: ensure_bootstrap_admin is idempotent ────────────────────────────

#[tokio::test]
async fn test_bootstrap_admin_idempotent() {
    let (store, schema) = setup_isolated_store().await;

    // First call: creates bootstrap admin and returns a key
    let key = store
        .ensure_bootstrap_admin()
        .await
        .expect("ensure_bootstrap_admin failed");
    assert!(
        key.is_some(),
        "first call should return a key (no users exist)"
    );
    let key = key.unwrap();
    assert!(
        key.starts_with("dim_sk_"),
        "bootstrap key should start with dim_sk_"
    );

    // Second call: users exist, returns None
    let key2 = store
        .ensure_bootstrap_admin()
        .await
        .expect("ensure_bootstrap_admin second call failed");
    assert!(
        key2.is_none(),
        "second call should return None (users already exist)"
    );

    teardown_schema(&schema).await;
}

// ── Test 10: create_additional_key ──────────────────────────────────────────

#[tokio::test]
async fn test_create_additional_key() {
    let (store, schema) = setup_isolated_store().await;

    let (user, _first_key) = store
        .create_user("jack", UserRole::User)
        .await
        .expect("create_user failed");

    // Create a second key with a label
    let (api_key_meta, second_key) = store
        .create_key(user.id, Some("CI"))
        .await
        .expect("create_key failed");

    // Verify metadata
    assert_eq!(api_key_meta.user_id, user.id);
    assert_eq!(api_key_meta.label.as_deref(), Some("CI"));
    assert!(api_key_meta.revoked_at.is_none());

    // Verify second key authenticates
    let second_hash = hash_api_key(&second_key);
    let auth = store
        .authenticate_key(&second_hash)
        .await
        .expect("second key should authenticate");
    assert_eq!(auth.user_id, user.id);
    assert_eq!(auth.name, "jack");

    teardown_schema(&schema).await;
}
