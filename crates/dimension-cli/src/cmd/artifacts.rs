//! Artifact management subcommands.

use std::io::Write as IoWrite;
use std::path::PathBuf;

use anyhow::Result;
use clap::Subcommand;
use uuid::Uuid;

use crate::client::GatewayClient;

#[derive(Subcommand, Debug)]
pub enum ArtifactsCmd {
    /// List artifacts (admin: all users, or per session with --session)
    List {
        /// Filter by session ID
        #[arg(long)]
        session: Option<Uuid>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Download an artifact to stdout or a file
    Download {
        /// Session ID
        session_id: Uuid,
        /// Artifact key
        key: String,
        /// Write to file instead of stdout
        #[arg(long, short)]
        output: Option<PathBuf>,
    },
    /// Delete an artifact
    Delete {
        /// Session ID
        session_id: Uuid,
        /// Artifact key
        key: String,
        /// Show what would happen without making changes
        #[arg(long)]
        dry_run: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

pub async fn run(client: &GatewayClient, cmd: ArtifactsCmd) -> Result<()> {
    match cmd {
        ArtifactsCmd::List { session, json } => list(client, session, json).await,
        ArtifactsCmd::Download {
            session_id,
            key,
            output,
        } => download(client, session_id, &key, output).await,
        ArtifactsCmd::Delete {
            session_id,
            key,
            dry_run,
            json,
        } => delete(client, session_id, &key, dry_run, json).await,
    }
}

/// Format size_bytes as a human-readable string (B, KB, MB, GB).
fn format_size(bytes: Option<i64>) -> String {
    match bytes {
        None => "-".to_string(),
        Some(b) if b < 1024 => format!("{b} B"),
        Some(b) if b < 1024 * 1024 => format!("{:.1} KB", b as f64 / 1024.0),
        Some(b) if b < 1024 * 1024 * 1024 => {
            format!("{:.1} MB", b as f64 / (1024.0 * 1024.0))
        }
        Some(b) => format!("{:.1} GB", b as f64 / (1024.0 * 1024.0 * 1024.0)),
    }
}

async fn list(client: &GatewayClient, session: Option<Uuid>, json: bool) -> Result<()> {
    let body: serde_json::Value = if let Some(session_id) = session {
        // Per-session list: GET /sessions/{id}/artifacts (user-scoped)
        let resp = client
            .user_get(&format!("/sessions/{session_id}/artifacts"))
            .send()
            .await?;
        let resp = GatewayClient::check_response(resp).await?;
        resp.json().await?
    } else {
        // Cross-user admin list: GET /admin/artifacts
        let resp = client.admin_get("/artifacts").send().await?;
        let resp = GatewayClient::check_response(resp).await?;
        resp.json().await?
    };

    if json {
        crate::output::print_json(&body);
        return Ok(());
    }

    // Admin endpoint returns {"artifacts": [...], "next_cursor": "..."}
    // Session endpoint returns an array directly.
    let artifacts = if body.is_array() {
        body.as_array().cloned().unwrap_or_default()
    } else {
        body["artifacts"].as_array().cloned().unwrap_or_default()
    };

    if artifacts.is_empty() {
        println!("No artifacts found.");
        return Ok(());
    }

    let rows: Vec<Vec<String>> = artifacts
        .iter()
        .map(|a| {
            let id = a["id"].as_str().unwrap_or("-");
            let short_id = if id.len() > 8 { &id[..8] } else { id };
            let session = a["session_id"].as_str().unwrap_or("-");
            let short_session = if session.len() > 8 {
                &session[..8]
            } else {
                session
            };
            let key = a["object_key"].as_str().unwrap_or("-");
            let size = format_size(a["size_bytes"].as_i64());
            let content_type = a["content_type"].as_str().unwrap_or("-");
            let created = a["created_at"].as_str().unwrap_or("-");
            vec![
                short_id.to_string(),
                short_session.to_string(),
                key.to_string(),
                size,
                content_type.to_string(),
                created.to_string(),
            ]
        })
        .collect();

    crate::output::print_table(&["ID", "SESSION", "KEY", "SIZE", "TYPE", "CREATED"], rows);
    Ok(())
}

async fn download(
    client: &GatewayClient,
    session_id: Uuid,
    key: &str,
    output: Option<PathBuf>,
) -> Result<()> {
    // GET /sessions/{session_id}/artifacts/{key} — user-scoped
    let resp = client
        .user_get(&format!("/sessions/{session_id}/artifacts/{key}"))
        .send()
        .await?;
    let resp = GatewayClient::check_response(resp).await?;
    let bytes = resp.bytes().await?;

    match output {
        Some(path) => {
            let byte_count = bytes.len();
            std::fs::write(&path, &bytes)?;
            eprintln!("Wrote {byte_count} bytes to {}", path.display());
        }
        None => {
            let stdout = std::io::stdout();
            let mut handle = stdout.lock();
            handle.write_all(&bytes)?;
        }
    }
    Ok(())
}

async fn delete(
    client: &GatewayClient,
    session_id: Uuid,
    key: &str,
    dry_run: bool,
    json: bool,
) -> Result<()> {
    if dry_run {
        crate::output::dry_run_notice("artifact", &format!("{session_id}/{key}"), "delete");
        return Ok(());
    }
    if !crate::output::confirm(&format!(
        "Delete artifact '{key}' from session {session_id}?"
    )) {
        println!("Aborted.");
        return Ok(());
    }
    // DELETE /admin/sessions/{session_id}/artifacts/{key}
    let resp = client
        .admin_delete(&format!("/sessions/{session_id}/artifacts/{key}"))
        .send()
        .await?;
    let resp = GatewayClient::check_response(resp).await?;
    let body: serde_json::Value = resp.json().await?;

    if json {
        crate::output::print_json(&body);
    } else {
        println!("{}", body["message"].as_str().unwrap_or("artifact deleted"));
    }
    Ok(())
}
