//! Image sizing calculation, mkfs.ext4 invocation, and build_rootfs pipeline.
//!
//! This module ties together all rootfs components:
//! - Project type detection ([`detect`](super::detect))
//! - Staging directory creation ([`staging`](super::staging))
//! - Init binary embedding ([`init`](super::init))
//! - JS runtime preparation ([`js`](super::js))
//! - Rust runtime preparation ([`rust_build`](super::rust_build))
//!
//! The [`build_rootfs`] function orchestrates the full pipeline from a project
//! directory to a bootable ext4 image.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use walkdir::WalkDir;

use crate::manifest::{parse_manifest, DimensionManifest};
use hyphae_errors::RootfsError;

use crate::rootfs::binary::prepare_binary_runtime;
use crate::rootfs::detect::{ProjectType, detect_project_type};
use crate::rootfs::docker;
use crate::rootfs::init::{
    embed_dimension_agent, embed_dimension_agent_from_bytes, embed_init, embed_init_from_bytes,
    is_dimension_agent_real, is_init_real, validate_dimension_agent, validate_init,
    write_env_config,
};
use crate::rootfs::js::prepare_js_runtime;
use crate::rootfs::rust_build::prepare_rust_runtime;
use crate::rootfs::staging::create_directory_structure;

/// Headroom multiplier: 40% extra space beyond content size.
/// ext4 has significant metadata overhead (inodes, directory blocks, group
/// descriptors, journal) — 20% was insufficient for images with many small files.
const HEADROOM_PERCENT: f64 = 0.40;

/// Minimum image size: 16 MiB.
const MIN_IMAGE_SIZE: u64 = 16 * 1024 * 1024;

/// Block size for ext4 alignment (4 KiB).
const BLOCK_SIZE: u64 = 4096;

/// Extra inodes reserved for ext4 internal structures.
const INODE_OVERHEAD: u64 = 10;

/// Configuration for building a rootfs image.
pub struct BuildConfig {
    /// Path to the project directory (must contain package.json or Cargo.toml).
    pub project_dir: PathBuf,
    /// Path where the output ext4 image will be written.
    pub output_path: PathBuf,
    /// Optional size override in bytes. Bypasses automatic sizing when set.
    pub size_override: Option<u64>,
    /// Embed the dimension-agent binary in the rootfs. When present,
    /// `hyphae-init` will start it alongside the application entrypoint.
    pub embed_dimension_agent: bool,
    /// Path to a pre-built binary or directory (bypasses project detection).
    pub binary_path: Option<PathBuf>,
    /// Entrypoint command inside the VM (used with binary_path).
    pub entrypoint: Option<String>,
    /// Docker image reference to convert to a rootfs (bypasses project detection).
    pub docker_image: Option<String>,
}

/// Result of a successful rootfs build.
pub struct BuildResult {
    /// Path to the created ext4 image file.
    pub image_path: PathBuf,
    /// Detected project type.
    pub project_type: ProjectType,
    /// Total size of content in the staging directory (bytes).
    pub content_size: u64,
    /// Final image size (bytes).
    pub image_size: u64,
    /// Number of inodes allocated in the image.
    pub inode_count: u64,
    /// Number of inodes actually used (files + directories).
    pub inode_usage: u64,
    /// Parsed manifest from dimension.toml (if present in the project).
    pub manifest: Option<DimensionManifest>,
}

/// Image sizing calculation result.
pub struct ImageSizing {
    /// Total size of regular file content (bytes).
    pub content_size: u64,
    /// Final image size after headroom and floor (bytes).
    pub total_size: u64,
    /// Number of inodes to allocate (file_count + overhead).
    pub inode_count: u64,
    /// Number of filesystem entries (files + directories, excluding the root).
    pub file_count: u64,
}

