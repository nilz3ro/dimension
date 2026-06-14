//! Pre-built binary packaging for rootfs images.
//!
//! Copies a pre-built static binary (or directory of files) into a rootfs
//! staging area, bypassing project type detection and language-specific builds.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use walkdir::WalkDir;

use hyphae_errors::RootfsError;

/// Prepare a pre-built binary (or directory) in a rootfs staging area.
///
/// - If `binary_path` is a file: copies it to `staging_dir/app/{filename}` with mode 0o755.
///   Default entrypoint is `/app/{filename}`.
/// - If `binary_path` is a directory: copies all contents into `staging_dir/app/`.
///   An explicit entrypoint must be provided (errors if `None`).
///
/// Returns the entrypoint command parts suitable for [`embed_init`](super::init::embed_init).
pub fn prepare_binary_runtime(
    staging_dir: &Path,
    binary_path: &Path,
    entrypoint: Option<&str>,
) -> Result<Vec<String>, RootfsError> {
    let app_dir = staging_dir.join("app");
    fs::create_dir_all(&app_dir)?;

    if binary_path.is_file() {
        let filename = binary_path
            .file_name()
            .ok_or_else(|| {
                RootfsError::BinaryNotFound {
                    expected: binary_path.to_path_buf(),
                }
            })?
            .to_string_lossy()
            .to_string();

        let dest = app_dir.join(&filename);
        fs::copy(binary_path, &dest)?;
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o755))?;

        let cmd = entrypoint
            .map(|e| e.to_string())
            .unwrap_or_else(|| format!("/app/{filename}"));

        Ok(vec![cmd])
    } else if binary_path.is_dir() {
        let entrypoint_str = entrypoint.ok_or_else(|| {
            RootfsError::NoEntrypoint {
                project_dir: binary_path.to_path_buf(),
            }
        })?;

        // Walk and copy directory contents into staging_dir/app/
        for entry in WalkDir::new(binary_path).min_depth(1) {
            let entry = entry.map_err(|e| RootfsError::StagingWalk(e.to_string()))?;
            let rel = entry
                .path()
                .strip_prefix(binary_path)
                .map_err(|e| RootfsError::StagingWalk(e.to_string()))?;
            let dest = app_dir.join(rel);

            if entry.file_type().is_dir() {
                fs::create_dir_all(&dest)?;
            } else if entry.file_type().is_file() {
                if let Some(parent) = dest.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::copy(entry.path(), &dest)?;
                // Preserve executable bit for files that already have it
                let src_mode = entry
                    .metadata()
                    .map_err(|e| RootfsError::StagingWalk(e.to_string()))?
                    .permissions()
                    .mode();
                if src_mode & 0o111 != 0 {
                    fs::set_permissions(&dest, fs::Permissions::from_mode(0o755))?;
                }
            }
        }

        Ok(vec![entrypoint_str.to_string()])
    } else {
        Err(RootfsError::BinaryNotFound {
            expected: binary_path.to_path_buf(),
        })
    }
}
