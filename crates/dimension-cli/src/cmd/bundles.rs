//! Bundle management subcommands.

use anyhow::Result;
use clap::Subcommand;

use crate::client::GatewayClient;

#[derive(Subcommand, Debug)]
pub enum BundlesCmd {
    /// List all bundles (admin: all users)
    List {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Inspect a bundle
    Inspect {
        /// Bundle ID
        bundle_id: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Delete a bundle
    Delete {
        /// Bundle ID
        bundle_id: String,
        /// Show what would happen without making changes
        #[arg(long)]
        dry_run: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

pub async fn run(client: &GatewayClient, cmd: BundlesCmd) -> Result<()> {
    match cmd {
        BundlesCmd::List { json } => list(client, json).await,
        BundlesCmd::Inspect { bundle_id, json } => inspect(client, &bundle_id, json).await,
        BundlesCmd::Delete {
            bundle_id,
            dry_run,
            json,
        } => delete(client, &bundle_id, dry_run, json).await,
    }
}

async fn list(client: &GatewayClient, json: bool) -> Result<()> {
    let resp = client.admin_get("/bundles").send().await?;
    let resp = GatewayClient::check_response(resp).await?;
    let body: serde_json::Value = resp.json().await?;

    if json {
        crate::output::print_json(&body);
        return Ok(());
    }

    let bundles = body["bundles"].as_array().cloned().unwrap_or_default();
    if bundles.is_empty() {
        println!("No bundles found.");
        return Ok(());
    }

    let rows: Vec<Vec<String>> = bundles
        .iter()
        .map(|b| {
            vec![
                b["id"].as_str().unwrap_or("-").to_string(),
                b["name"].as_str().unwrap_or("-").to_string(),
                b["tag"].as_str().unwrap_or("-").to_string(),
                b["created_at"].as_str().unwrap_or("-").to_string(),
            ]
        })
        .collect();

    crate::output::print_table(&["ID", "NAME", "TAG", "CREATED"], rows);
    Ok(())
}

async fn inspect(client: &GatewayClient, bundle_id: &str, json: bool) -> Result<()> {
    // Use user-scoped endpoint — works with admin tokens too.
    let resp = client
        .user_get(&format!("/bundles/{bundle_id}"))
        .send()
        .await?;
    let resp = GatewayClient::check_response(resp).await?;
    let body: serde_json::Value = resp.json().await?;

    if json {
        crate::output::print_json(&body);
        return Ok(());
    }

    // The response may be wrapped in a "bundle" key or be the object directly.
    let b = if body.get("id").is_some() {
        &body
    } else {
        body.get("bundle").unwrap_or(&body)
    };

    println!("ID:           {}", b["id"].as_str().unwrap_or("-"));
    println!("Name:         {}", b["name"].as_str().unwrap_or("-"));
    println!("Tag:          {}", b["tag"].as_str().unwrap_or("-"));
    println!(
        "Content Hash: {}",
        b["content_hash"].as_str().unwrap_or("-")
    );
    println!("Owner:        {}", b["owner_id"].as_str().unwrap_or("-"));
    println!("Created:      {}", b["created_at"].as_str().unwrap_or("-"));

    // Print manifest details if present.
    if let Some(manifest) = b.get("manifest")
        && !manifest.is_null()
    {
        println!("Manifest:");
        if let Some(entrypoint) = manifest["entrypoint"].as_str() {
            println!("  Entrypoint: {entrypoint}");
        }
        if let Some(memory) = manifest["memory_mb"].as_i64() {
            println!("  Memory MB:  {memory}");
        }
        if let Some(vcpus) = manifest["vcpus"].as_i64() {
            println!("  VCPUs:      {vcpus}");
        }
    }
    Ok(())
}

async fn delete(
    client: &GatewayClient,
    bundle_id: &str,
    dry_run: bool,
    json: bool,
) -> Result<()> {
    if dry_run {
        crate::output::dry_run_notice("bundle", bundle_id, "delete");
        return Ok(());
    }
    if !crate::output::confirm(&format!("Delete bundle {bundle_id}?")) {
        println!("Aborted.");
        return Ok(());
    }
    let resp = client
        .admin_delete(&format!("/bundles/{bundle_id}"))
        .send()
        .await?;

    // Handle 409 (active sessions) specially — don't treat as an error.
    if resp.status() == reqwest::StatusCode::CONFLICT {
        let body_text = resp.text().await.unwrap_or_default();
        let body: serde_json::Value =
            serde_json::from_str(&body_text).unwrap_or(serde_json::Value::Null);
        let msg = body["message"].as_str().unwrap_or(&body_text).to_string();
        if json {
            crate::output::print_json(&serde_json::json!({ "error": msg }));
        } else {
            println!("Cannot delete bundle: {msg}");
        }
        return Ok(());
    }

    let resp = GatewayClient::check_response(resp).await?;
    let body: serde_json::Value = resp.json().await?;

    if json {
        crate::output::print_json(&body);
    } else {
        println!("{}", body["message"].as_str().unwrap_or("bundle deleted"));
    }
    Ok(())
}
