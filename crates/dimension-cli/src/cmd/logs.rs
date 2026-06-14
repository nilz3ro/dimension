//! `dimension logs` — live gateway log streaming and invocation log retrieval.
//!
//! ## Subcommands
//!
//! - `dimension logs tail` — stream live gateway logs (SSE)
//! - `dimension logs get <id>` — fetch invocation metadata and logs
//!
//! # Usage
//! ```text
//! dimension logs tail [--request-id <id>] [--session <uuid>] [--bundle <name>]
//! dimension logs get <invocation_id>
//! ```

use anyhow::Result;
use clap::Subcommand;
use futures::StreamExt;
use serde::Deserialize;

use crate::client::GatewayClient;

/// Log streaming subcommand group.
#[derive(Subcommand)]
pub enum LogsCmd {
    /// Stream recent and live gateway log lines (press Ctrl-C to stop)
    Tail {
        /// Only show lines containing this request ID
        #[arg(long)]
        request_id: Option<String>,
        /// Only show lines containing this session UUID
        #[arg(long)]
        session: Option<String>,
        /// Only show lines containing this bundle name
        #[arg(long)]
        bundle: Option<String>,
    },
    /// Fetch invocation metadata and logs
    Get {
        /// Invocation UUID
        invocation_id: String,
    },
}

/// Run the logs subcommand.
pub async fn run(cmd: LogsCmd, client: &GatewayClient) -> Result<()> {
    match cmd {
        LogsCmd::Tail {
            request_id,
            session,
            bundle,
        } => tail(client, request_id, session, bundle).await,
        LogsCmd::Get { invocation_id } => get_invocation(client, &invocation_id).await,
    }
}

// ── Invocation response ────────────────────────────────────────────────

/// Invocation metadata returned by GET /invocations/{id}.
#[derive(Debug, Deserialize)]
struct InvocationResponse {
    invocation_id: String,
    #[allow(dead_code)]
    user_id: String,
    bundle_id: String,
    #[allow(dead_code)]
    worker_id: String,
    mode: String,
    status: String,
    exit_code: i32,
    duration_ms: u64,
    #[allow(dead_code)]
    log_url: String,
    #[allow(dead_code)]
    created_at: i64,
    #[allow(dead_code)]
    completed_at: i64,
    logs: Option<String>,
}

/// Fetch and display invocation metadata and logs.
async fn get_invocation(client: &GatewayClient, invocation_id: &str) -> Result<()> {
    let path = format!("/invocations/{}?include_logs=true", url_encode(invocation_id));
    let resp = client.user_get(&path).send().await?;
    let resp = GatewayClient::check_response(resp).await?;
    let invocation: InvocationResponse = resp.json().await?;

    // Print metadata header.
    println!("Invocation: {}", invocation.invocation_id);
    println!("  Bundle:   {}", invocation.bundle_id);
    println!("  Mode:     {}", invocation.mode);
    println!("  Status:   {}", invocation.status);
    println!("  Exit:     {}", invocation.exit_code);
    println!("  Duration: {}ms", invocation.duration_ms);

    // Print log content if available.
    match invocation.logs {
        Some(ref content) if !content.is_empty() => {
            println!();
            println!("--- logs ---");
            print!("{content}");
            if !content.ends_with('\n') {
                println!();
            }
        }
        _ => {
            println!();
            println!("(no logs available)");
        }
    }

    Ok(())
}

// ── Tail (existing) ────────────────────────────────────────────────────

/// Stream log lines from the gateway SSE endpoint.
async fn tail(
    client: &GatewayClient,
    request_id: Option<String>,
    session: Option<String>,
    bundle: Option<String>,
) -> Result<()> {
    // Build the query string from optional filters.
    let mut params: Vec<String> = Vec::new();
    if let Some(ref rid) = request_id {
        params.push(format!("request_id={}", url_encode(rid)));
    }
    if let Some(ref sid) = session {
        params.push(format!("session={}", url_encode(sid)));
    }
    if let Some(ref bid) = bundle {
        params.push(format!("bundle={}", url_encode(bid)));
    }
    let qs = if params.is_empty() {
        String::new()
    } else {
        format!("?{}", params.join("&"))
    };

    let path = format!("/logs/tail{}", qs);

    // Use a streaming client (no timeout) so the SSE connection is not killed
    // by the default 30-second timeout on GatewayClient's inner client.
    let resp = client.admin_get_streaming(&path).send().await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!("HTTP {}: {}", status, body));
    }

    // Read the response body as a stream of chunks, split on newlines, and
    // print each SSE `data:` line to stdout.
    let mut stream = resp.bytes_stream();
    let mut buf = String::new();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        buf.push_str(&String::from_utf8_lossy(&chunk));

        // SSE lines end with \n. Flush all complete lines from the buffer.
        while let Some(pos) = buf.find('\n') {
            let raw_line = buf[..pos].trim().to_string();
            buf = buf[pos + 1..].to_string();

            // SSE format: "data: <content>" — strip the prefix.
            if let Some(content) = raw_line.strip_prefix("data: ") {
                // Skip heartbeat events sent by the server keep-alive.
                if content != "heartbeat" && !content.is_empty() {
                    println!("{}", content);
                }
            }
            // Lines starting with ":" are SSE comments — ignore them.
            // Empty lines are SSE event separators — ignore them.
        }
    }

    Ok(())
}

/// Percent-encode a query parameter value.
///
/// Only unreserved characters (A-Z, a-z, 0-9, `-`, `_`, `.`, `~`) are passed
/// through; everything else is encoded as `%XX`.
fn url_encode(s: &str) -> String {
    s.chars()
        .flat_map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => {
                vec![c.to_string()]
            }
            other => vec![format!("%{:02X}", other as u32)],
        })
        .collect::<Vec<_>>()
        .join("")
}
