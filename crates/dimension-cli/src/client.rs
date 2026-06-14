//! HTTP client for the Dimension gateway REST API.
//!
//! `GatewayClient` wraps `reqwest::Client` and provides convenience builders
//! for admin and user-scoped endpoints, handling base URL construction and
//! bearer-token authentication automatically.

use std::time::Duration;

use anyhow::{anyhow, Result};
use reqwest::{Client, RequestBuilder, Response};

/// HTTP client for the Dimension gateway API.
///
/// All requests are authenticated via `Authorization: Bearer <token>`.
/// Individual requests should set their own timeouts as needed.
#[derive(Clone)]
pub struct GatewayClient {
    inner: Client,
    base_url: String,
    token: String,
}

impl GatewayClient {
    /// Create a new client targeting `base_url` (e.g. `http://localhost:3000`)
    /// authenticated with `token`.
    pub fn new(base_url: String, token: String) -> Self {
        let inner = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()
            .expect("failed to build reqwest client");
        Self {
            inner,
            base_url,
            token,
        }
    }

    /// Build a GET request to an admin endpoint (`/admin/<path>`).
    pub fn admin_get(&self, path: &str) -> RequestBuilder {
        let url = format!("{}/admin{}", self.base_url, path);
        self.inner.get(url).bearer_auth(&self.token)
    }

    /// Build a POST request to an admin endpoint.
    pub fn admin_post(&self, path: &str) -> RequestBuilder {
        let url = format!("{}/admin{}", self.base_url, path);
        self.inner.post(url).bearer_auth(&self.token)
    }

    /// Build a DELETE request to an admin endpoint.
    pub fn admin_delete(&self, path: &str) -> RequestBuilder {
        let url = format!("{}/admin{}", self.base_url, path);
        self.inner.delete(url).bearer_auth(&self.token)
    }

    /// Build a PUT request to an admin endpoint.
    #[allow(dead_code)]
    pub fn admin_put(&self, path: &str) -> RequestBuilder {
        let url = format!("{}/admin{}", self.base_url, path);
        self.inner.put(url).bearer_auth(&self.token)
    }

    /// Build a GET request to an admin endpoint using a streaming-friendly client with no timeout.
    ///
    /// Use this for SSE / long-lived streaming connections (e.g. `GET /admin/logs/tail`).
    /// The returned [`reqwest::RequestBuilder`] uses a fresh `reqwest::Client` with
    /// `timeout(None)` so the connection is not dropped after 30 seconds.
    pub fn admin_get_streaming(&self, path: &str) -> reqwest::RequestBuilder {
        let url = format!("{}/admin{}", self.base_url, path);
        // Build a new client with no timeout for SSE streams.
        // Creating an extra client per stream is acceptable (streaming is rare / long-lived).
        reqwest::Client::new()
            .get(url)
            .bearer_auth(&self.token)
            .header("Accept", "text/event-stream")
    }

    /// Build a GET request to a user-scoped endpoint (e.g. `/bundles`, `/sessions`).
    pub fn user_get(&self, path: &str) -> RequestBuilder {
        let url = format!("{}{}", self.base_url, path);
        self.inner.get(url).bearer_auth(&self.token)
    }

    /// Build a POST request to a user-scoped endpoint.
    pub fn user_post(&self, path: &str) -> RequestBuilder {
        let url = format!("{}{}", self.base_url, path);
        self.inner.post(url).bearer_auth(&self.token)
    }

    /// Build a DELETE request to a user-scoped endpoint.
    pub fn user_delete(&self, path: &str) -> RequestBuilder {
        let url = format!("{}{}", self.base_url, path);
        self.inner.delete(url).bearer_auth(&self.token)
    }

    /// Send a request and check that the response status is 2xx.
    ///
    /// On non-2xx responses, reads the error body and returns an `anyhow::Error`
    /// containing the HTTP status code and the gateway error message (if any).
    pub async fn check_response(resp: Response) -> Result<Response> {
        if resp.status().is_success() {
            return Ok(resp);
        }
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        // Try to extract a "message" field from JSON error bodies.
        let message = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(str::to_string))
            .unwrap_or(body);
        Err(anyhow!("HTTP {}: {}", status, message))
    }
}
