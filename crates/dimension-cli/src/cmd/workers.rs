//! Worker management subcommands.

use anyhow::Result;
use clap::Subcommand;
use uuid::Uuid;

use crate::client::GatewayClient;

#[derive(Subcommand, Debug)]
pub enum WorkersCmd {
    /// List registered workers
    List {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Mark a worker as draining (no new VMs)
    Drain {
        /// Worker ID
        worker_id: Uuid,
        /// Show what would happen without making changes
        #[arg(long)]
        dry_run: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Remove a worker from the registry
    Remove {
        /// Worker ID
        worker_id: Uuid,
        /// Show what would happen without making changes
        #[arg(long)]
        dry_run: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

pub async fn run(client: &GatewayClient, cmd: WorkersCmd) -> Result<()> {
    match cmd {
        WorkersCmd::List { json } => list(client, json).await,
        WorkersCmd::Drain { worker_id, dry_run, json } => drain(client, worker_id, dry_run, json).await,
        WorkersCmd::Remove { worker_id, dry_run, json } => remove(client, worker_id, dry_run, json).await,
    }
}

/// Format seconds as a human-readable "N ago" string.
fn format_last_seen(secs: Option<i64>) -> String {
    match secs {
        None => "-".to_string(),
        Some(s) if s < 60 => format!("{}s ago", s),
        Some(s) if s < 3600 => format!("{}m ago", s / 60),
        Some(s) if s < 86400 => format!("{}h ago", s / 3600),
        Some(s) => format!("{}d ago", s / 86400),
    }
}

async fn list(client: &GatewayClient, json: bool) -> Result<()> {
    let resp = client.admin_get("/workers").send().await?;
    let resp = GatewayClient::check_response(resp).await?;
    let body: serde_json::Value = resp.json().await?;

    if json {
        crate::output::print_json(&body);
        return Ok(());
    }

    // Single-host mode: gateway returns a "note" instead of workers list.
    if let Some(note) = body.get("note").and_then(|n| n.as_str()) {
        println!("{}", note);
        return Ok(());
    }

    let workers = body["workers"].as_array().cloned().unwrap_or_default();
    if workers.is_empty() {
        println!("No workers registered.");
        return Ok(());
    }

    let rows: Vec<Vec<String>> = workers
        .iter()
        .map(|w| {
            let id = w["worker_id"].as_str().unwrap_or("-");
            // Truncate worker_id to first 8 chars for readability.
            let short_id = if id.len() > 8 { &id[..8] } else { id };
            let addr = w["grpc_addr"].as_str().unwrap_or("-").to_string();
            let memory = w["available_memory_mb"]
                .as_i64()
                .map(|m| format!("{m}"))
                .unwrap_or_else(|| "-".to_string());
            let vcpus = w["available_vcpus"]
                .as_i64()
                .map(|v| format!("{v}"))
                .unwrap_or_else(|| "-".to_string());
            let vms = w["running_vms"]
                .as_i64()
                .map(|v| format!("{v}"))
                .unwrap_or_else(|| "-".to_string());
            let draining = if w["draining"].as_bool().unwrap_or(false) {
                "yes"
            } else {
                "no"
            }
            .to_string();
            let last_seen = format_last_seen(w["last_seen_secs"].as_i64());
            vec![
                short_id.to_string(),
                addr,
                memory,
                vcpus,
                vms,
                draining,
                last_seen,
            ]
        })
        .collect();

    crate::output::print_table(
        &["ID", "ADDR", "MEMORY MB", "VCPUs", "VMs", "DRAINING", "LAST SEEN"],
        rows,
    );
    Ok(())
}

async fn drain(client: &GatewayClient, worker_id: Uuid, dry_run: bool, json: bool) -> Result<()> {
    if dry_run {
        crate::output::dry_run_notice("worker", &worker_id.to_string(), "drain");
        return Ok(());
    }
    if !crate::output::confirm(&format!("Drain worker {worker_id}?")) {
        println!("Aborted.");
        return Ok(());
    }
    let resp = client
        .admin_post(&format!("/workers/{worker_id}/drain"))
        .send()
        .await?;
    let resp = GatewayClient::check_response(resp).await?;
    let body: serde_json::Value = resp.json().await?;

    if json {
        crate::output::print_json(&serde_json::json!({ "drained": worker_id.to_string() }));
    } else {
        println!(
            "{}",
            body["message"].as_str().unwrap_or("worker marked as draining")
        );
    }
    Ok(())
}

async fn remove(client: &GatewayClient, worker_id: Uuid, dry_run: bool, json: bool) -> Result<()> {
    if dry_run {
        crate::output::dry_run_notice("worker", &worker_id.to_string(), "remove");
        return Ok(());
    }
    if !crate::output::confirm(&format!(
        "Remove worker {worker_id}? This is irreversible."
    )) {
        println!("Aborted.");
        return Ok(());
    }
    let resp = client
        .admin_delete(&format!("/workers/{worker_id}"))
        .send()
        .await?;
    let resp = GatewayClient::check_response(resp).await?;
    let body: serde_json::Value = resp.json().await?;

    if json {
        crate::output::print_json(&serde_json::json!({ "removed": worker_id.to_string() }));
    } else {
        println!(
            "{}",
            body["message"].as_str().unwrap_or("worker removed")
        );
    }
    Ok(())
}
