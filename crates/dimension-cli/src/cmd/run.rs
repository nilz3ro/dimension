//! `dimension run` — dispatch a bundle invocation to a worker.
//!
//! Usage:
//!   dimension run my-agent "Hello world"           # sync, plain text
//!   dimension run my-agent --mode async "Hello"    # async
//!   dimension run my-agent --payload '{"custom":1}'  # raw JSON payload
//!   dimension run my-agent "Continue" --session s1   # with session ID

use anyhow::{Context, Result};
use clap::Args;
use serde::Deserialize;

use crate::client::GatewayClient;

/// Run a bundle invocation on the compute platform.
#[derive(Debug, Args)]
pub struct RunCmd {
    /// Bundle name or numeric ID to run
    pub bundle: String,

    /// Message to send to the agent (plain text, auto-wrapped in JSON envelope)
    pub message: Option<String>,

    /// Execution mode: sync or async (default: sync)
    #[arg(long, default_value = "sync")]
    pub mode: String,

    /// Session ID for conversation continuity
    #[arg(long)]
    pub session: Option<String>,

    /// Raw JSON payload (bypasses auto-wrapping; for advanced use)
    #[arg(long)]
    pub payload: Option<String>,
}

/// Sync-mode response (200).
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct RunSyncResponse {
    invocation_id: String,
    stdout: String,
    #[allow(dead_code)]
    exit_code: i32,
}

/// Async/persistent-mode response (202).
#[derive(Debug, Deserialize)]
struct RunAsyncResponse {
    invocation_id: String,
    #[allow(dead_code)]
    status: String,
}

/// Build the JSON envelope that agents expect on stdin.
fn build_agent_payload(message: &str, bundle: &str, session: Option<&str>) -> serde_json::Value {
    let session_id = session.unwrap_or("cli");
    serde_json::json!({
        "role": "user",
        "content": [{"type": "text", "text": message}],
        "session_id": session_id,
        "bundle_id": bundle,
        "history": [],
        "truncation": {
            "total_messages": 1,
            "included_messages": 1,
            "truncated": false
        }
    })
}

/// Resolve the payload: plain text message → envelope, or raw JSON passthrough.
fn resolve_payload(cmd: &RunCmd) -> Result<serde_json::Value> {
    // If --payload is given, use it as raw JSON (advanced mode).
    if let Some(ref raw) = cmd.payload {
        if raw.starts_with('@') {
            let path = &raw[1..];
            let data = std::fs::read_to_string(path)
                .with_context(|| format!("failed to read payload file: {path}"))?;
            return serde_json::from_str(&data)
                .with_context(|| format!("payload file '{path}' is not valid JSON"));
        }
        return serde_json::from_str(raw).context("--payload is not valid JSON");
    }

    // Otherwise, wrap the plain text message in the agent envelope.
    match cmd.message.as_deref() {
        Some(msg) => Ok(build_agent_payload(msg, &cmd.bundle, cmd.session.as_deref())),
        None => anyhow::bail!("provide a message: dimension run <bundle> \"your message\""),
    }
}

/// Execute the run command.
pub async fn run(cmd: RunCmd, client: &GatewayClient) -> Result<()> {
    let payload = resolve_payload(&cmd)?;

    let body = serde_json::json!({
        "bundle_id": cmd.bundle,
        "mode": cmd.mode,
        "payload": payload,
    });

    let resp = client
        .user_post("/run")
        .json(&body)
        .send()
        .await
        .context("failed to connect to gateway")?;

    let status = resp.status();
    let resp = GatewayClient::check_response(resp).await?;

    if status == reqwest::StatusCode::OK {
        // Sync mode — print stdout
        let sync_resp: RunSyncResponse = resp
            .json()
            .await
            .context("failed to parse sync run response")?;
        if !sync_resp.stdout.is_empty() {
            println!("{}", sync_resp.stdout);
        }
    } else {
        // Async or persistent — print invocation_id
        let async_resp: RunAsyncResponse = resp
            .json()
            .await
            .context("failed to parse async run response")?;
        println!("{}", async_resp.invocation_id);
    }

    Ok(())
}
