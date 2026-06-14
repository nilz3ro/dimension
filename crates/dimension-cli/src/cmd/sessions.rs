//! Session management subcommands.

use std::path::PathBuf;

use anyhow::Result;
use clap::Subcommand;
use uuid::Uuid;

use crate::client::GatewayClient;

#[derive(Subcommand, Debug)]
pub enum SessionsCmd {
    /// List all sessions (admin: all users)
    List {
        /// Filter by bundle ID
        #[arg(long)]
        bundle: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Inspect a session (detail + messages)
    Inspect {
        /// Session ID
        session_id: Uuid,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Delete a session
    Delete {
        /// Session ID
        session_id: Uuid,
        /// Show what would happen without making changes
        #[arg(long)]
        dry_run: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Search sessions by keyword, bundle, or date range
    Search {
        /// Search keyword
        #[arg(long, short = 'q')]
        query: Option<String>,
        /// Filter by bundle ID
        #[arg(long)]
        bundle: Option<String>,
        /// Start date (ISO 8601)
        #[arg(long)]
        from: Option<String>,
        /// End date (ISO 8601)
        #[arg(long)]
        to: Option<String>,
        /// Maximum results to return
        #[arg(long, default_value_t = 50)]
        limit: i64,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Export session history as JSON
    Export {
        /// Session ID
        session_id: Uuid,
        /// Output file (defaults to stdout)
        #[arg(long, short)]
        output: Option<PathBuf>,
    },
}

pub async fn run(client: &GatewayClient, cmd: SessionsCmd) -> Result<()> {
    match cmd {
        SessionsCmd::List { bundle, json } => list(client, bundle, json).await,
        SessionsCmd::Inspect { session_id, json } => inspect(client, session_id, json).await,
        SessionsCmd::Delete {
            session_id,
            dry_run,
            json,
        } => delete(client, session_id, dry_run, json).await,
        SessionsCmd::Search {
            query,
            bundle,
            from,
            to,
            limit,
            json,
        } => search(client, query, bundle, from, to, limit, json).await,
        SessionsCmd::Export { session_id, output } => export(client, session_id, output).await,
    }
}

async fn list(client: &GatewayClient, bundle: Option<String>, json: bool) -> Result<()> {
    let path = match &bundle {
        Some(b) => format!("/sessions?bundle={}", urlencoding_simple(b)),
        None => "/sessions".to_string(),
    };
    let resp = client.admin_get(&path).send().await?;
    let resp = GatewayClient::check_response(resp).await?;
    let body: serde_json::Value = resp.json().await?;

    if json {
        crate::output::print_json(&body);
        return Ok(());
    }

    let sessions = body["sessions"].as_array().cloned().unwrap_or_default();
    if sessions.is_empty() {
        println!("No sessions found.");
        return Ok(());
    }

    let rows: Vec<Vec<String>> = sessions
        .iter()
        .map(|s| {
            let id = s["id"].as_str().unwrap_or("-");
            let user = s["user_id"].as_str().unwrap_or("-");
            let bnd = s["bundle_id"].as_str().unwrap_or("-");
            let msgs = s["message_count"]
                .as_i64()
                .map(|n| n.to_string())
                .unwrap_or_else(|| "-".to_string());
            let created = s["created_at"].as_str().unwrap_or("-");
            let updated = s["updated_at"].as_str().unwrap_or("-");
            vec![
                truncate_uuid(id),
                truncate_uuid(user),
                truncate_uuid(bnd),
                msgs,
                created.to_string(),
                updated.to_string(),
            ]
        })
        .collect();

    crate::output::print_table(&["ID", "USER", "BUNDLE", "MSGS", "CREATED", "UPDATED"], rows);
    Ok(())
}

async fn inspect(client: &GatewayClient, session_id: Uuid, json: bool) -> Result<()> {
    let resp = client
        .admin_get(&format!("/sessions/{session_id}"))
        .send()
        .await?;
    let resp = GatewayClient::check_response(resp).await?;
    let body: serde_json::Value = resp.json().await?;

    if json {
        crate::output::print_json(&body);
        return Ok(());
    }

    let session = &body["session"];
    println!("Session:   {}", session["id"].as_str().unwrap_or("-"));
    println!("Bundle:    {}", session["bundle_id"].as_str().unwrap_or("-"));
    println!("Created:   {}", session["created_at"].as_str().unwrap_or("-"));
    println!("Updated:   {}", session["updated_at"].as_str().unwrap_or("-"));

    let messages = body["messages"].as_array();
    let msg_count = messages.map(|m| m.len()).unwrap_or(0);
    println!("Messages:  {msg_count}");

    if let Some(msgs) = messages
        && !msgs.is_empty()
    {
        println!();
        println!("--- Messages ---");
        for msg in msgs {
            let role = msg["role"].as_str().unwrap_or("unknown");
            let content = msg["content"].as_str().unwrap_or("");
            println!("[{role}] {content}");
        }
    }
    Ok(())
}

async fn delete(
    client: &GatewayClient,
    session_id: Uuid,
    dry_run: bool,
    json: bool,
) -> Result<()> {
    if dry_run {
        crate::output::dry_run_notice("session", &session_id.to_string(), "delete");
        return Ok(());
    }
    if !crate::output::confirm(&format!("Delete session {session_id}?")) {
        println!("Aborted.");
        return Ok(());
    }
    let resp = client
        .admin_delete(&format!("/sessions/{session_id}"))
        .send()
        .await?;
    let resp = GatewayClient::check_response(resp).await?;
    let body: serde_json::Value = resp.json().await?;

    if json {
        crate::output::print_json(&body);
    } else {
        println!("{}", body["message"].as_str().unwrap_or("session deleted"));
    }
    Ok(())
}

async fn search(
    client: &GatewayClient,
    query: Option<String>,
    bundle: Option<String>,
    from: Option<String>,
    to: Option<String>,
    limit: i64,
    json: bool,
) -> Result<()> {
    let mut parts: Vec<String> = vec![format!("limit={limit}")];
    if let Some(q) = &query {
        parts.push(format!("q={}", urlencoding_simple(q)));
    }
    if let Some(b) = &bundle {
        parts.push(format!("bundle={}", urlencoding_simple(b)));
    }
    if let Some(f) = &from {
        parts.push(format!("from={}", urlencoding_simple(f)));
    }
    if let Some(t) = &to {
        parts.push(format!("to={}", urlencoding_simple(t)));
    }
    let qs = parts.join("&");
    let path = format!("/sessions/search?{qs}");

    let resp = client
        .admin_get(&path)
        .send()
        .await?;
    let resp = GatewayClient::check_response(resp).await?;
    let body: serde_json::Value = resp.json().await?;

    if json {
        crate::output::print_json(&body);
        return Ok(());
    }

    let results = body["results"].as_array().cloned().unwrap_or_default();
    if results.is_empty() {
        println!("No results found.");
        return Ok(());
    }

    let rows: Vec<Vec<String>> = results
        .iter()
        .map(|r| {
            let session = r["session_id"].as_str().unwrap_or("-");
            let bnd = r["bundle_id"].as_str().unwrap_or("-");
            let date = r["created_at"].as_str().unwrap_or("-");
            let snippet = r["match_snippet"].as_str().unwrap_or("");
            let snippet_truncated = if snippet.len() > 60 {
                format!("{}...", &snippet[..60])
            } else {
                snippet.to_string()
            };
            vec![
                truncate_uuid(session),
                truncate_uuid(bnd),
                date.to_string(),
                snippet_truncated,
            ]
        })
        .collect();

    crate::output::print_table(&["SESSION", "BUNDLE", "DATE", "MATCH"], rows);
    Ok(())
}

async fn export(
    client: &GatewayClient,
    session_id: Uuid,
    output: Option<PathBuf>,
) -> Result<()> {
    let resp = client
        .admin_get(&format!("/sessions/{session_id}"))
        .send()
        .await?;
    let resp = GatewayClient::check_response(resp).await?;
    let body: serde_json::Value = resp.json().await?;

    let pretty = serde_json::to_string_pretty(&body)?;

    match output {
        Some(path) => {
            std::fs::write(&path, &pretty)?;
            println!("Exported session {session_id} to {}", path.display());
        }
        None => println!("{pretty}"),
    }
    Ok(())
}

/// Truncate a UUID string to its first 8 characters for table display.
fn truncate_uuid(s: &str) -> String {
    if s.len() > 8 {
        s[..8].to_string()
    } else {
        s.to_string()
    }
}

/// Percent-encode a string for use in URL query parameters.
///
/// Only encodes characters that are not unreserved (A-Z, a-z, 0-9, -, _, ., ~).
fn urlencoding_simple(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                out.push(char::from_digit((b >> 4) as u32, 16).unwrap().to_ascii_uppercase());
                out.push(char::from_digit((b & 0xF) as u32, 16).unwrap().to_ascii_uppercase());
            }
        }
    }
    out
}
