//! Health check command.

use anyhow::Result;

use crate::client::GatewayClient;

/// Run the health command: GET /admin/health and display service status.
///
/// In --json mode, prints the full response. In table mode, prints a
/// summary header followed by a per-service table.
///
/// Exits with code 1 if the overall status is "degraded".
pub async fn run(client: &GatewayClient, json: bool) -> Result<()> {
    let resp = client.admin_get("/health").send().await?;
    let resp = GatewayClient::check_response(resp).await?;
    let body: serde_json::Value = resp.json().await?;

    if json {
        crate::output::print_json(&body);
        check_degraded_exit(&body);
        return Ok(());
    }

    let status = body["status"].as_str().unwrap_or("unknown");
    let version = body["version"].as_str().unwrap_or("-");
    let uptime = body["uptime_secs"]
        .as_i64()
        .map(|s| format!("{s}s"))
        .unwrap_or_else(|| "-".to_string());

    println!("Overall: {status}");
    println!("Version: {version}  Uptime: {uptime}");

    let services = body["services"].as_array().cloned().unwrap_or_default();
    if !services.is_empty() {
        let rows: Vec<Vec<String>> = services
            .iter()
            .map(|s| {
                vec![
                    s["name"].as_str().unwrap_or("-").to_string(),
                    s["status"].as_str().unwrap_or("-").to_string(),
                    s["detail"].as_str().unwrap_or("").to_string(),
                ]
            })
            .collect();
        crate::output::print_table(&["SERVICE", "STATUS", "DETAIL"], rows);
    }

    check_degraded_exit(&body);
    Ok(())
}

/// Exit with code 1 if the overall status is "degraded".
fn check_degraded_exit(body: &serde_json::Value) {
    if body["status"].as_str() == Some("degraded") {
        std::process::exit(1);
    }
}
