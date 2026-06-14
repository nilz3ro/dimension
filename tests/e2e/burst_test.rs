//! Burst traffic e2e test — sends N concurrent messages to the Dimension gateway
//! and reports session IDs, worker routing, VM lifecycle, and response content.
//!
//! Usage:
//!   cargo run --release -p burst-test
//!
//! Environment:
//!   DIMENSION_URL     — gateway URL (default: http://192.168.12.165:3000)
//!   DIMENSION_TOKEN   — API key for auth
//!   DIMENSION_BUNDLE  — bundle to target (default: dimension-planner)
//!   BURST_COUNT       — number of parallel requests (default: 20)

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Instant;

use futures::stream::{self, StreamExt};
use reqwest::Client;
use serde_json::json;
use tokio::sync::Mutex;

#[derive(Debug)]
struct RequestResult {
    id: u32,
    success: bool,
    duration_ms: u128,
    session_id: Option<String>,
    request_id: Option<String>,
    worker: Option<String>,
    vm_duration_ms: Option<u64>,
    bytes_streamed: Option<u64>,
    response_preview: Option<String>,
    error: Option<String>,
}

fn extract_field<'a>(text: &'a str, field: &str) -> Option<&'a str> {
    text.split(field)
        .nth(1)?
        .split('"')
        .nth(1)
}

fn extract_number(text: &str, field: &str) -> Option<u64> {
    let after = text.split(field).nth(1)?;
    let num_str: String = after.chars().skip_while(|c| !c.is_ascii_digit()).take_while(|c| c.is_ascii_digit()).collect();
    num_str.parse().ok()
}