/// Calculate image sizing by walking a staging directory.
///
/// Walks `staging_dir` to count files and directories and sum content sizes.
///
/// - `file_count` = number of files + directories (excluding the root directory itself)
/// - `inode_count` = file_count + 10 (overhead for ext4 internal inodes)
/// - `content_size` = sum of regular file sizes
/// - `total_size`:
///   - If `size_override` is `Some`, uses that value directly
///   - Otherwise: `max(ceil_to_block(content_size * 1.20), 16 MiB)`
pub fn calculate_image_sizing(
    staging_dir: &Path,
    size_override: Option<u64>,
) -> Result<ImageSizing, RootfsError> {
    let mut file_count: u64 = 0;
    let mut content_size: u64 = 0;

    for entry in WalkDir::new(staging_dir).min_depth(1) {
        let entry = entry.map_err(|e| RootfsError::StagingWalk(e.to_string()))?;
        file_count += 1;

        if entry.file_type().is_file() {
            content_size += entry.metadata()
                .map_err(|e| RootfsError::StagingWalk(e.to_string()))?
                .len();
        }
    }

    let inode_count = file_count + INODE_OVERHEAD;

    let total_size = match size_override {
        Some(override_size) => override_size,
        None => {
            let with_headroom = (content_size as f64 * (1.0 + HEADROOM_PERCENT)) as u64;
            let aligned = ceil_to_block(with_headroom);
            aligned.max(MIN_IMAGE_SIZE)
        }
    };

    Ok(ImageSizing {
        content_size,
        total_size,
        inode_count,
        file_count,
    })
}

/// Round `size` up to the next multiple of [`BLOCK_SIZE`].
fn ceil_to_block(size: u64) -> u64 {
    if size == 0 {
        return 0;
    }
    ((size + BLOCK_SIZE - 1) / BLOCK_SIZE) * BLOCK_SIZE
}

/// Directory that will hold the output image, for the pre-build disk-space check.
///
/// `Path::new("bundle.ext4").parent()` is `Some("")` (empty), not `None`, so a
/// bare relative filename would otherwise be passed straight to `statvfs`, which
/// fails with ENOENT. Treat an absent or empty parent as the current directory.
fn output_dir_of(output_path: &Path) -> &Path {
    match output_path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    }
}

/// Check that sufficient disk space is available at the given path.
///
/// Returns `Ok(())` if available space >= `required_bytes`, or a
/// [`RootfsError::InsufficientDisk`] if space is too low, or a
/// [`RootfsError::DiskSpaceCheck`] if the filesystem stat call fails.
pub fn check_disk_space(dir: &Path, required_bytes: u64) -> Result<(), RootfsError> {
    use nix::sys::statvfs::statvfs;
    let stat = statvfs(dir)
        .map_err(|e| RootfsError::DiskSpaceCheck(e.to_string()))?;
    let available = stat.blocks_available() as u64 * stat.fragment_size() as u64;
    if available < required_bytes {
        return Err(RootfsError::InsufficientDisk {
            required: required_bytes,
            available,
        });
    }
    Ok(())
}

/// Write `/etc/resolv.conf` into the staging directory with public DNS servers.
///
/// Writes `8.8.8.8` (Google) and `1.1.1.1` (Cloudflare) as nameservers so
/// that guest VMs can resolve hostnames at runtime. The `etc/` directory is
/// guaranteed to exist at this point (created by [`create_directory_structure`]).
pub fn write_resolv_conf(staging_dir: &Path) -> Result<(), RootfsError> {
    let resolv_conf = staging_dir.join("etc/resolv.conf");
    let mut file = std::fs::File::create(&resolv_conf)?;
    file.write_all(b"nameserver 8.8.8.8\nnameserver 1.1.1.1\n")?;
    Ok(())
}

