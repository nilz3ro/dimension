//! VaultClient: authenticated Vault client with KV v2, Transit, VM tokens, and health check.

use std::sync::Arc;
use tokio::sync::RwLock;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};

use crate::vault::auth::{approle_login, token_renewal_loop};
use crate::vault::config::VaultConfig;

/// Error type for Vault operations.
#[derive(Debug)]
pub enum VaultError {
    /// AppRole credentials not configured (role_id or secret_id missing).
    NotConfigured,
    /// Authentication failed -- response missing auth.client_token.
    AuthFailed,
    /// HTTP request failed (network error, DNS, etc).
    RequestFailed(reqwest::Error),
    /// Vault API returned a non-2xx status.
    ApiError { status: u16, message: String },
    /// Transit encrypt/decrypt operation failed.
    TransitFailed,
    /// Token creation failed.
    TokenCreateFailed,
    /// Policy creation/update failed.
    PolicyFailed,
}

impl std::fmt::Display for VaultError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VaultError::NotConfigured => {
                write!(f, "Vault not configured: VAULT_ROLE_ID and VAULT_SECRET_ID must be set")
            }
            VaultError::AuthFailed => write!(f, "Vault AppRole authentication failed: missing client_token in response"),
            VaultError::RequestFailed(e) => write!(f, "Vault HTTP request failed: {e}"),
            VaultError::ApiError { status, message } => {
                write!(f, "Vault API error {status}: {message}")
            }
            VaultError::TransitFailed => write!(f, "Vault Transit encrypt/decrypt failed"),
            VaultError::TokenCreateFailed => write!(f, "Vault token creation failed"),
            VaultError::PolicyFailed => write!(f, "Vault policy create/update failed"),
        }
    }
}

impl std::error::Error for VaultError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            VaultError::RequestFailed(e) => Some(e),
            _ => None,
        }
    }
}

/// Health status returned by `check_health`.
#[derive(Debug, PartialEq)]
pub enum VaultHealthStatus {
    /// Vault is reachable and healthy.
    Ok,
    /// Vault is unreachable or returned a non-200 status.
    Degraded(String),
}

/// Authenticated client for HashiCorp Vault.
///
/// Created via `VaultClient::connect`, which performs AppRole login and
/// spawns a background token renewal loop.
pub struct VaultClient {
    http: reqwest::Client,
    base_url: String,
    token: Arc<RwLock<String>>,
    pub config: VaultConfig,
}

impl VaultClient {
    /// Connect to Vault using AppRole authentication.
    ///
    /// Returns `VaultError::NotConfigured` if `vault_role_id` or `vault_secret_id` is None.
    /// On success, spawns a background token renewal loop and returns the client.
    pub async fn connect(config: VaultConfig) -> Result<Self, VaultError> {
        // Fail early if credentials are not configured.
        if config.vault_role_id.is_none() || config.vault_secret_id.is_none() {
            return Err(VaultError::NotConfigured);
        }

        let http = reqwest::Client::new();
        let initial_token = approle_login(&http, &config).await?;
        let token = Arc::new(RwLock::new(initial_token));

        // Spawn the background renewal loop.
        let renewal_http = http.clone();
        let renewal_vault_url = config.vault_url.clone();
        let renewal_token = token.clone();
        let renewal_interval = config.vault_renewal_interval_secs;
        tokio::spawn(async move {
            token_renewal_loop(renewal_http, renewal_vault_url, renewal_token, renewal_interval)
                .await;
        });

        Ok(Self {
            http,
            base_url: config.vault_url.clone(),
            token,
            config,
        })
    }

    /// Read the current Vault token.
    async fn current_token(&self) -> String {
        self.token.read().await.clone()
    }