#[tokio::main]
async fn main() {
    let url = std::env::var("DIMENSION_URL")
        .unwrap_or_else(|_| "http://192.168.12.165:3000".to_string());
    let token = std::env::var("DIMENSION_TOKEN").expect("DIMENSION_TOKEN is required");
    let bundle = std::env::var("DIMENSION_BUNDLE")
        .unwrap_or_else(|_| "dimension-planner".to_string());
    let count: u32 = std::env::var("BURST_COUNT")
        .unwrap_or_else(|_| "20".to_string())
        .parse()
        .expect("BURST_COUNT must be a number");

    let client = Client::new();
    let success_count = Arc::new(AtomicU32::new(0));
    let fail_count = Arc::new(AtomicU32::new(0));
    let results: Arc<Mutex<Vec<RequestResult>>> = Arc::new(Mutex::new(Vec::new()));

    println!("🚀 Burst test: {count} concurrent requests");
    println!("   Gateway: {url}");
    println!("   Bundle:  {bundle}");
    println!();

    let start = Instant::now();
    let tasks: Vec<u32> = (1..=count).collect();

    stream::iter(tasks)
        .for_each_concurrent(None, |i| {
            let client = client.clone();
            let url = url.clone();
            let token = token.clone();
            let bundle = bundle.clone();
            let success_count = success_count.clone();
            let fail_count = fail_count.clone();
            let results = results.clone();

            async move {
                let req_start = Instant::now();

                let body = json!({
                    "role": "user",
                    "content": [{"type": "text", "text": format!("Burst test {i}: respond with just 'OK {i}'")}],
                    "bundle_id": bundle,
                    "processing_timeout_secs": 30
                });

                let resp = client
                    .post(format!("{url}/messages"))
                    .header("Content-Type", "application/json")
                    .header("Authorization", format!("Bearer {token}"))
                    .json(&body)
                    .timeout(std::time::Duration::from_secs(30))
                    .send()
                    .await;

                let duration_ms = req_start.elapsed().as_millis();

                let mut result = RequestResult {
                    id: i,
                    success: false,
                    duration_ms,
                    session_id: None,
                    request_id: None,
                    worker: None,
                    vm_duration_ms: None,
                    bytes_streamed: None,
                    response_preview: None,
                    error: None,
                };

                match resp {
                    Ok(resp) => {
                        let body_text = resp.text().await.unwrap_or_default();

                        // Parse SSE events — each line is either "event: X" or "data: {json}"
                        // We only care about the data lines
                        for line in body_text.lines() {
                            let json_str = if let Some(d) = line.strip_prefix("data: ") {
                                d
                            } else if line.starts_with('{') {
                                line
                            } else {
                                continue;
                            };

                            // Try parse as JSON
                            if let Ok(val) = serde_json::from_str::<serde_json::Value>(json_str) {
                                if result.request_id.is_none() {
                                    result.request_id = val.get("request_id")
                                        .and_then(|v| v.as_str())
                                        .map(|s| s.to_string());
                                }

                                match val.get("type").and_then(|v| v.as_str()) {
                                    Some("done") => {
                                        result.success = true;
                                        result.vm_duration_ms = val.get("duration_ms").and_then(|v| v.as_u64());
                                        result.bytes_streamed = val.get("bytes_streamed").and_then(|v| v.as_u64());
                                    }
                                    Some("message") => {
                                        if result.response_preview.is_none() {
                                            result.response_preview = val.get("content")
                                                .and_then(|v| v.as_str())
                                                .map(|s| s.chars().take(80).collect());
                                        }
                                    }
                                    Some("error") => {
                                        result.error = val.get("message")
                                            .and_then(|v| v.as_str())
                                            .map(|s| s.to_string());
                                    }
                                    Some("status") => {
                                        // Extract session_id if present
                                        if result.session_id.is_none() {
                                            result.session_id = val.get("session_id")
                                                .and_then(|v| v.as_str())
                                                .map(|s| s.to_string());
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }

                        if result.success {
                            success_count.fetch_add(1, Ordering::Relaxed);
                        } else if result.error.is_some() {
                            fail_count.fetch_add(1, Ordering::Relaxed);
                        } else {
                            fail_count.fetch_add(1, Ordering::Relaxed);
                            result.error = Some("no done event received".to_string());
                        }
                    }
                    Err(e) => {
                        fail_count.fetch_add(1, Ordering::Relaxed);
                        result.error = Some(e.to_string());
                    }
                }

                // Print live progress
                if result.success {
                    println!("  ✅ #{:>2} — {:>5}ms — VM {:>4}ms — {} bytes",
                        i, duration_ms,
                        result.vm_duration_ms.unwrap_or(0),
                        result.bytes_streamed.unwrap_or(0));
                } else {
                    println!("  ❌ #{:>2} — {:>5}ms — {}",
                        i, duration_ms,
                        result.error.as_deref().unwrap_or("unknown"));
                }

                results.lock().await.push(result);
            }
        })
        .await;

    let total_ms = start.elapsed().as_millis();
    let s = success_count.load(Ordering::Relaxed);
    let f = fail_count.load(Ordering::Relaxed);

    // Sort results by ID for clean output
    let mut all_results = results.lock().await;
    all_results.sort_by_key(|r| r.id);

    println!();
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  RESULTS: {s}/{count} succeeded, {f} failed");
    println!("  Total time: {:.1}s", total_ms as f64 / 1000.0);
    println!("  Throughput: {:.1} req/s", count as f64 / (total_ms as f64 / 1000.0));
    println!();

    // Session summary
    let sessions: Vec<&str> = all_results.iter()
        .filter_map(|r| r.session_id.as_deref())
        .collect();
    println!("  Sessions created: {} unique", {
        let mut s = sessions.clone();
        s.sort();
        s.dedup();
        s.len()
    });

    // VM duration stats
    let vm_durations: Vec<u64> = all_results.iter()
        .filter_map(|r| r.vm_duration_ms)
        .collect();
    if !vm_durations.is_empty() {
        let min = vm_durations.iter().min().unwrap();
        let max = vm_durations.iter().max().unwrap();
        let avg = vm_durations.iter().sum::<u64>() / vm_durations.len() as u64;
        println!("  VM duration: min={min}ms avg={avg}ms max={max}ms");
    }

    println!();
    println!("  Per-request detail:");
    for r in all_results.iter() {
        let status = if r.success { "✅" } else { "❌" };
        let session = r.session_id.as_deref().unwrap_or("-");
        let preview = r.response_preview.as_deref().unwrap_or(
            r.error.as_deref().unwrap_or("-")
        );
        println!("    {status} #{:>2} | session: {} | vm: {:>4}ms | {}",
            r.id,
            &session[..8.min(session.len())],
            r.vm_duration_ms.unwrap_or(0),
            preview);
    }
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    if f > 0 {
        std::process::exit(1);
    }
}