/// Create an ext4 filesystem image from a staging directory.
///
/// Shells out to `mkfs.ext4` with flags:
/// - `-d staging_dir` (populate from directory)
/// - `-N inodes` (number of inodes)
/// - `-b 4096` (block size)
/// - `-m 0` (no reserved blocks)
/// - `-E root_owner=0:0` (root ownership)
/// - `-t ext4` (filesystem type)
/// - `output_path` (output file)
/// - `{size}K` (size in KiB)
///
/// On non-Linux hosts (macOS), falls back to running mkfs.ext4 inside a Docker
/// container when the native binary is not available.
pub fn create_ext4_image(
    output_path: &Path,
    staging_dir: &Path,
    sizing: &ImageSizing,
) -> Result<(), RootfsError> {
    // Check available disk space before creating image (FIX-08).
    check_disk_space(output_dir_of(output_path), sizing.total_size)?;

    // Try native mkfs.ext4 first; fall back to Docker on non-Linux hosts
    if which::which("mkfs.ext4").is_ok() {
        create_ext4_native(output_path, staging_dir, sizing)
    } else {
        eprintln!("mkfs.ext4 not found, using Docker fallback...");
        create_ext4_via_docker(output_path, staging_dir, sizing)
    }
}

/// Native mkfs.ext4 invocation (Linux hosts).
fn create_ext4_native(
    output_path: &Path,
    staging_dir: &Path,
    sizing: &ImageSizing,
) -> Result<(), RootfsError> {
    let size_kib = sizing.total_size / 1024;

    let output = Command::new("mkfs.ext4")
        .arg("-d")
        .arg(staging_dir)
        .arg("-N")
        .arg(sizing.inode_count.to_string())
        .arg("-b")
        .arg("4096")
        .arg("-m")
        .arg("0")
        .arg("-E")
        .arg("root_owner=0:0")
        .arg("-t")
        .arg("ext4")
        .arg(output_path)
        .arg(format!("{size_kib}K"))
        .output()
        .map_err(RootfsError::MkfsExec)?;

    if !output.status.success() {
        return Err(RootfsError::MkfsFailed {
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        });
    }

    Ok(())
}

/// Create an ext4 image using Docker (for macOS / non-Linux hosts).
///
/// Mounts the staging directory and output directory into a Debian container
/// with e2fsprogs installed, runs mkfs.ext4 inside it, and writes the result
/// to the host-mounted output path.
fn create_ext4_via_docker(
    output_path: &Path,
    staging_dir: &Path,
    sizing: &ImageSizing,
) -> Result<(), RootfsError> {
    let staging_dir = staging_dir.canonicalize().map_err(|e| {
        RootfsError::MkfsFailed {
            stderr: format!("failed to canonicalize staging dir: {e}"),
        }
    })?;

    let output_path_abs = if output_path.is_absolute() {
        output_path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| RootfsError::MkfsFailed {
                stderr: format!("failed to get cwd: {e}"),
            })?
            .join(output_path)
    };

    let output_dir = output_path_abs
        .parent()
        .ok_or_else(|| RootfsError::MkfsFailed {
            stderr: "output path has no parent directory".to_string(),
        })?
        .canonicalize()
        .map_err(|e| RootfsError::MkfsFailed {
            stderr: format!("failed to canonicalize output dir: {e}"),
        })?;

    let output_filename = output_path_abs
        .file_name()
        .ok_or_else(|| RootfsError::MkfsFailed {
            stderr: "output path has no filename".to_string(),
        })?
        .to_string_lossy()
        .to_string();

    let size_kib = sizing.total_size / 1024;

    let output = Command::new("docker")
        .arg("run")
        .arg("--rm")
        .arg("--platform")
        .arg("linux/amd64")
        .arg("-v")
        .arg(format!("{}:/staging:ro", staging_dir.display()))
        .arg("-v")
        .arg(format!("{}:/output", output_dir.display()))
        .arg("debian:bookworm-slim")
        .arg("sh")
        .arg("-c")
        .arg(format!(
            "apt-get update -qq && apt-get install -y -qq e2fsprogs >/dev/null 2>&1 && \
             mkfs.ext4 -d /staging -N {} -b 4096 -m 0 -E root_owner=0:0 -t ext4 /output/{} {}K",
            sizing.inode_count, output_filename, size_kib,
        ))
        .output()
        .map_err(|e| RootfsError::MkfsFailed {
            stderr: format!("failed to run Docker for mkfs.ext4: {e}"),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(RootfsError::MkfsFailed {
            stderr: format!("Docker mkfs.ext4 failed:\n{stderr}"),
        });
    }

    Ok(())
}

