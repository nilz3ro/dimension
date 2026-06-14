//! Data directory resolution and image file path construction.

use std::path::{Path, PathBuf};

use hyphae_errors::RegistryError;

/// Resolve the data directory for the registry.
///
/// Resolution order:
/// 1. `HYPHAE_DATA_DIR` environment variable (if set and non-empty).
/// 2. Platform data directory via [`dirs::data_dir`] joined with `"hyphae"`.
/// 3. [`RegistryError::NoDataDirectory`] if neither is available.
pub fn default_data_dir() -> Result<PathBuf, RegistryError> {
    if let Ok(dir) = std::env::var("HYPHAE_DATA_DIR") {
        if !dir.is_empty() {
            return Ok(PathBuf::from(dir));
        }
    }

    dirs::data_dir()
        .map(|d| d.join("hyphae"))
        .ok_or(RegistryError::NoDataDirectory)
}

/// Construct the on-disk path for a content-addressed image file.
///
/// The naming convention `sha256-{hash}.ext4` makes files self-describing
/// and allows recovery scans to reconstruct the content hash from filenames.
pub fn image_file_path(storage_dir: &Path, content_hash: &str) -> PathBuf {
    storage_dir.join(format!("sha256-{content_hash}.ext4"))
}
