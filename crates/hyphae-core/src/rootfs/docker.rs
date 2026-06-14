//! Docker image interaction via bollard.
//!
//! Provides functions to build Docker images from Dockerfiles, inspect images,
//! export their filesystems, and merge entrypoint/cmd metadata for rootfs building.

use std::path::Path;
use std::process::Command;

use bollard::container::{Config, CreateContainerOptions, RemoveContainerOptions};
use bollard::Docker;
use futures_util::StreamExt;
use hyphae_errors::RootfsError;

/// Metadata extracted from a Docker image config.
pub struct DockerImageMeta {
    pub entrypoint: Option<Vec<String>>,
    pub cmd: Option<Vec<String>>,
    pub env: Vec<String>,
    pub working_dir: Option<String>,
}

/// Build a Docker image from a Dockerfile.
///
/// Runs `docker build --platform linux/amd64 -f <dockerfile> <context>` and
/// returns the image tag used. The caller can then inspect/export the image
/// via [`inspect_image`] and [`export_image_to_staging`].
pub fn build_image(
    dockerfile: &Path,
    context: &Path,
    tag: &str,
) -> Result<String, RootfsError> {
    let dockerfile = dockerfile.canonicalize().map_err(|e| {
        RootfsError::DockerApi(format!(
            "failed to resolve Dockerfile path {}: {e}",
            dockerfile.display()
        ))
    })?;

    let context = context.canonicalize().map_err(|e| {
        RootfsError::DockerApi(format!(
            "failed to resolve context path {}: {e}",
            context.display()
        ))
    })?;

    let output = Command::new("docker")
        .arg("build")
        .arg("--platform")
        .arg("linux/amd64")
        .arg("-f")
        .arg(&dockerfile)
        .arg("-t")
        .arg(tag)
        .arg(&context)
        .output()
        .map_err(|e| {
            RootfsError::DockerApi(format!(
                "failed to run `docker build`: {e}. Is Docker installed and running?"
            ))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(RootfsError::DockerApi(format!(
            "docker build failed:\n{stderr}"
        )));
    }

    Ok(tag.to_string())
}

/// Inspect a Docker image and return its metadata.
pub async fn inspect_image(image_ref: &str) -> Result<DockerImageMeta, RootfsError> {
    let docker = Docker::connect_with_local_defaults()
        .map_err(|e| RootfsError::DockerApi(format!("failed to connect to Docker: {e}")))?;

    let image = docker
        .inspect_image(image_ref)
        .await
        .map_err(|e| RootfsError::DockerApi(format!("failed to inspect image {image_ref}: {e}")))?;

    let config = image.config.unwrap_or_default();

    Ok(DockerImageMeta {
        entrypoint: config.entrypoint,
        cmd: config.cmd,
        env: config.env.unwrap_or_default(),
        working_dir: config.working_dir.filter(|s| !s.is_empty()),
    })
}

/// Export a Docker image's filesystem into a staging directory.
///
/// Creates a throwaway container (never started), exports its root filesystem
/// as a tar stream, extracts it to `staging_dir`, then removes the container.
pub async fn export_image_to_staging(
    image_ref: &str,
    staging_dir: &Path,
) -> Result<(), RootfsError> {
    let docker = Docker::connect_with_local_defaults()
        .map_err(|e| RootfsError::DockerApi(format!("failed to connect to Docker: {e}")))?;

    // Create throwaway container (never started)
    let container_name = format!("hyphae-export-{}", uuid::Uuid::new_v4().simple());
    let container = docker
        .create_container(
            Some(CreateContainerOptions {
                name: container_name,
                platform: None,
            }),
            Config {
                image: Some(image_ref.to_string()),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| RootfsError::DockerApi(format!("failed to create container: {e}")))?;

    let container_id = container.id;

    // Export entire container filesystem as tar stream
    let mut stream = docker.export_container(&container_id);

    let mut tar_bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk
            .map_err(|e| RootfsError::DockerExportFailed(format!("stream error: {e}")))?;
        tar_bytes.extend_from_slice(&chunk);
    }

    // Unpack tar into staging directory
    let staging_dir = staging_dir.to_path_buf();
    let mut archive = tar::Archive::new(&tar_bytes[..]);
    archive.set_preserve_permissions(true);
    archive
        .unpack(&staging_dir)
        .map_err(|e| RootfsError::DockerExportFailed(format!("tar unpack failed: {e}")))?;

    // Clean up container
    let _ = docker
        .remove_container(
            &container_id,
            Some(RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        )
        .await;

    Ok(())
}

/// Build hyphae-init for x86_64-unknown-linux-musl inside a Docker container.
///
/// Used on non-Linux hosts (macOS) where the embedded init binary is a
/// placeholder. Mounts the workspace root into a Rust container and
/// cross-compiles hyphae-init, returning the built binary bytes.
///
/// The container runs on the host's native architecture (no QEMU emulation)
/// and installs musl cross-compilation tools to produce an x86_64 binary.
pub fn build_init_via_docker(workspace_root: &Path) -> Result<Vec<u8>, RootfsError> {
    let workspace_root = workspace_root.canonicalize().map_err(|e| {
        RootfsError::DockerApi(format!(
            "failed to resolve workspace root {}: {e}",
            workspace_root.display()
        ))
    })?;

    // Detect host architecture for the Docker platform flag.
    // We run the builder container natively (not under QEMU) and cross-compile
    // for x86_64-musl inside it. hyphae-init only depends on `libc` (Rust
    // bindings) so rust-lld can link it without a C cross-toolchain.
    let host_platform = detect_host_docker_platform();

    let output = Command::new("docker")
        .arg("run")
        .arg("--rm")
        .arg("--platform")
        .arg(&host_platform)
        .arg("-v")
        .arg(format!("{}:/src:ro", workspace_root.display()))
        .arg("-w")
        .arg("/src")
        .arg("rust:slim")
        .arg("sh")
        .arg("-c")
        .arg(
            "rustup target add x86_64-unknown-linux-musl >/dev/null 2>&1 && \
             CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld \
             cargo build --release --target x86_64-unknown-linux-musl -p hyphae-init \
               --target-dir /tmp/hyphae-build >/dev/null 2>&1 && \
             cat /tmp/hyphae-build/x86_64-unknown-linux-musl/release/hyphae-init",
        )
        .output()
        .map_err(|e| {
            RootfsError::DockerApi(format!(
                "failed to run Docker for hyphae-init build: {e}"
            ))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(RootfsError::DockerApi(format!(
            "hyphae-init Docker build failed:\n{stderr}"
        )));
    }

    // The binary is on stdout (from `cat`)
    if output.stdout.len() < 4 || output.stdout[..4] != [0x7f, b'E', b'L', b'F'] {
        return Err(RootfsError::DockerApi(
            "hyphae-init Docker build produced invalid output (not ELF)".to_string(),
        ));
    }

    Ok(output.stdout)
}

/// Build dimension-agent for x86_64-unknown-linux-musl inside a Docker container.
///
/// Same approach as [`build_init_via_docker`] — used on non-Linux hosts where
/// the compile-time embedded dimension-agent is a placeholder.
pub fn build_agent_via_docker(workspace_root: &Path) -> Result<Vec<u8>, RootfsError> {
    let workspace_root = workspace_root.canonicalize().map_err(|e| {
        RootfsError::DockerApi(format!(
            "failed to resolve workspace root {}: {e}",
            workspace_root.display()
        ))
    })?;

    let host_platform = detect_host_docker_platform();

    let output = Command::new("docker")
        .arg("run")
        .arg("--rm")
        .arg("--platform")
        .arg(&host_platform)
        .arg("-v")
        .arg(format!("{}:/src:ro", workspace_root.display()))
        .arg("-w")
        .arg("/src")
        .arg("rust:slim")
        .arg("sh")
        .arg("-c")
        .arg(
            "apt-get update -qq && apt-get install -y -qq protobuf-compiler >/dev/null 2>&1 && \
             rustup target add x86_64-unknown-linux-musl >/dev/null 2>&1 && \
             CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld \
             cargo build --release --target x86_64-unknown-linux-musl -p dimension-agent \
               --target-dir /tmp/hyphae-build >/dev/null 2>&1 && \
             cat /tmp/hyphae-build/x86_64-unknown-linux-musl/release/dimension-agent",
        )
        .output()
        .map_err(|e| {
            RootfsError::DockerApi(format!(
                "failed to run Docker for dimension-agent build: {e}"
            ))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(RootfsError::DockerApi(format!(
            "dimension-agent Docker build failed:\n{stderr}"
        )));
    }

    if output.stdout.len() < 4 || output.stdout[..4] != [0x7f, b'E', b'L', b'F'] {
        return Err(RootfsError::DockerApi(
            "dimension-agent Docker build produced invalid output (not ELF)".to_string(),
        ));
    }

    Ok(output.stdout)
}

/// Detect the host's native Docker platform (e.g., "linux/arm64" or "linux/amd64").
///
/// Uses `uname -m` to detect architecture and maps to Docker platform strings.
/// Falls back to "linux/amd64" if detection fails.
fn detect_host_docker_platform() -> String {
    let output = Command::new("uname").arg("-m").output().ok();
    let arch = output
        .as_ref()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();

    match arch.as_str() {
        "arm64" | "aarch64" => "linux/arm64".to_string(),
        "x86_64" | "amd64" => "linux/amd64".to_string(),
        _ => "linux/amd64".to_string(),
    }
}

/// Merge Docker Entrypoint and Cmd into a single command vector.
///
/// Docker semantics:
/// - Entrypoint + Cmd: entrypoint items followed by cmd items
/// - Entrypoint only: use as-is
/// - Cmd only: use as-is
/// - Neither: error
pub fn merge_entrypoint_cmd(meta: &DockerImageMeta) -> Result<Vec<String>, RootfsError> {
    match (&meta.entrypoint, &meta.cmd) {
        (Some(ep), Some(cmd)) => {
            let mut merged = ep.clone();
            merged.extend(cmd.iter().cloned());
            Ok(merged)
        }
        (Some(ep), None) => Ok(ep.clone()),
        (None, Some(cmd)) => Ok(cmd.clone()),
        (None, None) => Err(RootfsError::DockerNoEntrypoint),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_entrypoint_and_cmd() {
        let meta = DockerImageMeta {
            entrypoint: Some(vec!["/bin/sh".to_string(), "-c".to_string()]),
            cmd: Some(vec!["echo hello".to_string()]),
            env: vec![],
            working_dir: None,
        };
        let result = merge_entrypoint_cmd(&meta).unwrap();
        assert_eq!(result, vec!["/bin/sh", "-c", "echo hello"]);
    }

    #[test]
    fn merge_entrypoint_only() {
        let meta = DockerImageMeta {
            entrypoint: Some(vec!["/app/server".to_string()]),
            cmd: None,
            env: vec![],
            working_dir: None,
        };
        let result = merge_entrypoint_cmd(&meta).unwrap();
        assert_eq!(result, vec!["/app/server"]);
    }

    #[test]
    fn merge_cmd_only() {
        let meta = DockerImageMeta {
            entrypoint: None,
            cmd: Some(vec!["/app/server".to_string()]),
            env: vec![],
            working_dir: None,
        };
        let result = merge_entrypoint_cmd(&meta).unwrap();
        assert_eq!(result, vec!["/app/server"]);
    }

    #[test]
    fn merge_neither_returns_error() {
        let meta = DockerImageMeta {
            entrypoint: None,
            cmd: None,
            env: vec![],
            working_dir: None,
        };
        let result = merge_entrypoint_cmd(&meta);
        assert!(result.is_err());
    }
}