/// Build a rootfs ext4 image from a project directory.
///
/// Orchestrates the full pipeline:
/// 1. Detect project type (JavaScript or Rust)
/// 2. Create staging directory structure
/// 3. Prepare runtime (JS: embed Node.js + copy project; Rust: build + copy binary)
/// 4. Optionally embed dimension-agent binary
/// 5. Embed init binary and entrypoint config
/// 6. Validate init binary placement (and dimension-agent if embedded)
/// 7. Calculate image sizing
/// 8. Create ext4 filesystem image
/// 9. Return [`BuildResult`] with size breakdown
/// Resolve the workspace root from CARGO_MANIFEST_DIR (hyphae-core is at crates/hyphae-core/).
fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .unwrap_or(&manifest_dir)
        .to_path_buf()
}

/// Embed dimension-agent, building via Docker if the compile-time binary is a placeholder.
fn embed_agent_with_fallback(staging_dir: &Path) -> Result<(), RootfsError> {
    if is_dimension_agent_real() {
        embed_dimension_agent(staging_dir)
    } else {
        eprintln!("Building dimension-agent via Docker (non-Linux host)...");
        let agent_bytes = docker::build_agent_via_docker(&workspace_root())?;
        embed_dimension_agent_from_bytes(staging_dir, &agent_bytes)
    }
}

