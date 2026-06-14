//! AppRole authentication and background token renewal for Vault.

use std::sync::Arc;
use tokio::sync::RwLock;

use crate::vault::client::VaultError;
use crate::vault::config::VaultConfig;

/// Perform an AppRole login and return the client token.
///
/// POSTs to `{vault_url}/v1/auth/approle/login` with role_id and secret_id.
/// Returns `VaultError::AuthFailed` if the response does not contain `auth.client_token`.
pub async fn approle_login(
    http: &reqwest::Client,
    config: &VaultConfig,
) -> Result<String, VaultError> {
    let role_id = config
        .vault_role_id
        .as_deref()
        .ok_or(VaultError::NotConfigured)?;
    let secret_id = config
        .vault_secret_id
        .as_deref()
        .ok_or(VaultError::NotConfigured)?;

    let url = format!("{}/v1/auth/approle/login", config.vault_url);
    let body = serde_json::json!({
        "role_id": role_id,
        "secret_id": secret_id,
    });

    let resp = http
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(VaultError::RequestFailed)?;

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let message = resp
            .text()
            .await
            .unwrap_or_else(|_| "unknown error".to_string());
        return Err(VaultError::ApiError { status, message });
    }

    let json: serde_json::Value = resp.json().await.map_err(VaultError::RequestFailed)?;

    let token = json
        .get("auth")
        .and_then(|a| a.get("client_token"))
        .and_then(|t| t.as_str())
        .ok_or(VaultError::AuthFailed)?;

    Ok(token.to_string())
}

/// Background loop that renews the Vault token at a fixed interval.
///
/// Uses `tokio::time::interval` to fire every `interval_secs` seconds.
/// On each tick, reads the current token and POSTs to `{vault_url}/v1/auth/token/renew-self`.
/// On failure, retries up to 3 times with 5-second delays before giving up on that cycle.
/// Logs success and failure via tracing.
pub async fn token_renewal_loop(
    http: reqwest::Client,
    vault_url: String,
    token: Arc<RwLock<String>>,
    interval_secs: u64,
) {
    // Use at least 1ms to avoid panic on zero interval (e.g., in tests).
    let period = if interval_secs == 0 {
        std::time::Duration::from_millis(50)
    } else {
        std::time::Duration::from_secs(interval_secs)
    };
    let mut interval = tokio::time::interval(period);
    // Skip first tick (fires immediately)
    interval.tick().await;

    loop {
        interval.tick().await;

        let current_token = token.read().await.clone();
        let url = format!("{vault_url}/v1/auth/token/renew-self");

        let mut success = false;
        for attempt in 1..=3u32 {
            match http
                .post(&url)
                .header("X-Vault-Token", &current_token)
                .json(&serde_json::json!({}))
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    tracing::info!("vault token renewed successfully");
                    success = true;
                    break;
                }
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    tracing::error!(
                        attempt = attempt,
                        status = status,
                        "vault token renewal failed with non-2xx status"
                    );
                }
                Err(e) => {
                    tracing::error!(
                        attempt = attempt,
                        error = %e,
                        "vault token renewal request failed"
                    );
                }
            }

            if attempt < 3 {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        }

        if !success {
            tracing::error!("vault token renewal failed after 3 attempts -- will retry next interval");
        }
    }
}
