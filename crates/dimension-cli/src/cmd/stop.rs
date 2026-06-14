//! `dimension stop` — stop a running invocation.
//!
//! Sends POST /run/{id}/stop to the gateway to kill a persistent (or still-running)
//! invocation VM.

use anyhow::{Context, Result};
use clap::Args;
use serde::Deserialize;

use crate::client::GatewayClient;

/// Stop a running invocation.
#[derive(Debug, Args)]
pub struct StopCmd {
    /// Invocation ID to stop (UUID)
    pub id: String,
}

/// Response from POST /run/{id}/stop.
#[derive(Debug, Deserialize)]
struct StopResponse {
    message: String,
    #[allow(dead_code)]
    invocation_id: String,
}

/// Execute the stop command.
pub async fn run(cmd: StopCmd, client: &GatewayClient) -> Result<()> {
    let path = format!("/run/{}/stop", cmd.id);

    let resp = client
        .user_post(&path)
        .send()
        .await
        .context("failed to connect to gateway")?;

    let resp = GatewayClient::check_response(resp).await?;
    let stop_resp: StopResponse = resp
        .json()
        .await
        .context("failed to parse stop response")?;

    println!("{}", stop_resp.message);

    Ok(())
}