pub fn build_rootfs(config: &BuildConfig) -> Result<BuildResult, RootfsError> {
    // Docker mode: export Docker image filesystem to rootfs
    if let Some(ref docker_image) = config.docker_image {
        let staging = tempfile::tempdir()?;

        // Bridge async bollard calls into sync context
        let rt = tokio::runtime::Handle::try_current()
            .map_err(|e| RootfsError::DockerApi(format!("no tokio runtime: {e}")))?;

        let meta = tokio::task::block_in_place(|| {
            rt.block_on(docker::inspect_image(docker_image))
        })?;

        tokio::task::block_in_place(|| {
            rt.block_on(docker::export_image_to_staging(docker_image, staging.path()))
        })?;

        // Read and parse dimension.toml from the Docker image.
        // Check /etc/hyphae/dimension.toml first (standard location via Dockerfile COPY),
        // then fall back to root /dimension.toml for compatibility.
        let manifest = std::fs::read_to_string(staging.path().join("etc/hyphae/dimension.toml"))
            .or_else(|_| std::fs::read_to_string(staging.path().join("dimension.toml")))
            .ok()
            .and_then(|content| parse_manifest(&content).ok())
            .map(|mut m| { m.env.resolve_host_env(); m });

        // Ensure hyphae directories exist (idempotent, Docker fs may already have /etc)
        create_directory_structure(staging.path())?;

        // Write DNS configuration so the guest can resolve hostnames at runtime
        write_resolv_conf(staging.path())?;

        // Merge entrypoint + cmd
        let entrypoint_cmd = docker::merge_entrypoint_cmd(&meta)?;

        // Override entrypoint if user specified one
        let entrypoint_cmd = if let Some(ref user_ep) = config.entrypoint {
            vec![user_ep.clone()]
        } else {
            entrypoint_cmd
        };

        // Optionally embed dimension-agent
        if config.embed_dimension_agent {
            embed_agent_with_fallback(staging.path())?;
        }

        // Embed hyphae-init (overwrites any /sbin/init from the Docker image).
        // On non-Linux hosts the compile-time embedded binary is a placeholder,
        // so we build hyphae-init inside Docker instead.
        if is_init_real() {
            embed_init(staging.path(), &entrypoint_cmd)?;
        } else {
            eprintln!("Building hyphae-init via Docker (non-Linux host)...");
            let init_bytes = docker::build_init_via_docker(&workspace_root())?;
            embed_init_from_bytes(staging.path(), &init_bytes, &entrypoint_cmd)?;
        }

        // Write env vars and workdir from Docker image config
        write_env_config(staging.path(), &meta.env, meta.working_dir.as_deref())?;

        validate_init(staging.path())?;
        if config.embed_dimension_agent {
            validate_dimension_agent(staging.path())?;
        }

        let sizing = calculate_image_sizing(staging.path(), config.size_override)?;
        create_ext4_image(&config.output_path, staging.path(), &sizing)?;

        return Ok(BuildResult {
            image_path: config.output_path.clone(),
            project_type: ProjectType::Docker,
            content_size: sizing.content_size,
            image_size: sizing.total_size,
            inode_count: sizing.inode_count,
            inode_usage: sizing.file_count,
            manifest,
        });
    }

    // 1. Determine project type and prepare runtime
    if let Some(ref binary_path) = config.binary_path {
        // Binary mode: skip project detection entirely
        let staging = tempfile::tempdir()?;
        create_directory_structure(staging.path())?;

        // Write DNS configuration so the guest can resolve hostnames at runtime
        write_resolv_conf(staging.path())?;

        let entrypoint_cmd = prepare_binary_runtime(
            staging.path(),
            binary_path,
            config.entrypoint.as_deref(),
        )?;

        // Optionally embed dimension-agent
        if config.embed_dimension_agent {
            embed_agent_with_fallback(staging.path())?;
        }

        // Embed init binary and entrypoint
        embed_init(staging.path(), &entrypoint_cmd)?;
        validate_init(staging.path())?;
        if config.embed_dimension_agent {
            validate_dimension_agent(staging.path())?;
        }

        // Read and parse dimension.toml from the project directory if present
        let manifest = std::fs::read_to_string(config.project_dir.join("dimension.toml"))
            .ok()
            .and_then(|content| parse_manifest(&content).ok())
            .map(|mut m| { m.env.resolve_host_env(); m });

        let sizing = calculate_image_sizing(staging.path(), config.size_override)?;
        create_ext4_image(&config.output_path, staging.path(), &sizing)?;

        Ok(BuildResult {
            image_path: config.output_path.clone(),
            project_type: ProjectType::Binary,
            content_size: sizing.content_size,
            image_size: sizing.total_size,
            inode_count: sizing.inode_count,
            inode_usage: sizing.file_count,
            manifest,
        })
    } else {
        let project_type = detect_project_type(&config.project_dir)?;
        let staging = tempfile::tempdir()?;
        create_directory_structure(staging.path())?;

        // Write DNS configuration so the guest can resolve hostnames at runtime
        write_resolv_conf(staging.path())?;

        let entrypoint_cmd = match &project_type {
            ProjectType::JavaScript => prepare_js_runtime(staging.path(), &config.project_dir)?,
            ProjectType::Rust => prepare_rust_runtime(staging.path(), &config.project_dir)?,
            ProjectType::Binary => unreachable!("Binary type is only used via --binary flag"),
            ProjectType::Docker => unreachable!("Docker type is only used via --docker flag"),
        };

        // Optionally embed dimension-agent
        if config.embed_dimension_agent {
            embed_agent_with_fallback(staging.path())?;
        }

        // Embed init binary and entrypoint
        embed_init(staging.path(), &entrypoint_cmd)?;

        // Validate binaries
        validate_init(staging.path())?;
        if config.embed_dimension_agent {
            validate_dimension_agent(staging.path())?;
        }

        // Read and parse dimension.toml from the project directory if present
        let manifest = std::fs::read_to_string(config.project_dir.join("dimension.toml"))
            .ok()
            .and_then(|content| parse_manifest(&content).ok())
            .map(|mut m| { m.env.resolve_host_env(); m });

        // Calculate image sizing
        let sizing = calculate_image_sizing(staging.path(), config.size_override)?;

        // Create ext4 image
        create_ext4_image(&config.output_path, staging.path(), &sizing)?;

        Ok(BuildResult {
            image_path: config.output_path.clone(),
            project_type,
            content_size: sizing.content_size,
            image_size: sizing.total_size,
            inode_count: sizing.inode_count,
            inode_usage: sizing.file_count,
            manifest,
        })
    }
}

