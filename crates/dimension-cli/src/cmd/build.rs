//! `dimension build` — Build a bundle from a Dockerfile.
//!
//! ```sh
//! dimension build -f Dockerfile --name my-agent           # build only
//! dimension build -f Dockerfile --name my-agent --push    # build and push
//! ```
//!
//! Internally this:
//! 1. Builds a Docker image from the Dockerfile (`docker build --platform linux/amd64`)
//! 2. Exports the image filesystem and injects Dimension runtime files
//! 3. Creates an ext4 rootfs bundle (saved to `./bundle.ext4` or `--output`)
//! 4. If `--push` is passed, uploads the bundle to the gateway

use std::path::PathBuf;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use hyphae_core::rootfs::{BuildConfig, build_rootfs};

use crate::client::GatewayClient;
use crate::cmd::push::upload_bundle;

/// Build a bundle from a Dockerfile.
///
/// Write a Dockerfile, point this command at it, and the ext4 bundle is
/// produced locally. Add `--push` to also upload it to the gateway.
#[derive(clap::Parser, Debug)]
pub struct BuildCmd {
    /// Path to the Dockerfile
    #[arg(short = 'f', long = "dockerfile", default_value = "./Dockerfile")]
    dockerfile: PathBuf,

    /// Build context directory (defaults to the Dockerfile's parent directory)
    #[arg(long)]
    context: Option<PathBuf>,

    /// Bundle name (required)
    #[arg(long)]
    pub name: String,

    /// Version tag
    #[arg(long, default_value = "latest")]
    pub tag: String,

    /// Save the ext4 bundle to this path (default: ./bundle.ext4)
    #[arg(long)]
    output: Option<PathBuf>,

    /// Also push the bundle to the gateway after building
    #[arg(long)]
    push: bool,

    /// Embed the dimension-agent sidecar in the bundle (default: true)
    #[arg(long, default_value_t = true)]
    embed_agent: bool,
}

/// Run the `dimension build` command.
pub async fn run(cmd: BuildCmd, client: &GatewayClient) -> Result<()> {
    let dockerfile = &cmd.dockerfile;
    if !dockerfile.exists() {
        anyhow::bail!(
            "Dockerfile not found: {}\nCreate a Dockerfile or use -f to specify its path.",
            dockerfile.display()
        );
    }

    // Resolve build context: explicit --context, or the Dockerfile's parent dir.
    let context = cmd.context.clone().unwrap_or_else(|| {
        dockerfile
            .parent()
            .map(|p| if p.as_os_str().is_empty() { PathBuf::from(".") } else { p.to_path_buf() })
            .unwrap_or_else(|| PathBuf::from("."))
    });

    if !context.is_dir() {
        anyhow::bail!(
            "Build context is not a directory: {}",
            context.display()
        );
    }

    // ── Step 1: Build Docker image ───────────────────────────────────────────
    let image_tag = format!("dimension-build-{}:{}", cmd.name, cmd.tag);

    println!("Building {}...", cmd.name);
    println!("  Dockerfile: {}", dockerfile.display());
    println!("  Context:    {}", context.display());
    println!();

    hyphae_core::rootfs::docker::build_image(dockerfile, &context, &image_tag)
        .context("Docker build failed")?;

    println!();
    println!("Docker image built: {image_tag}");

    // ── Step 2: Convert to ext4 rootfs ───────────────────────────────────────
    let output_path = cmd
        .output
        .clone()
        .unwrap_or_else(|| PathBuf::from("bundle.ext4"));

    let config = BuildConfig {
        project_dir: PathBuf::from("."),
        output_path: output_path.clone(),
        size_override: None,
        embed_dimension_agent: cmd.embed_agent,
        binary_path: None,
        entrypoint: None,
        docker_image: Some(image_tag.clone()),
    };

    println!("Creating bundle...");

    let result = build_rootfs(&config).context("rootfs build failed")?;

    let hash = {
        let ext4_bytes = std::fs::read(&result.image_path)
            .with_context(|| format!("failed to read bundle at {}", result.image_path.display()))?;
        let h = Sha256::digest(&ext4_bytes);
        hex::encode(h)
    };

    println!("  Size:    {} bytes", result.image_size);
    println!("  Hash:    sha256:{hash}");
    println!("  Output:  {}", result.image_path.display());

    if result.manifest.is_some() {
        println!("  Config:  dimension.toml detected");
    }

    // ── Step 3 (optional): Push to gateway ──────────────────────────────────
    if cmd.push {
        let file_size = result.image_size;
        println!();
        println!("Pushing {} ({})...", cmd.name, bytesize::ByteSize(file_size));

        // Forward the manifest we just parsed during the build so the gateway
        // stores the bundle with the correct [resources]/[env]/[capabilities].
        let manifest_json = result
            .manifest
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .context("failed to serialize manifest")?;

        let push_resp = upload_bundle(
            client,
            &result.image_path,
            &cmd.name,
            &cmd.tag,
            manifest_json.as_deref(),
        )
        .await?;

        println!();
        println!("Deployed!");
        println!("  Name:    {}", cmd.name);
        println!("  Tag:     {}", cmd.tag);
        println!("  ID:      {}", push_resp.bundle_id);
        println!("  Hash:    {}", push_resp.content_hash);
        println!("  Status:  {}", push_resp.status);
    }

    // Clean up the temp Docker image (best-effort).
    let _ = std::process::Command::new("docker")
        .arg("rmi")
        .arg(&image_tag)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    Ok(())
}
