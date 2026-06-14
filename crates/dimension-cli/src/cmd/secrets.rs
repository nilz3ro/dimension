//! Secrets management subcommands.

use anyhow::Result;
use clap::Subcommand;

use crate::client::GatewayClient;

#[derive(Subcommand, Debug)]
pub enum SecretsCmd {
    /// List secrets for a bundle
    List {
        /// Bundle ID
        #[arg(long)]
        bundle: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Set a secret for a bundle
    Set {
        /// Bundle ID
        #[arg(long)]
        bundle: String,
        /// Secret name
        #[arg(long)]
        name: String,
        /// Secret value (prompted on stdin if not provided)
        #[arg(long)]
        value: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Delete a secret for a bundle
    Delete {
        /// Bundle ID
        #[arg(long)]
        bundle: String,
        /// Secret name
        #[arg(long)]
        name: String,
        /// Show what would happen without making changes
        #[arg(long)]
        dry_run: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

pub async fn run(client: &GatewayClient, cmd: SecretsCmd) -> Result<()> {
    match cmd {
        SecretsCmd::List { bundle, json } => list(client, &bundle, json).await,
        SecretsCmd::Set {
            bundle,
            name,
            value,
            json,
        } => set(client, &bundle, &name, value, json).await,
        SecretsCmd::Delete {
            bundle,
            name,
            dry_run,
            json,
        } => delete(client, &bundle, &name, dry_run, json).await,
    }
}

async fn list(client: &GatewayClient, bundle: &str, json: bool) -> Result<()> {
    // GET /bundles/{id}/secrets — user-scoped; admin token works
    let resp = client
        .user_get(&format!("/bundles/{bundle}/secrets"))
        .send()
        .await?;
    let resp = GatewayClient::check_response(resp).await?;
    let body: serde_json::Value = resp.json().await?;

    if json {
        crate::output::print_json(&body);
        return Ok(());
    }

    // Response is an array of {name, created_at, updated_at}
    let secrets = if body.is_array() {
        body.as_array().cloned().unwrap_or_default()
    } else {
        body["secrets"].as_array().cloned().unwrap_or_default()
    };

    if secrets.is_empty() {
        println!("No secrets found for bundle {bundle}.");
        return Ok(());
    }

    let rows: Vec<Vec<String>> = secrets
        .iter()
        .map(|s| {
            vec![
                s["name"].as_str().unwrap_or("-").to_string(),
                s["created_at"].as_str().unwrap_or("-").to_string(),
                s["updated_at"].as_str().unwrap_or("-").to_string(),
            ]
        })
        .collect();

    crate::output::print_table(&["NAME", "CREATED", "UPDATED"], rows);
    Ok(())
}

async fn set(
    client: &GatewayClient,
    bundle: &str,
    name: &str,
    value: Option<String>,
    json: bool,
) -> Result<()> {
    // If value not provided, prompt from stdin.
    let secret_value = match value {
        Some(v) => v,
        None => {
            eprint!("Secret value for '{name}': ");
            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;
            input.trim().to_string()
        }
    };

    // POST /bundles/{id}/secrets with {"name": name, "value": value}
    let resp = client
        .user_post(&format!("/bundles/{bundle}/secrets"))
        .json(&serde_json::json!({ "name": name, "value": secret_value }))
        .send()
        .await?;
    let resp = GatewayClient::check_response(resp).await?;
    let body: serde_json::Value = resp.json().await?;

    if json {
        crate::output::print_json(&body);
    } else {
        println!("{}", body["message"].as_str().unwrap_or("secret set"));
    }
    Ok(())
}

async fn delete(
    client: &GatewayClient,
    bundle: &str,
    name: &str,
    dry_run: bool,
    json: bool,
) -> Result<()> {
    if dry_run {
        crate::output::dry_run_notice("secret", &format!("{name} (bundle {bundle})"), "delete");
        return Ok(());
    }
    if !crate::output::confirm(&format!(
        "Delete secret '{name}' from bundle '{bundle}'?"
    )) {
        println!("Aborted.");
        return Ok(());
    }
    // DELETE /bundles/{id}/secrets/{name}
    let resp = client
        .user_delete(&format!("/bundles/{bundle}/secrets/{name}"))
        .send()
        .await?;
    let resp = GatewayClient::check_response(resp).await?;
    let body: serde_json::Value = resp.json().await?;

    if json {
        crate::output::print_json(&body);
    } else {
        println!("{}", body["message"].as_str().unwrap_or("secret deleted"));
    }
    Ok(())
}
