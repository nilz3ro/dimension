//! `dimension login` — authenticate and persist API key.
//!
//! Validates the supplied (or interactively prompted) API key by hitting the
//! gateway's admin health endpoint.  Only saves the key to
//! `~/.dimension/credentials` on successful authentication.

use anyhow::{Context, Result};
use reqwest::Client;
use std::time::Duration;

use crate::credentials;

/// Run the login flow.
///
/// 1. Obtain the API key (from `--api-key` flag or interactive prompt).
/// 2. Validate it against `GET <gateway>/admin/health` with bearer auth.
/// 3. On success, persist to `~/.dimension/credentials`.
pub async fn run(gateway_url: &str, api_key: Option<String>) -> Result<()> {
    let key = match api_key {
        Some(k) if !k.is_empty() => k,
        _ => credentials::prompt_api_key()?,
    };

    eprintln!("Validating API key against {gateway_url} …");

    validate_key(gateway_url, &key)
        .await
        .context("API key validation failed")?;

    credentials::write_api_key(&key)?;
    eprintln!("✔ Credentials saved to ~/.dimension/credentials");

    Ok(())
}

/// Validate the API key by making an authenticated request to the gateway.
///
/// Uses `GET /admin/health` — a lightweight endpoint that requires bearer auth.
/// Returns `Ok(())` on HTTP 200, an error otherwise.
async fn validate_key(gateway_url: &str, key: &str) -> Result<()> {
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .context("failed to build HTTP client")?;

    let url = format!("{gateway_url}/admin/health");
    let resp = client
        .get(&url)
        .bearer_auth(key)
        .send()
        .await
        .context("could not reach gateway — is it running?")?;

    if resp.status().is_success() {
        return Ok(());
    }

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    anyhow::bail!(
        "Gateway returned HTTP {status}: {body}\n\n\
         Make sure the API key is valid and the gateway is reachable at {gateway_url}."
    );
}
