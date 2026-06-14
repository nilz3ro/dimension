//! Task management subcommands.

use anyhow::Result;
use clap::Subcommand;
use uuid::Uuid;

use crate::client::GatewayClient;

#[derive(Subcommand, Debug)]
pub enum TasksCmd {
    /// List all tasks (admin: all users)
    List {
        /// Filter by status (e.g. pending, running, completed, failed, cancelled)
        #[arg(long)]
        status: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Inspect a task
    Inspect {
        /// Task ID
        task_id: Uuid,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Cancel a running or pending task
    Cancel {
        /// Task ID
        task_id: Uuid,
        /// Show what would happen without making changes
        #[arg(long)]
        dry_run: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Retry a failed or cancelled task
    Retry {
        /// Task ID
        task_id: Uuid,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// View run history for a task
    Runs {
        /// Task ID
        task_id: Uuid,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

pub async fn run(client: &GatewayClient, cmd: TasksCmd) -> Result<()> {
    match cmd {
        TasksCmd::List { status, json } => list(client, status, json).await,
        TasksCmd::Inspect { task_id, json } => inspect(client, task_id, json).await,
        TasksCmd::Cancel {
            task_id,
            dry_run,
            json,
        } => cancel(client, task_id, dry_run, json).await,
        TasksCmd::Retry { task_id, json } => retry(client, task_id, json).await,
        TasksCmd::Runs { task_id, json } => runs(client, task_id, json).await,
    }
}

async fn list(client: &GatewayClient, status: Option<String>, json: bool) -> Result<()> {
    let path = match &status {
        Some(s) => format!("/tasks?status={s}"),
        None => "/tasks".to_string(),
    };
    let resp = client.admin_get(&path).send().await?;
    let resp = GatewayClient::check_response(resp).await?;
    let body: serde_json::Value = resp.json().await?;

    if json {
        crate::output::print_json(&body);
        return Ok(());
    }

    let tasks = body["tasks"].as_array().cloned().unwrap_or_default();
    if tasks.is_empty() {
        println!("No tasks found.");
        return Ok(());
    }

    let rows: Vec<Vec<String>> = tasks
        .iter()
        .map(|t| {
            let id = t["id"].as_str().unwrap_or("-");
            let short_id = if id.len() > 8 { &id[..8] } else { id };
            let bundle = t["bundle_id"].as_str().unwrap_or("-");
            let short_bundle = if bundle.len() > 8 { &bundle[..8] } else { bundle };
            let status = t["status"].as_str().unwrap_or("-");
            let goal = t["goal"].as_str().unwrap_or("");
            let goal_truncated = truncate_str(goal, 40);
            let iters = t["current_iteration"]
                .as_i64()
                .map(|n| n.to_string())
                .unwrap_or_else(|| "-".to_string());
            let created = t["created_at"].as_str().unwrap_or("-");
            vec![
                short_id.to_string(),
                short_bundle.to_string(),
                status.to_string(),
                goal_truncated,
                iters,
                created.to_string(),
            ]
        })
        .collect();

    crate::output::print_table(&["ID", "BUNDLE", "STATUS", "GOAL", "ITERS", "CREATED"], rows);
    Ok(())
}

async fn inspect(client: &GatewayClient, task_id: Uuid, json: bool) -> Result<()> {
    // GET /tasks/{id} — user-scoped; admin token is accepted.
    let resp = client
        .user_get(&format!("/tasks/{task_id}"))
        .send()
        .await?;
    let resp = GatewayClient::check_response(resp).await?;
    let body: serde_json::Value = resp.json().await?;

    if json {
        crate::output::print_json(&body);
        return Ok(());
    }

    let t = if body.is_object() && body.get("id").is_some() {
        &body
    } else {
        body.get("task").unwrap_or(&body)
    };

    println!("ID:               {}", t["id"].as_str().unwrap_or("-"));
    println!("Bundle:           {}", t["bundle_id"].as_str().unwrap_or("-"));
    println!("Goal:             {}", t["goal"].as_str().unwrap_or("-"));
    println!("Status:           {}", t["status"].as_str().unwrap_or("-"));
    let cur = t["current_iteration"].as_i64().unwrap_or(0);
    let max = t["max_iterations"].as_i64().unwrap_or(0);
    println!("Iterations:       {cur}/{max}");
    if let Some(trigger) = t["trigger"].as_str() {
        println!("Trigger:          {trigger}");
    }
    if let Some(cron) = t["cron"].as_str() {
        println!("Cron:             {cron}");
    }
    if let Some(sc) = t["success_criteria"].as_str() {
        println!("Success Criteria: {sc}");
    }
    println!("Created:          {}", t["created_at"].as_str().unwrap_or("-"));
    if let Some(started) = t["started_at"].as_str() {
        println!("Started:          {started}");
    }
    if let Some(completed) = t["completed_at"].as_str() {
        println!("Completed:        {completed}");
    }
    Ok(())
}

async fn cancel(
    client: &GatewayClient,
    task_id: Uuid,
    dry_run: bool,
    json: bool,
) -> Result<()> {
    if dry_run {
        crate::output::dry_run_notice("task", &task_id.to_string(), "cancel");
        return Ok(());
    }
    if !crate::output::confirm(&format!("Cancel task {task_id}?")) {
        println!("Aborted.");
        return Ok(());
    }
    // POST /tasks/{id}/cancel — user-scoped; admin token is accepted.
    let resp = client
        .user_post(&format!("/tasks/{task_id}/cancel"))
        .send()
        .await?;
    let resp = GatewayClient::check_response(resp).await?;
    let body: serde_json::Value = resp.json().await?;

    if json {
        crate::output::print_json(&body);
    } else {
        println!("{}", body["message"].as_str().unwrap_or("task cancelled"));
    }
    Ok(())
}

async fn retry(client: &GatewayClient, task_id: Uuid, json: bool) -> Result<()> {
    // POST /admin/tasks/{id}/retry
    let resp = client
        .admin_post(&format!("/tasks/{task_id}/retry"))
        .send()
        .await?;
    let resp = GatewayClient::check_response(resp).await?;
    let body: serde_json::Value = resp.json().await?;

    if json {
        crate::output::print_json(&body);
    } else {
        println!("{}", body["message"].as_str().unwrap_or("task queued for retry"));
    }
    Ok(())
}

async fn runs(client: &GatewayClient, task_id: Uuid, json: bool) -> Result<()> {
    // GET /tasks/{id}/runs — user-scoped; admin token is accepted.
    let resp = client
        .user_get(&format!("/tasks/{task_id}/runs"))
        .send()
        .await?;
    let resp = GatewayClient::check_response(resp).await?;
    let body: serde_json::Value = resp.json().await?;

    if json {
        crate::output::print_json(&body);
        return Ok(());
    }

    let run_list = body.as_array().cloned().unwrap_or_default();
    if run_list.is_empty() {
        println!("No runs found.");
        return Ok(());
    }

    let rows: Vec<Vec<String>> = run_list
        .iter()
        .map(|r| {
            let id = r["id"].as_str().unwrap_or("-");
            let short_id = if id.len() > 8 { &id[..8] } else { id };
            let iter = r["iteration"]
                .as_i64()
                .map(|n| n.to_string())
                .unwrap_or_else(|| "-".to_string());
            let started = r["started_at"].as_str().unwrap_or("-");
            let completed = r["completed_at"].as_str().unwrap_or("-");
            let passed = match r["eval_passed"].as_bool() {
                Some(true) => "yes",
                Some(false) => "no",
                None => "-",
            }
            .to_string();
            let error = r["error"].as_str().unwrap_or("");
            let error_truncated = truncate_str(error, 40);
            vec![
                short_id.to_string(),
                iter,
                started.to_string(),
                completed.to_string(),
                passed,
                error_truncated,
            ]
        })
        .collect();

    crate::output::print_table(
        &["RUN", "ITERATION", "STARTED", "COMPLETED", "PASSED", "ERROR"],
        rows,
    );
    Ok(())
}

/// Truncate a string to `max_len` chars, appending "..." if truncated.
fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() > max_len {
        format!("{}...", &s[..max_len])
    } else {
        s.to_string()
    }
}
