//! Rust project binary detection, build, and runtime preparation.
//!
//! Uses `cargo metadata` to discover binary targets and `cargo build --release`
//! to compile them. Handles single binary, multiple binaries with `default-run`,
//! and error cases (no binaries, ambiguous binaries).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use cargo_metadata::MetadataCommand;

use hyphae_errors::RootfsError;

/// Detect the primary binary target in a Rust project.
///
/// Uses `cargo metadata --no-deps` to inspect binary targets:
/// - Single binary target -> returns its name
/// - Multiple binaries with `default-run` set -> returns the default-run name
/// - Multiple binaries without `default-run` -> `Err(MultipleBinaries)`
/// - No binary targets -> `Err(NoBinaryTarget)`
pub fn detect_rust_binary(project_dir: &Path) -> Result<String, RootfsError> {
    let metadata = MetadataCommand::new()
        .manifest_path(project_dir.join("Cargo.toml"))
        .no_deps()
        .exec()
        .map_err(|e| RootfsError::CargoMetadata(e.to_string()))?;

    // Find the root package (the one whose manifest_path parent matches project_dir)
    let canonical_dir = project_dir
        .canonicalize()
        .map_err(|e| RootfsError::CargoMetadata(format!("cannot canonicalize project dir: {e}")))?;

    let package = metadata
        .packages
        .iter()
        .find(|p| {
            p.manifest_path
                .parent()
                .map(|parent| {
                    // cargo_metadata paths may or may not match exactly,
                    // compare canonical forms
                    let parent_path = Path::new(parent.as_str());
                    parent_path.canonicalize().ok() == Some(canonical_dir.clone())
                })
                .unwrap_or(false)
        })
        .ok_or_else(|| RootfsError::NoRootPackage {
            path: project_dir.to_path_buf(),
        })?;

    // Filter for binary targets
    let bin_targets: Vec<&str> = package
        .targets
        .iter()
        .filter(|t| t.is_bin())
        .map(|t| t.name.as_str())
        .collect();

    match bin_targets.len() {
        0 => Err(RootfsError::NoBinaryTarget {
            path: project_dir.to_path_buf(),
        }),
        1 => Ok(bin_targets[0].to_string()),
        _ => {
            // Check for default-run
            if let Some(ref default_run) = package.default_run {
                Ok(default_run.clone())
            } else {
                Err(RootfsError::MultipleBinaries {
                    path: project_dir.to_path_buf(),
                    names: bin_targets.iter().map(|s| s.to_string()).collect(),
                })
            }
        }
    }
}

/// Build a Rust project in release mode and return the path to the compiled binary.
///
/// Runs `cargo build --release --bin <binary_name>` in the project directory.
/// Returns the path to `target/release/<binary_name>` on success.
pub fn build_rust_project(
    project_dir: &Path,
    binary_name: &str,
) -> Result<PathBuf, RootfsError> {
    let target_triple = "x86_64-unknown-linux-musl";

    let output = Command::new("cargo")
        .arg("build")
        .arg("--release")
        .arg("--target")
        .arg(target_triple)
        .arg("--bin")
        .arg(binary_name)
        .current_dir(project_dir)
        .output()
        .map_err(RootfsError::CargoBuild)?;

    if !output.status.success() {
        return Err(RootfsError::CargoBuildFailed {
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        });
    }

    let binary_path = project_dir
        .join("target")
        .join(target_triple)
        .join("release")
        .join(binary_name);

    if !binary_path.exists() {
        return Err(RootfsError::BinaryNotFound {
            expected: binary_path,
        });
    }

    Ok(binary_path)
}

/// Prepare the Rust runtime in a staging directory.
///
/// 1. Detects the binary name via `cargo metadata`
/// 2. Builds the project with `cargo build --release`
/// 3. Copies the compiled binary to `staging_dir/app/{binary_name}` (mode 0o755)
/// 4. Returns the entrypoint command parts `["/app/{binary_name}"]`
///
/// The returned command is suitable for passing to [`embed_init`](super::init::embed_init).
pub fn prepare_rust_runtime(
    staging_dir: &Path,
    project_dir: &Path,
) -> Result<Vec<String>, RootfsError> {
    let binary_name = detect_rust_binary(project_dir)?;
    let binary_path = build_rust_project(project_dir, &binary_name)?;

    let app_dir = staging_dir.join("app");
    fs::create_dir_all(&app_dir)?;

    let dest = app_dir.join(&binary_name);
    fs::copy(&binary_path, &dest)?;
    fs::set_permissions(&dest, fs::Permissions::from_mode(0o755))?;

    Ok(vec![format!("/app/{binary_name}")])
}