#[cfg(test)]
mod disk_space_tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn check_disk_space_passes_for_small_requirement() {
        // /tmp should always have *some* free space
        let result = check_disk_space(Path::new("/tmp"), 1024);
        assert!(result.is_ok(), "1KB should always be available on /tmp");
    }

    #[test]
    fn check_disk_space_fails_for_impossibly_large_requirement() {
        // Request more space than any disk could have (1 exabyte)
        let result = check_disk_space(Path::new("/tmp"), u64::MAX);
        assert!(result.is_err(), "u64::MAX bytes should always fail");
        match result.unwrap_err() {
            RootfsError::InsufficientDisk { required, available } => {
                assert_eq!(required, u64::MAX);
                assert!(available < u64::MAX);
            }
            other => panic!("Expected InsufficientDisk, got: {:?}", other),
        }
    }

    #[test]
    fn check_disk_space_error_on_nonexistent_dir() {
        let result = check_disk_space(Path::new("/nonexistent_dir_12345"), 1024);
        assert!(result.is_err(), "Nonexistent dir should fail");
        match result.unwrap_err() {
            RootfsError::DiskSpaceCheck(_) => {} // expected
            other => panic!("Expected DiskSpaceCheck, got: {:?}", other),
        }
    }

    #[test]
    fn output_dir_of_maps_bare_filename_to_cwd() {
        // `dimension build` defaults the output to a bare "bundle.ext4", whose
        // parent is Some("") — that must resolve to "." (cwd), not the empty
        // path, or the disk-space check statvfs's "" and fails with ENOENT.
        assert_eq!(output_dir_of(Path::new("bundle.ext4")), Path::new("."));
        // And it must round-trip through check_disk_space without erroring.
        assert!(check_disk_space(output_dir_of(Path::new("bundle.ext4")), 1024).is_ok());
    }

    #[test]
    fn output_dir_of_preserves_explicit_parents() {
        assert_eq!(output_dir_of(Path::new("./bundle.ext4")), Path::new("."));
        assert_eq!(output_dir_of(Path::new("/tmp/bundle.ext4")), Path::new("/tmp"));
        assert_eq!(output_dir_of(Path::new("out/bundle.ext4")), Path::new("out"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rootfs::staging::create_directory_structure;

    #[test]
    fn test_write_resolv_conf_creates_file() {
        let staging = tempfile::tempdir().expect("failed to create temp dir");

        // create_directory_structure creates etc/hyphae, which also creates etc/
        create_directory_structure(staging.path()).expect("create_directory_structure failed");

        write_resolv_conf(staging.path()).expect("write_resolv_conf failed");

        let resolv_conf = staging.path().join("etc/resolv.conf");
        assert!(resolv_conf.exists(), "/etc/resolv.conf was not created");

        let content = std::fs::read_to_string(&resolv_conf).expect("failed to read resolv.conf");
        assert!(
            content.contains("nameserver 8.8.8.8"),
            "missing 8.8.8.8 nameserver"
        );
        assert!(
            content.contains("nameserver 1.1.1.1"),
            "missing 1.1.1.1 nameserver"
        );
    }

    #[test]
    fn test_ceil_to_block_zero() {
        assert_eq!(ceil_to_block(0), 0);
    }

    #[test]
    fn test_ceil_to_block_exact() {
        assert_eq!(ceil_to_block(4096), 4096);
    }

    #[test]
    fn test_ceil_to_block_rounds_up() {
        assert_eq!(ceil_to_block(1), 4096);
        assert_eq!(ceil_to_block(4097), 8192);
    }
}
