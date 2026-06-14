//! `dimension push` — upload a pre-built ext4 rootfs to the gateway.
//!
//! Streams the ext4 file from disk, uploads it via multipart POST to
//! `/bundles/push` with name, tag, and (optionally) the bundle manifest.
//! Shows real-time progress with indicatif and prints bundle ID + content
//! hash on success.
//!
//! The streaming upload + manifest-forwarding logic is exposed via
//! [`upload_bundle`] so that `dimension build --push` shares the exact same
//! wire shape — preventing the two paths from drifting (in particular, both
//! must forward the manifest, otherwise the gateway falls back to default
//! resource/env settings and the bundle silently misbehaves at run time).

use std::path::Path;

use anyhow::{Context, Result};
use clap::Args;
use futures::TryStreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::{Body, multipart};
use serde::Deserialize;
use tokio_util::codec::{BytesCodec, FramedRead};

use hyphae_core::manifest::parse_manifest;

use crate::client::GatewayClient;

/// Push a pre-built ext4 rootfs to the gateway.
#[derive(Debug, Args)]
pub struct PushCmd {
    /// Path to the ext4 rootfs file to push
    #[arg(default_value = "./bundle.ext4")]
    pub file: String,

    /// Bundle name
    #[arg(long)]
    pub name: String,

    /// Version tag (defaults to "latest")
    #[arg(long, default_value = "latest")]
    pub tag: String,

    /// Path to a dimension.toml manifest to forward with the upload.
    /// Defaults to ./dimension.toml when present; pass --no-manifest to skip.
    #[arg(long)]
    pub manifest: Option<String>,

    /// Skip forwarding any dimension.toml manifest, even if ./dimension.toml exists.
    #[arg(long, conflicts_with = "manifest")]
    pub no_manifest: bool,
}

/// Response from POST /bundles/push.
#[derive(Debug, Deserialize)]
pub struct PushResponse {
    pub bundle_id: i64,
    pub content_hash: String,
    pub status: String,
}

/// Execute the push command.
pub async fn run(cmd: PushCmd, client: &GatewayClient) -> Result<()> {
    let file_path = Path::new(&cmd.file);
    if !file_path.exists() {
        anyhow::bail!(
            "File not found: {}\nRun `dimension build` first to produce an ext4 rootfs.",
            cmd.file
        );
    }

    let manifest_json = resolve_manifest_json(cmd.manifest.as_deref(), cmd.no_manifest)?;
    if !cmd.no_manifest && manifest_json.is_none() {
        eprintln!(
            "warning: no dimension.toml found (looked at ./dimension.toml). \
             Pushing without a manifest — the gateway will fall back to default \
             resources and the bundle's [env] block will be ignored at run time. \
             Pass --manifest <path> or run from a directory that contains \
             dimension.toml to fix this."
        );
    }

    let push_resp = upload_bundle(
        client,
        file_path,
        &cmd.name,
        &cmd.tag,
        manifest_json.as_deref(),
    )
    .await?;

    println!();
    println!("  Bundle pushed successfully!");
    println!("  ├─ Bundle ID:    {}", push_resp.bundle_id);
    println!("  ├─ Name:         {}", cmd.name);
    println!("  ├─ Tag:          {}", cmd.tag);
    println!("  ├─ Content Hash: {}", push_resp.content_hash);
    println!("  └─ Status:       {}", push_resp.status);

    Ok(())
}

