//! `dimension rollback` — rollback a bundle to its previous version.
//!
//! Sends POST /bundles/{name}/rollback to the gateway, which swaps the active
//! version to the previous one and redistributes to all workers.

use anyhow::{Context, Result};
use clap::Args;
use serde::Deserialize;

use crate::client::GatewayClient;

/// Rollback a bundle to its previous version.
#[derive(Debug, Args)]
pub struct RollbackCmd {
    /// Bundle name to rollback
    pub name: String,
}

/// Response from POST /bundles/{name}/rollback.
#[derive(Debug, Deserialize)]
struct RollbackResponse {
    bundle_id: i64,
    content_hash: String,
    status: String,
}

/// Execute the rollback command.
pub async fn run(cmd: RollbackCmd, client: &GatewayClient) -> Result<()> {
    let path = format!("/bundles/{}/rollback", cmd.name);

    let resp = client
        .user_post(&path)
        .send()
        .await
        .context("failed to connect to gateway")?;

    let resp = GatewayClient::check_response(resp).await?;
    let rollback_resp: RollbackResponse = resp
        .json()
        .await
        .context("failed to parse rollback response")?;

    println!();
    println!("  Bundle rolled back successfully!");
    println!("  ├─ Bundle ID:    {}", rollback_resp.bundle_id);
    println!("  ├─ Name:         {}", cmd.name);
    println!("  ├─ Content Hash: {}", rollback_resp.content_hash);
    println!("  └─ Status:       {}", rollback_resp.status);

    Ok(())
}
