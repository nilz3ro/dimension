//! Volume file creation utility for persistent agent workspaces.
//!
//! Provides helpers to create sparse ext4 volume images on disk and to
//! compute the canonical path for a volume given its UUID.

use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Default directory for volume image files.
///
/// MUST be on the same filesystem as `DEFAULT_CHROOT_BASE` (/srv/hyphae/jails)
/// to avoid EXDEV hard-link failures when the jailer hard-links the volume image
/// into the VM's chroot directory.
pub const DEFAULT_VOLUME_DIR: &str = "/srv/hyphae/volumes";

/// Errors that can occur during volume file creation.
#[derive(Debug, thiserror::Error)]
pub enum VolumeError {
    /// An I/O error occurred (file creation, truncation, etc.)
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    /// The `mke2fs` command failed to format the volume image.
    #[error("ext4 format failed: {0}")]
    FormatFailed(String),
}

/// Return the canonical path for a volume image file.
///
/// Volumes are stored as `{DEFAULT_VOLUME_DIR}/{volume_id}.img`.
///
/// # Example
///
/// ```
/// use uuid::Uuid;
/// use hyphae_core::volume::volume_path;
///
/// let id = Uuid::nil();
/// let path = volume_path(&id);
/// assert!(path.to_string_lossy().ends_with(".img"));
/// ```
pub fn volume_path(volume_id: &Uuid) -> PathBuf {
    PathBuf::from(DEFAULT_VOLUME_DIR).join(format!("{}.img", volume_id))
}

/// Return the canonical path for a named volume image file.
///
/// Named volumes are stored as `{DEFAULT_VOLUME_DIR}/named/{volume_id}.img`.
/// This separates them from session-scoped volumes to avoid ID collisions.
pub fn named_volume_path(volume_id: &Uuid) -> PathBuf {
    PathBuf::from(DEFAULT_VOLUME_DIR)
        .join("named")
        .join(format!("{}.img", volume_id))
}

/// Create a sparse ext4 volume image at the given path.
///
/// Steps:
/// 1. Ensures the parent directory exists (creates it if absent).
/// 2. Creates a sparse file of `size_bytes` using `tokio::fs::File::set_len`.
/// 3. Formats the file as ext4 using `mke2fs -t ext4 -F {path}`.
///
/// The file is sparse: it does not consume `size_bytes` of actual disk space
/// until data is written inside the ext4 filesystem.
///
/// # Errors
///
/// - [`VolumeError::Io`] if directory creation or file creation/truncation fails.
/// - [`VolumeError::FormatFailed`] if `mke2fs` exits with a non-zero status.
pub async fn create_ext4_volume(path: &Path, size_bytes: i64) -> Result<(), VolumeError> {
    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    // Create sparse file via truncate (preserves sparseness; fallocate would pre-allocate)
    let file = tokio::fs::File::create(path).await?;
    file.set_len(size_bytes as u64).await?;
    drop(file);

    // Format with ext4 using mke2fs
    // -t ext4  : use ext4 filesystem type
    // -F       : force formatting of a regular file (not a block device)
    let output = tokio::process::Command::new("mke2fs")
        .args(["-t", "ext4", "-F", &path.to_string_lossy()])
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(VolumeError::FormatFailed(stderr.into_owned()));
    }

    Ok(())
}