/// Upload a bundle file to the gateway with a streaming progress bar, optionally
/// forwarding a serialized `dimension.toml` manifest.
///
/// Both `dimension push` and `dimension build --push` call this. Keeping the
/// upload in one place ensures both paths stay in sync — the gateway needs the
/// manifest to honor `[resources]` / `[env]` / `[capabilities]`, and the
/// progress bar should reflect actual bytes-on-the-wire (not a pre-buffered blob).
pub async fn upload_bundle(
    client: &GatewayClient,
    file_path: &Path,
    name: &str,
    tag: &str,
    manifest_json: Option<&str>,
) -> Result<PushResponse> {
    let file_meta = tokio::fs::metadata(file_path)
        .await
        .with_context(|| format!("failed to stat {}", file_path.display()))?;
    let file_size = file_meta.len();

    let pb = ProgressBar::new(file_size);
    pb.set_style(
        ProgressStyle::with_template(
            "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})",
        )
        .unwrap()
        .progress_chars("#>-"),
    );

    let file = tokio::fs::File::open(file_path)
        .await
        .with_context(|| format!("failed to open {}", file_path.display()))?;

    let reader = FramedRead::new(file, BytesCodec::new());
    let pb_clone = pb.clone();
    let stream = reader.map_ok(move |chunk| {
        pb_clone.inc(chunk.len() as u64);
        chunk.freeze()
    });
    let body = Body::wrap_stream(stream);

    let file_part = multipart::Part::stream_with_length(body, file_size)
        .file_name(
            file_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string(),
        )
        .mime_str("application/octet-stream")?;

    let mut form = multipart::Form::new()
        .part("file", file_part)
        .text("name", name.to_string())
        .text("tag", tag.to_string());

    if let Some(manifest) = manifest_json {
        if std::env::var("DIMENSION_DEBUG_MANIFEST").is_ok() {
            eprintln!("[debug] forwarding manifest JSON: {manifest}");
        }
        form = form.text("manifest", manifest.to_string());
    } else if std::env::var("DIMENSION_DEBUG_MANIFEST").is_ok() {
        eprintln!("[debug] no manifest forwarded with this push");
    }

    let resp = client
        .user_post("/bundles/push")
        .timeout(std::time::Duration::from_secs(600))
        .multipart(form)
        .send()
        .await
        .context("failed to connect to gateway")?;

    pb.finish_with_message("Upload complete");

    let resp = GatewayClient::check_response(resp).await?;
    let push_resp: PushResponse = resp.json().await.context("failed to parse push response")?;
    Ok(push_resp)
}

/// Resolve which manifest JSON (if any) to forward with the upload.
///
/// - `no_manifest = true` → always `None`.
/// - explicit `manifest_path` → required to exist; parsed and serialized.
/// - otherwise → `./dimension.toml` if present; missing is not an error.
fn resolve_manifest_json(manifest_path: Option<&str>, no_manifest: bool) -> Result<Option<String>> {
    if no_manifest {
        return Ok(None);
    }

    let path = match manifest_path {
        Some(p) => Path::new(p).to_path_buf(),
        None => {
            let default = Path::new("./dimension.toml");
            if !default.exists() {
                return Ok(None);
            }
            default.to_path_buf()
        }
    };

    if !path.exists() {
        anyhow::bail!("manifest file not found: {}", path.display());
    }

    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read manifest {}", path.display()))?;
    let mut manifest = parse_manifest(&content)
        .with_context(|| format!("failed to parse manifest {}", path.display()))?;

    // Resolve `${VAR}` references in [env] against the deployer's shell, so
    // the gateway stores concrete values (not literal `${VAR}` strings that
    // would silently leak into the guest VM). build_rootfs already does this
    // for the build --push path; we do it here for the standalone push path.
    let env_before = manifest.env.vars.clone();
    manifest.env.resolve_host_env();
    for (key, raw) in &env_before {
        let trimmed = raw.trim();
        if trimmed.starts_with("${") && trimmed.ends_with('}') && trimmed.len() > 3 {
            let var_name = &trimmed[2..trimmed.len() - 1];
            if std::env::var(var_name).is_err() {
                eprintln!(
                    "warning: manifest [env].{key} references ${{{var_name}}} but \
                     {var_name} is not set in the deployer's shell — uploading an \
                     empty string. Use `dotenv -e .env -- dimension push ...` or \
                     export {var_name} before pushing."
                );
            }
        }
    }

    let json = serde_json::to_string(&manifest)
        .with_context(|| format!("failed to serialize manifest {}", path.display()))?;
    Ok(Some(json))
}