    /// Write a secret value at the given KV v2 path.
    ///
    /// Path is under `secret/data/` (KV v2 data endpoint).
    pub async fn kv_write(&self, path: &str, value: &str) -> Result<(), VaultError> {
        let url = format!("{}/v1/secret/data/{}", self.base_url, path);
        let body = serde_json::json!({ "data": { "value": value } });
        let token = self.current_token().await;

        let resp = self
            .http
            .post(&url)
            .header("X-Vault-Token", &token)
            .json(&body)
            .send()
            .await
            .map_err(VaultError::RequestFailed)?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let message = resp.text().await.unwrap_or_else(|_| "unknown".to_string());
            return Err(VaultError::ApiError { status, message });
        }

        Ok(())
    }

    /// Read a secret value at the given KV v2 path.
    ///
    /// Returns `Ok(None)` on 404 (secret does not exist).
    /// Extracts `data.data.value` from the KV v2 response.
    pub async fn kv_read(&self, path: &str) -> Result<Option<String>, VaultError> {
        let url = format!("{}/v1/secret/data/{}", self.base_url, path);
        let token = self.current_token().await;

        let resp = self
            .http
            .get(&url)
            .header("X-Vault-Token", &token)
            .send()
            .await
            .map_err(VaultError::RequestFailed)?;

        if resp.status().as_u16() == 404 {
            return Ok(None);
        }

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let message = resp.text().await.unwrap_or_else(|_| "unknown".to_string());
            return Err(VaultError::ApiError { status, message });
        }

        let json: serde_json::Value = resp.json().await.map_err(VaultError::RequestFailed)?;
        let value = json
            .get("data")
            .and_then(|d| d.get("data"))
            .and_then(|d| d.get("value"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        Ok(value)
    }

    /// Delete a secret and all its versions at the given KV v2 path.
    ///
    /// Uses the `metadata` endpoint (not `data`) to permanently delete all versions.
    pub async fn kv_delete(&self, path: &str) -> Result<(), VaultError> {
        let url = format!("{}/v1/secret/metadata/{}", self.base_url, path);
        let token = self.current_token().await;

        let resp = self
            .http
            .delete(&url)
            .header("X-Vault-Token", &token)
            .send()
            .await
            .map_err(VaultError::RequestFailed)?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let message = resp.text().await.unwrap_or_else(|_| "unknown".to_string());
            return Err(VaultError::ApiError { status, message });
        }

        Ok(())
    }

    /// List secret names at the given path prefix.
    ///
    /// Uses a LIST request to `secret/metadata/{path}`.
    /// Returns an empty vec on 404 (no secrets at path).
    pub async fn kv_list(&self, path: &str) -> Result<Vec<String>, VaultError> {
        let url = format!("{}/v1/secret/metadata/{}", self.base_url, path);
        let token = self.current_token().await;

        let resp = self
            .http
            .request(reqwest::Method::from_bytes(b"LIST").unwrap(), &url)
            .header("X-Vault-Token", &token)
            .send()
            .await
            .map_err(VaultError::RequestFailed)?;

        if resp.status().as_u16() == 404 {
            return Ok(vec![]);
        }

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let message = resp.text().await.unwrap_or_else(|_| "unknown".to_string());
            return Err(VaultError::ApiError { status, message });
        }

        let json: serde_json::Value = resp.json().await.map_err(VaultError::RequestFailed)?;
        let keys = json
            .get("data")
            .and_then(|d| d.get("keys"))
            .and_then(|k| k.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        Ok(keys)
    }

    /// Encrypt a plaintext value using the named Transit key.
    ///
    /// CRITICAL: base64-encodes the plaintext before sending to Vault.
    /// Returns the Vault ciphertext string.
    pub async fn transit_encrypt(
        &self,
        key_name: &str,
        plaintext: &str,
    ) -> Result<String, VaultError> {
        let url = format!("{}/v1/transit/encrypt/{}", self.base_url, key_name);
        let token = self.current_token().await;

        // Vault Transit requires base64-encoded plaintext.
        let encoded = BASE64.encode(plaintext.as_bytes());
        let body = serde_json::json!({ "plaintext": encoded });

        let resp = self
            .http
            .post(&url)
            .header("X-Vault-Token", &token)
            .json(&body)
            .send()
            .await
            .map_err(VaultError::RequestFailed)?;

        if !resp.status().is_success() {
            return Err(VaultError::TransitFailed);
        }

        let json: serde_json::Value = resp.json().await.map_err(VaultError::RequestFailed)?;
        let ciphertext = json
            .get("data")
            .and_then(|d| d.get("ciphertext"))
            .and_then(|c| c.as_str())
            .ok_or(VaultError::TransitFailed)?;

        Ok(ciphertext.to_string())
    }

    /// Decrypt a ciphertext using the named Transit key.
    ///
    /// Decodes the base64-encoded plaintext in the Vault response to get the original string.
    pub async fn transit_decrypt(
        &self,
        key_name: &str,
        ciphertext: &str,
    ) -> Result<String, VaultError> {
        let url = format!("{}/v1/transit/decrypt/{}", self.base_url, key_name);
        let token = self.current_token().await;
        let body = serde_json::json!({ "ciphertext": ciphertext });

        let resp = self
            .http
            .post(&url)
            .header("X-Vault-Token", &token)
            .json(&body)
            .send()
            .await
            .map_err(VaultError::RequestFailed)?;

        if !resp.status().is_success() {
            return Err(VaultError::TransitFailed);
        }

        let json: serde_json::Value = resp.json().await.map_err(VaultError::RequestFailed)?;
        let b64_plaintext = json
            .get("data")
            .and_then(|d| d.get("plaintext"))
            .and_then(|p| p.as_str())
            .ok_or(VaultError::TransitFailed)?;

        let decoded = BASE64
            .decode(b64_plaintext)
            .map_err(|_| VaultError::TransitFailed)?;

        String::from_utf8(decoded).map_err(|_| VaultError::TransitFailed)
    }

    /// Create (or confirm existence of) a named Transit encryption key.
    ///
    /// Idempotent: if the key already exists, Vault returns 200 with no error.
    pub async fn create_transit_key(&self, key_name: &str) -> Result<(), VaultError> {
        let url = format!("{}/v1/transit/keys/{}", self.base_url, key_name);
        let token = self.current_token().await;

        let resp = self
            .http
            .post(&url)
            .header("X-Vault-Token", &token)
            .json(&serde_json::json!({}))
            .send()
            .await
            .map_err(VaultError::RequestFailed)?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let message = resp.text().await.unwrap_or_else(|_| "unknown".to_string());
            return Err(VaultError::ApiError { status, message });
        }

        Ok(())
    }

    /// Mint a scoped VM token with the given bundle path and TTL.
    ///
    /// Creates a policy named `dimension-bundle-{bundle_path with / replaced by -}`
    /// and mints a token with that policy. Returns the client token string.
    pub async fn create_vm_token(
        &self,
        bundle_path: &str,
        ttl_secs: u64,
    ) -> Result<String, VaultError> {
        let url = format!("{}/v1/auth/token/create", self.base_url);
        let token = self.current_token().await;

        let policy_name = format!("dimension-bundle-{}", bundle_path.replace('/', "-"));
        let ttl = format!("{ttl_secs}s");
        let display_name = format!("vm-{bundle_path}");

        let body = serde_json::json!({
            "policies": [policy_name],
            "ttl": ttl,
            "renewable": false,
            "display_name": display_name,
        });

        let resp = self
            .http
            .post(&url)
            .header("X-Vault-Token", &token)
            .json(&body)
            .send()
            .await
            .map_err(VaultError::RequestFailed)?;

        if !resp.status().is_success() {
            return Err(VaultError::TokenCreateFailed);
        }

        let json: serde_json::Value = resp.json().await.map_err(VaultError::RequestFailed)?;
        let client_token = json
            .get("auth")
            .and_then(|a| a.get("client_token"))
            .and_then(|t| t.as_str())
            .ok_or(VaultError::TokenCreateFailed)?;

        Ok(client_token.to_string())
    }

    /// Revoke a Vault token by its value.
    ///
    /// Retries up to 3 times on failure -- the token's TTL is the safety net
    /// if all retries fail.
    pub async fn revoke_token(&self, target_token: &str) -> Result<(), VaultError> {
        let url = format!("{}/v1/auth/token/revoke", self.base_url);
        let token = self.current_token().await;
        let body = serde_json::json!({ "token": target_token });

        for attempt in 1..=3u32 {
            match self
                .http
                .post(&url)
                .header("X-Vault-Token", &token)
                .json(&body)
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => return Ok(()),
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    tracing::error!(
                        attempt = attempt,
                        status = status,
                        "vault token revocation failed with non-2xx status"
                    );
                }
                Err(e) => {
                    tracing::error!(
                        attempt = attempt,
                        error = %e,
                        "vault token revocation request failed"
                    );
                }
            }

            if attempt < 3 {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }

        Err(VaultError::TokenCreateFailed)
    }

    /// Create or update an ACL policy with the given HCL content.
    pub async fn create_policy(&self, name: &str, policy_hcl: &str) -> Result<(), VaultError> {
        let url = format!("{}/v1/sys/policies/acl/{}", self.base_url, name);
        let token = self.current_token().await;
        let body = serde_json::json!({ "policy": policy_hcl });

        let resp = self
            .http
            .put(&url)
            .header("X-Vault-Token", &token)
            .json(&body)
            .send()
            .await
            .map_err(VaultError::RequestFailed)?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let message = resp.text().await.unwrap_or_else(|_| "unknown".to_string());
            return Err(VaultError::ApiError { status, message });
        }

        Ok(())
    }

    /// Check Vault health.
    ///
    /// GETs `/v1/sys/health`. Returns `VaultHealthStatus::Ok` on 200.
    /// Returns `VaultHealthStatus::Degraded(reason)` on non-200 or connection error.
    pub async fn check_health(&self) -> VaultHealthStatus {
        let url = format!("{}/v1/sys/health", self.base_url);

        match self.http.get(&url).send().await {
            Ok(resp) if resp.status().as_u16() == 200 => VaultHealthStatus::Ok,
            Ok(resp) => {
                VaultHealthStatus::Degraded(format!("http status {}", resp.status().as_u16()))
            }
            Err(e) => VaultHealthStatus::Degraded(format!("connection error: {e}")),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Unit tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, Json as AxumJson};
    use axum::routing::{get, post, put, delete};
    use axum::extract::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    // ── Helper: build VaultConfig pointing at the mock server ──────────

    fn mock_config(base_url: &str) -> VaultConfig {
        VaultConfig {
            vault_url: base_url.to_string(),
            vault_role_id: Some("test-role-id".to_string()),
            vault_secret_id: Some("test-secret-id".to_string()),
            vault_renewal_interval_secs: 900,
            vault_vm_token_ttl_secs: 3600,
        }
    }

    /// Spin up an axum mock server on a random port and return (base_url, server_handle).
    async fn start_mock_server(router: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        format!("http://127.0.0.1:{}", addr.port())
    }

    /// Build a VaultClient backed by a mock server that handles AppRole login.
    /// The mock router must include the login endpoint.
    async fn build_client_with_mock(router: Router) -> VaultClient {
        let base_url = start_mock_server(router).await;
        let config = mock_config(&base_url);
        VaultClient::connect(config).await.unwrap()
    }

    // ── Router helpers ─────────────────────────────────────────────────

    /// A login endpoint that always succeeds.
    async fn login_handler() -> AxumJson<serde_json::Value> {
        AxumJson(serde_json::json!({
            "auth": { "client_token": "test-vault-token" }
        }))
    }

    /// A login endpoint that returns a response missing auth.client_token.
    async fn login_missing_token_handler() -> AxumJson<serde_json::Value> {
        AxumJson(serde_json::json!({ "auth": {} }))
    }

    /// A renewal endpoint that always succeeds.
    async fn renew_handler() -> AxumJson<serde_json::Value> {
        AxumJson(serde_json::json!({ "auth": { "client_token": "renewed-token" } }))
    }

    // ── approle_login tests ────────────────────────────────────────────

    #[tokio::test]
    async fn test_approle_login_success() {
        let router = Router::new()
            .route("/v1/auth/approle/login", post(login_handler));
        let base_url = start_mock_server(router).await;
        let config = mock_config(&base_url);
        let http = reqwest::Client::new();
        let token = approle_login(&http, &config).await.unwrap();
        assert_eq!(token, "test-vault-token");
    }

    #[tokio::test]
    async fn test_approle_login_missing_token_returns_auth_failed() {
        let router = Router::new()
            .route("/v1/auth/approle/login", post(login_missing_token_handler));
        let base_url = start_mock_server(router).await;
        let config = mock_config(&base_url);
        let http = reqwest::Client::new();
        let result = approle_login(&http, &config).await;
        assert!(matches!(result, Err(VaultError::AuthFailed)));
    }

    #[tokio::test]
    async fn test_approle_login_not_configured() {
        let config = VaultConfig {
            vault_url: "http://unused".to_string(),
            vault_role_id: None,
            vault_secret_id: None,
            vault_renewal_interval_secs: 900,
            vault_vm_token_ttl_secs: 3600,
        };
        let http = reqwest::Client::new();
        let result = approle_login(&http, &config).await;
        assert!(matches!(result, Err(VaultError::NotConfigured)));
    }

    // ── connect returns NotConfigured ──────────────────────────────────

    #[tokio::test]
    async fn test_connect_not_configured() {
        let config = VaultConfig {
            vault_url: "http://unused".to_string(),
            vault_role_id: None,
            vault_secret_id: Some("secret".to_string()),
            vault_renewal_interval_secs: 900,
            vault_vm_token_ttl_secs: 3600,
        };
        let result = VaultClient::connect(config).await;
        assert!(matches!(result, Err(VaultError::NotConfigured)));
    }

    // ── token_renewal_loop test ────────────────────────────────────────

    #[tokio::test]
    async fn test_token_renewal_loop_fires() {
        let renewal_count = Arc::new(AtomicUsize::new(0));
        let renewal_count_clone = renewal_count.clone();

        let router = Router::new()
            .route("/v1/auth/approle/login", post(login_handler))
            .route(
                "/v1/auth/token/renew-self",
                post(move || {
                    let count = renewal_count_clone.clone();
                    async move {
                        count.fetch_add(1, Ordering::SeqCst);
                        AxumJson(serde_json::json!({ "auth": { "client_token": "renewed" } }))
                    }
                }),
            );

        let base_url = start_mock_server(router).await;
        let http = reqwest::Client::new();
        let token = Arc::new(RwLock::new("initial-token".to_string()));
        let token_clone = token.clone();

        // Use 100ms interval for testing.
        tokio::spawn(async move {
            token_renewal_loop(http, base_url, token_clone, 0).await;
        });

        // Wait up to 500ms for at least one renewal.
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
        while std::time::Instant::now() < deadline {
            if renewal_count.load(Ordering::SeqCst) > 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        assert!(
            renewal_count.load(Ordering::SeqCst) > 0,
            "renewal loop should have fired at least once within 500ms"
        );
    }

    // ── kv_write tests ─────────────────────────────────────────────────

    #[tokio::test]
    async fn test_kv_write_success() {
        let router = Router::new()
            .route("/v1/auth/approle/login", post(login_handler))
            .route(
                "/v1/secret/data/{*path}",
                post(|| async { AxumJson(serde_json::json!({ "data": { "version": 1 } })) }),
            );
        let client = build_client_with_mock(router).await;
        let result = client.kv_write("dimension/user1/bundle1/mykey", "myvalue").await;
        assert!(result.is_ok());
    }

    // ── kv_read tests ──────────────────────────────────────────────────

    #[tokio::test]
    async fn test_kv_read_success() {
        let router = Router::new()
            .route("/v1/auth/approle/login", post(login_handler))
            .route(
                "/v1/secret/data/{*path}",
                get(|| async {
                    AxumJson(serde_json::json!({
                        "data": { "data": { "value": "hello-secret" } }
                    }))
                }),
            );
        let client = build_client_with_mock(router).await;
        let result = client.kv_read("dimension/user1/bundle1/mykey").await.unwrap();
        assert_eq!(result, Some("hello-secret".to_string()));
    }

    #[tokio::test]
    async fn test_kv_read_not_found_returns_none() {
        use axum::http::StatusCode;
        let router = Router::new()
            .route("/v1/auth/approle/login", post(login_handler))
            .route(
                "/v1/secret/data/{*path}",
                get(|| async { StatusCode::NOT_FOUND }),
            );
        let client = build_client_with_mock(router).await;
        let result = client.kv_read("dimension/user1/bundle1/missing").await.unwrap();
        assert_eq!(result, None);
    }

    // ── kv_delete tests ────────────────────────────────────────────────

    #[tokio::test]
    async fn test_kv_delete_uses_metadata_path() {
        // Track that the request hits /v1/secret/metadata/... (not /data/)
        let hit_metadata = Arc::new(AtomicUsize::new(0));
        let hit_metadata_clone = hit_metadata.clone();

        let router = Router::new()
            .route("/v1/auth/approle/login", post(login_handler))
            .route(
                "/v1/secret/metadata/{*path}",
                delete(move || {
                    let count = hit_metadata_clone.clone();
                    async move {
                        count.fetch_add(1, Ordering::SeqCst);
                        axum::http::StatusCode::NO_CONTENT
                    }
                }),
            );

        let client = build_client_with_mock(router).await;
        let result = client.kv_delete("dimension/user1/bundle1/mykey").await;
        assert!(result.is_ok(), "kv_delete should succeed: {result:?}");
        assert_eq!(
            hit_metadata.load(Ordering::SeqCst),
            1,
            "DELETE should hit metadata endpoint"
        );
    }

    // ── kv_list tests ──────────────────────────────────────────────────

    #[tokio::test]
    async fn test_kv_list_returns_keys() {
        let router = Router::new()
            .route("/v1/auth/approle/login", post(login_handler))
            .route(
                "/v1/secret/metadata/{*path}",
                axum::routing::any(|| async {
                    AxumJson(serde_json::json!({
                        "data": { "keys": ["key1", "key2", "key3"] }
                    }))
                }),
            );
        let client = build_client_with_mock(router).await;
        let keys = client.kv_list("dimension/user1/bundle1").await.unwrap();
        assert_eq!(keys, vec!["key1", "key2", "key3"]);
    }

    #[tokio::test]
    async fn test_kv_list_empty_on_404() {
        use axum::http::StatusCode;
        let router = Router::new()
            .route("/v1/auth/approle/login", post(login_handler))
            .route(
                "/v1/secret/metadata/{*path}",
                axum::routing::any(|| async { StatusCode::NOT_FOUND }),
            );
        let client = build_client_with_mock(router).await;
        let keys = client.kv_list("dimension/user1/bundle1").await.unwrap();
        assert!(keys.is_empty());
    }

    // ── transit_encrypt / transit_decrypt tests ────────────────────────

    #[tokio::test]
    async fn test_transit_encrypt_base64_encodes_plaintext() {
        let received_plaintext = Arc::new(tokio::sync::Mutex::new(String::new()));
        let received_clone = received_plaintext.clone();

        let router = Router::new()
            .route("/v1/auth/approle/login", post(login_handler))
            .route(
                "/v1/transit/encrypt/{key_name}",
                post(move |axum::extract::Json(body): axum::extract::Json<serde_json::Value>| {
                    let received = received_clone.clone();
                    async move {
                        let pt = body["plaintext"].as_str().unwrap_or("").to_string();
                        *received.lock().await = pt;
                        AxumJson(serde_json::json!({
                            "data": { "ciphertext": "vault:v1:dGVzdA==" }
                        }))
                    }
                }),
            );

        let client = build_client_with_mock(router).await;
        let result = client.transit_encrypt("mykey", "hello").await.unwrap();
        assert_eq!(result, "vault:v1:dGVzdA==");

        // Verify the plaintext sent was base64-encoded
        let sent = received_plaintext.lock().await.clone();
        let decoded = BASE64.decode(&sent).unwrap();
        assert_eq!(String::from_utf8(decoded).unwrap(), "hello");
    }

    #[tokio::test]
    async fn test_transit_decrypt_decodes_base64() {
        let router = Router::new()
            .route("/v1/auth/approle/login", post(login_handler))
            .route(
                "/v1/transit/decrypt/{key_name}",
                post(|| async {
                    // Return base64("world")
                    AxumJson(serde_json::json!({
                        "data": { "plaintext": "d29ybGQ=" }
                    }))
                }),
            );

        let client = build_client_with_mock(router).await;
        let result = client.transit_decrypt("mykey", "vault:v1:xyz").await.unwrap();
        assert_eq!(result, "world");
    }

    #[tokio::test]
    async fn test_transit_roundtrip() {
        // Simulate a full encrypt->decrypt roundtrip.
        let router = Router::new()
            .route("/v1/auth/approle/login", post(login_handler))
            .route(
                "/v1/transit/encrypt/{key_name}",
                post(|axum::extract::Json(body): axum::extract::Json<serde_json::Value>| async move {
                    // Echo the plaintext as "ciphertext" for test
                    let pt = body["plaintext"].as_str().unwrap_or("").to_string();
                    let fake_ct = format!("ct:{pt}");
                    AxumJson(serde_json::json!({ "data": { "ciphertext": fake_ct } }))
                }),
            )
            .route(
                "/v1/transit/decrypt/{key_name}",
                post(|axum::extract::Json(body): axum::extract::Json<serde_json::Value>| async move {
                    let ct = body["ciphertext"].as_str().unwrap_or("").to_string();
                    // Strip "ct:" prefix to get back the base64-encoded plaintext
                    let b64 = ct.strip_prefix("ct:").unwrap_or("").to_string();
                    AxumJson(serde_json::json!({ "data": { "plaintext": b64 } }))
                }),
            );

        let client = build_client_with_mock(router).await;
        let original = "my-secret-value";
        let ciphertext = client.transit_encrypt("mykey", original).await.unwrap();
        let decrypted = client.transit_decrypt("mykey", &ciphertext).await.unwrap();
        assert_eq!(decrypted, original);
    }

    // ── create_transit_key tests ───────────────────────────────────────

    #[tokio::test]
    async fn test_create_transit_key() {
        let router = Router::new()
            .route("/v1/auth/approle/login", post(login_handler))
            .route(
                "/v1/transit/keys/{key_name}",
                post(|| async { axum::http::StatusCode::OK }),
            );
        let client = build_client_with_mock(router).await;
        let result = client.create_transit_key("dimension-user1-bundle1").await;
        assert!(result.is_ok());
    }

    // ── create_vm_token tests ──────────────────────────────────────────

    #[tokio::test]
    async fn test_create_vm_token() {
        let received_body = Arc::new(tokio::sync::Mutex::new(serde_json::Value::Null));
        let received_clone = received_body.clone();

        let router = Router::new()
            .route("/v1/auth/approle/login", post(login_handler))
            .route(
                "/v1/auth/token/create",
                post(move |axum::extract::Json(body): axum::extract::Json<serde_json::Value>| {
                    let received = received_clone.clone();
                    async move {
                        *received.lock().await = body;
                        AxumJson(serde_json::json!({
                            "auth": { "client_token": "vm-scoped-token" }
                        }))
                    }
                }),
            );

        let client = build_client_with_mock(router).await;
        let token = client.create_vm_token("user1/bundle1", 3600).await.unwrap();
        assert_eq!(token, "vm-scoped-token");

        let body = received_body.lock().await;
        assert_eq!(body["ttl"], "3600s");
        assert_eq!(body["renewable"], false);
        assert!(body["policies"].as_array().unwrap().contains(&serde_json::json!("dimension-bundle-user1-bundle1")));
    }

    // ── revoke_token tests ─────────────────────────────────────────────

    #[tokio::test]
    async fn test_revoke_token_success() {
        let router = Router::new()
            .route("/v1/auth/approle/login", post(login_handler))
            .route(
                "/v1/auth/token/revoke",
                post(|| async { axum::http::StatusCode::NO_CONTENT }),
            );
        let client = build_client_with_mock(router).await;
        let result = client.revoke_token("some-token").await;
        assert!(result.is_ok());
    }

    // ── create_policy tests ────────────────────────────────────────────

    #[tokio::test]
    async fn test_create_policy() {
        let received_body = Arc::new(tokio::sync::Mutex::new(serde_json::Value::Null));
        let received_clone = received_body.clone();

        let router = Router::new()
            .route("/v1/auth/approle/login", post(login_handler))
            .route(
                "/v1/sys/policies/acl/{name}",
                put(move |
                    _path: axum::extract::Path<String>,
                    axum::extract::Json(body): axum::extract::Json<serde_json::Value>
                | {
                    let received = received_clone.clone();
                    async move {
                        *received.lock().await = body;
                        axum::http::StatusCode::OK
                    }
                }),
            );

        let client = build_client_with_mock(router).await;
        let hcl = r#"path "secret/*" { capabilities = ["read"] }"#;
        let result = client.create_policy("test-policy", hcl).await;
        assert!(result.is_ok(), "create_policy should succeed: {result:?}");

        let body = received_body.lock().await;
        assert_eq!(body["policy"], hcl);
    }

    // ── check_health tests ─────────────────────────────────────────────

    #[tokio::test]
    async fn test_check_health_ok() {
        let router = Router::new()
            .route("/v1/auth/approle/login", post(login_handler))
            .route(
                "/v1/sys/health",
                get(|| async { axum::http::StatusCode::OK }),
            );
        let client = build_client_with_mock(router).await;
        let status = client.check_health().await;
        assert_eq!(status, VaultHealthStatus::Ok);
    }

    #[tokio::test]
    async fn test_check_health_degraded_on_non_200() {
        let router = Router::new()
            .route("/v1/auth/approle/login", post(login_handler))
            .route(
                "/v1/sys/health",
                get(|| async { axum::http::StatusCode::SERVICE_UNAVAILABLE }),
            );
        let client = build_client_with_mock(router).await;
        let status = client.check_health().await;
        assert!(matches!(status, VaultHealthStatus::Degraded(_)));
    }

    #[tokio::test]
    async fn test_check_health_degraded_on_connection_error() {
        // Point to a port that is not listening.
        let config = VaultConfig {
            vault_url: "http://127.0.0.1:1".to_string(),
            vault_role_id: Some("r".to_string()),
            vault_secret_id: Some("s".to_string()),
            vault_renewal_interval_secs: 900,
            vault_vm_token_ttl_secs: 3600,
        };
        let http = reqwest::Client::new();
        let token = Arc::new(RwLock::new("dummy".to_string()));
        let client = VaultClient {
            http,
            base_url: config.vault_url.clone(),
            token,
            config,
        };
        let status = client.check_health().await;
        assert!(matches!(status, VaultHealthStatus::Degraded(_)));
    }
}
