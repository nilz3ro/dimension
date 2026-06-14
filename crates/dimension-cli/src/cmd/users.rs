//! User management subcommands.

use anyhow::Result;
use clap::Subcommand;
use uuid::Uuid;

use crate::client::GatewayClient;

#[derive(Subcommand, Debug)]
pub enum UsersCmd {
    /// List all users
    List {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Create a new user
    Create {
        /// User display name
        #[arg(long)]
        name: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show a user's details
    Show {
        /// User ID
        id: Uuid,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Promote a user to admin
    Promote {
        /// User ID
        id: Uuid,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Demote an admin to regular user
    Demote {
        /// User ID
        id: Uuid,
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Show what would happen without making changes
        #[arg(long)]
        dry_run: bool,
    },
    /// Delete a user
    Delete {
        /// User ID
        id: Uuid,
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Show what would happen without making changes
        #[arg(long)]
        dry_run: bool,
    },
}

pub async fn run(cmd: UsersCmd, client: &GatewayClient) -> Result<()> {
    match cmd {
        UsersCmd::List { json } => list(client, json).await,
        UsersCmd::Create { name, json } => create(client, &name, json).await,
        UsersCmd::Show { id, json } => show(client, id, json).await,
        UsersCmd::Promote { id, json } => promote(client, id, json).await,
        UsersCmd::Demote { id, json, dry_run } => demote(client, id, json, dry_run).await,
        UsersCmd::Delete { id, json, dry_run } => delete(client, id, json, dry_run).await,
    }
}

async fn list(client: &GatewayClient, json: bool) -> Result<()> {
    let resp = client.admin_get("/users").send().await?;
    let resp = GatewayClient::check_response(resp).await?;
    let body: serde_json::Value = resp.json().await?;

    if json {
        crate::output::print_json(&body);
        return Ok(());
    }

    let users = body["users"].as_array().cloned().unwrap_or_default();
    if users.is_empty() {
        println!("No users found.");
        return Ok(());
    }

    let rows: Vec<Vec<String>> = users
        .iter()
        .map(|u| {
            vec![
                u["id"].as_str().unwrap_or("-").to_string(),
                u["name"].as_str().unwrap_or("-").to_string(),
                u["role"].as_str().unwrap_or("-").to_string(),
                u["created_at"].as_str().unwrap_or("-").to_string(),
            ]
        })
        .collect();

    crate::output::print_table(&["ID", "NAME", "ROLE", "CREATED"], rows);
    Ok(())
}

async fn create(client: &GatewayClient, name: &str, json: bool) -> Result<()> {
    let resp = client
        .admin_post("/users")
        .json(&serde_json::json!({ "name": name }))
        .send()
        .await?;
    let resp = GatewayClient::check_response(resp).await?;
    let body: serde_json::Value = resp.json().await?;

    if json {
        crate::output::print_json(&body);
        return Ok(());
    }

    println!("User created:");
    println!("  ID:      {}", body["user_id"].as_str().unwrap_or("-"));
    println!("  Name:    {}", body["name"].as_str().unwrap_or("-"));
    println!("  Role:    {}", body["role"].as_str().unwrap_or("-"));
    println!("  API Key: {}", body["api_key"].as_str().unwrap_or("-"));
    println!("(The API key is shown once and cannot be retrieved again.)");
    Ok(())
}

async fn show(client: &GatewayClient, id: Uuid, json: bool) -> Result<()> {
    // No dedicated GET /admin/users/{id} endpoint — list and filter.
    let resp = client.admin_get("/users").send().await?;
    let resp = GatewayClient::check_response(resp).await?;
    let body: serde_json::Value = resp.json().await?;

    let users = body["users"].as_array().cloned().unwrap_or_default();
    let user = users
        .iter()
        .find(|u| u["id"].as_str() == Some(&id.to_string()));

    match user {
        None => anyhow::bail!("User {id} not found"),
        Some(u) => {
            if json {
                crate::output::print_json(u);
            } else {
                println!("ID:                    {}", u["id"].as_str().unwrap_or("-"));
                println!("Name:                  {}", u["name"].as_str().unwrap_or("-"));
                println!("Role:                  {}", u["role"].as_str().unwrap_or("-"));
                println!("Created:               {}", u["created_at"].as_str().unwrap_or("-"));
                if let Some(v) = u.get("quota_max_sessions") {
                    println!("Quota max sessions:    {}", v);
                }
                if let Some(v) = u.get("quota_max_bundles") {
                    println!("Quota max bundles:     {}", v);
                }
                if let Some(v) = u.get("quota_max_concurrent_vms") {
                    println!("Quota max concurrent:  {}", v);
                }
            }
            Ok(())
        }
    }
}

async fn promote(client: &GatewayClient, id: Uuid, json: bool) -> Result<()> {
    let resp = client
        .admin_post(&format!("/users/{id}/promote"))
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

async fn demote(client: &GatewayClient, id: Uuid, json: bool, dry_run: bool) -> Result<()> {
    if dry_run {
        crate::output::dry_run_notice("user", &id.to_string(), "demote");
        return Ok(());
    }
    if !crate::output::confirm(&format!("Demote user {id}?")) {
        println!("Aborted.");
        return Ok(());
    }
    let resp = client
        .admin_post(&format!("/users/{id}/demote"))
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

async fn delete(client: &GatewayClient, id: Uuid, json: bool, dry_run: bool) -> Result<()> {
    if dry_run {
        crate::output::dry_run_notice("user", &id.to_string(), "delete");
        return Ok(());
    }
    if !crate::output::confirm(&format!("Delete user {id}?")) {
        println!("Aborted.");
        return Ok(());
    }
    let resp = client
        .admin_delete(&format!("/users/{id}"))
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
