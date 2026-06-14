//! API key management subcommands.

use anyhow::Result;
use clap::Subcommand;
use uuid::Uuid;

use crate::client::GatewayClient;

#[derive(Subcommand, Debug)]
pub enum KeysCmd {
    /// List API keys for a user
    List {
        /// User ID
        #[arg(long)]
        user: Uuid,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Create an API key for a user
    Create {
        /// User ID
        #[arg(long)]
        user: Uuid,
        /// Optional key label
        #[arg(long)]
        label: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Revoke an API key
    Revoke {
        /// User ID
        #[arg(long)]
        user: Uuid,
        /// Key ID to revoke
        #[arg(long)]
        key: Uuid,
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Show what would happen without making changes
        #[arg(long)]
        dry_run: bool,
    },
}

pub async fn run(cmd: KeysCmd, client: &GatewayClient) -> Result<()> {
    match cmd {
        KeysCmd::List { user, json } => list(client, user, json).await,
        KeysCmd::Create { user, label, json } => create(client, user, label, json).await,
        KeysCmd::Revoke {
            user,
            key,
            json,
            dry_run,
        } => revoke(client, user, key, json, dry_run).await,
    }
}

async fn list(client: &GatewayClient, user_id: Uuid, json: bool) -> Result<()> {
    let resp = client
        .admin_get(&format!("/users/{user_id}/keys"))
        .send()
        .await?;
    let resp = GatewayClient::check_response(resp).await?;
    let body: serde_json::Value = resp.json().await?;

    if json {
        crate::output::print_json(&body);
        return Ok(());
    }

    let keys = body["keys"].as_array().cloned().unwrap_or_default();
    if keys.is_empty() {
        println!("No keys found for user {user_id}.");
        return Ok(());
    }

    let rows: Vec<Vec<String>> = keys
        .iter()
        .map(|k| {
            let revoked = k["revoked"].as_bool().unwrap_or(false);
            vec![
                k["key_id"].as_str().unwrap_or("-").to_string(),
                k["key_prefix"].as_str().unwrap_or("-").to_string(),
                k["label"].as_str().unwrap_or("-").to_string(),
                k["created_at"].as_str().unwrap_or("-").to_string(),
                if revoked { "revoked" } else { "active" }.to_string(),
            ]
        })
        .collect();

    crate::output::print_table(&["KEY_ID", "PREFIX", "LABEL", "CREATED", "STATUS"], rows);
    Ok(())
}

async fn create(client: &GatewayClient, user_id: Uuid, label: Option<String>, json: bool) -> Result<()> {
    let body_json = serde_json::json!({ "label": label });
    let resp = client
        .admin_post(&format!("/users/{user_id}/keys"))
        .json(&body_json)
        .send()
        .await?;
    let resp = GatewayClient::check_response(resp).await?;
    let body: serde_json::Value = resp.json().await?;

    if json {
        crate::output::print_json(&body);
        return Ok(());
    }

    println!("Key created:");
    println!("  Key ID:  {}", body["key_id"].as_str().unwrap_or("-"));
    println!("  Prefix:  {}", body["key_prefix"].as_str().unwrap_or("-"));
    if let Some(lbl) = body["label"].as_str() {
        println!("  Label:   {lbl}");
    }
    println!("  API Key: {}", body["api_key"].as_str().unwrap_or("-"));
    println!("(The API key is shown once and cannot be retrieved again.)");
    Ok(())
}

async fn revoke(client: &GatewayClient, user_id: Uuid, key_id: Uuid, json: bool, dry_run: bool) -> Result<()> {
    if dry_run {
        crate::output::dry_run_notice("key", &key_id.to_string(), "revoke");
        return Ok(());
    }
    if !crate::output::confirm(&format!("Revoke key {key_id} for user {user_id}?")) {
        println!("Aborted.");
        return Ok(());
    }
    let resp = client
        .admin_post(&format!("/users/{user_id}/keys/{key_id}/revoke"))
        .send()
        .await?;
    let resp = GatewayClient::check_response(resp).await?;
    let body: serde_json::Value = resp.json().await?;

    if json {
        crate::output::print_json(&body);
    } else {
        println!("{}", body["message"].as_str().unwrap_or("done"));
    }
    Ok(())
}
