//! Kernel management: download, cache, validate, and register kernels.
//!
//! The [`KernelManager`] provides the public API for listing available kernels
//! from the Firecracker CI S3 bucket, downloading them with content-addressed
//! caching, validating and registering custom user-provided kernels, and
//! looking up kernel records in the SQLite registry.

pub mod download;
pub mod registry;
pub mod types;
pub mod validate;

use crate::kernel::download::{create_http_client, download_kernel, list_available_kernels};
use crate::kernel::registry as kernel_db;
use crate::kernel::types::{AvailableKernel, KernelRecord, KernelSource};
use crate::kernel::validate::validate_kernel;
use hyphae_errors::KernelError;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Progress callback for kernel operations (spinner messages).
pub type KernelProgressFn = Box<dyn Fn(&str) + Send>;

/// Manages kernel download, caching, validation, and registry operations.
///
/// Kernels are stored as content-addressed files at
/// `$HYPHAE_DATA_DIR/kernels/{sha256}.vmlinux`. The manager never auto-deletes
/// cached kernels; users remove versions manually.
pub struct KernelManager {
    client: reqwest::Client,
    storage_dir: PathBuf,
}

impl KernelManager {
    /// Create a new `KernelManager`.
    ///
    /// `data_dir` is the Hyphae data directory (e.g., `~/.local/share/hyphae/`).
    /// Kernels are stored in `data_dir/kernels/`. On creation, any leftover
    /// `.vmlinux.tmp` files from interrupted downloads are cleaned up.
    pub fn new(data_dir: &Path) -> Result<Self, KernelError> {
        let storage_dir = data_dir.join("kernels");
        std::fs::create_dir_all(&storage_dir).map_err(|e| KernelError::IoError {
            path: storage_dir.clone(),
            source: e,
        })?;

        // Clean up partial downloads from previous interrupted sessions.
        if let Ok(entries) = std::fs::read_dir(&storage_dir) {
            for entry in entries.flatten() {
                if entry.path().extension().is_some_and(|e| e == "tmp") {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }

        let client = create_http_client()?;

        Ok(Self {
            client,
            storage_dir,
        })
    }

    /// List available kernels from the Firecracker CI S3 bucket.
    ///
    /// `fc_version` is the Firecracker version prefix (e.g., `"v1.14"`).
    /// Automatically detects the host architecture via [`std::env::consts::ARCH`].
    pub async fn list_available(
        &self,
        fc_version: &str,
    ) -> Result<Vec<AvailableKernel>, KernelError> {
        let arch = std::env::consts::ARCH;
        list_available_kernels(&self.client, fc_version, arch).await
    }

    /// Download and register a managed kernel.
    ///
    /// If `kernel_version` is `None`, downloads the latest available version.
    /// Returns the existing record if a kernel with the same content hash is
    /// already cached (content-hash deduplication).
    ///
    /// `conn` is a reference to the Registry's database connection, keeping
    /// the kernel manager decoupled from the `Registry` struct.
    pub async fn download(
        &self,
        conn: &rusqlite::Connection,
        fc_version: &str,
        kernel_version: Option<&str>,
        progress: Option<&KernelProgressFn>,
    ) -> Result<KernelRecord, KernelError> {
        let arch = std::env::consts::ARCH;
        let report = |msg: &str| {
            if let Some(f) = progress {
                f(msg);
            }
        };

        // Step 1: List available kernels.
        report("Querying available kernels...");
        let available = list_available_kernels(&self.client, fc_version, arch).await?;

        if available.is_empty() {
            return Err(KernelError::NoKernelsAvailable {
                fc_version: fc_version.to_string(),
                arch: arch.to_string(),
            });
        }

        // Step 2: Select kernel version.
        let selected = match kernel_version {
            Some(v) => available
                .iter()
                .find(|k| k.version == v)
                .ok_or_else(|| KernelError::VersionNotFound {
                    version: v.to_string(),
                    available: available.iter().map(|k| k.version.clone()).collect(),
                })?,
            None => available.last().unwrap(), // Latest (list is sorted ascending).
        };

        report(&format!("Selected kernel: vmlinux-{}", selected.version));

        // Step 3: Download to temp file.
        let tmp_path = self
            .storage_dir
            .join(format!("{}.vmlinux.tmp", selected.version));
        report("Downloading...");
        let result = download_kernel(&self.client, selected, &tmp_path).await?;

        // Step 4: Check for duplicate by content hash.
        if let Some(existing) = kernel_db::find_by_hash(conn, &result.content_hash)? {
            let _ = std::fs::remove_file(&tmp_path);
            report("Kernel already cached (content match)");
            return Ok(existing);
        }

        // Step 5: Rename to final path (atomic on same filesystem).
        let final_path = self
            .storage_dir
            .join(format!("{}.vmlinux", &result.content_hash));
        std::fs::rename(&tmp_path, &final_path).map_err(|e| KernelError::IoError {
            path: final_path.clone(),
            source: e,
        })?;

        // Step 6: Register in database.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        let record = kernel_db::register_kernel(
            conn,
            &KernelRecord {
                id: 0, // Auto-assigned by database.
                version: selected.version.clone(),
                content_hash: result.content_hash,
                source: KernelSource::Managed,
                arch: arch.to_string(),
                disk_path: final_path,
                size_bytes: result.size_bytes,
                fc_version: Some(fc_version.to_string()),
                created_at: now,
            },
        )?;

        report(&format!("Kernel registered: id={}", record.id));
        Ok(record)
    }

    /// Register a custom kernel from a user-provided path.
    ///
    /// Validates the ELF header (must be 64-bit, x86_64 or aarch64), copies
    /// the file to managed storage, and registers it in the database. Returns
    /// the existing record if a kernel with the same content hash is already
    /// cached.
    pub fn register_custom(
        &self,
        conn: &rusqlite::Connection,
        source_path: &Path,
        version_label: &str,
    ) -> Result<KernelRecord, KernelError> {
        // Step 1: Validate ELF.
        let validation = validate_kernel(source_path);
        if !validation.is_valid {
            return Err(KernelError::InvalidKernel {
                path: source_path.to_path_buf(),
                reason: validation.error.unwrap_or_default(),
            });
        }

        let arch = validation.machine.unwrap_or_default();

        // Step 2: Compute content hash.
        let content_hash = hash_kernel_file(source_path)?;

        // Step 3: Check for duplicate.
        if let Some(existing) = kernel_db::find_by_hash(conn, &content_hash)? {
            return Ok(existing);
        }

        // Step 4: Copy to managed storage.
        let dest = self
            .storage_dir
            .join(format!("{}.vmlinux", &content_hash));
        std::fs::copy(source_path, &dest).map_err(|e| KernelError::IoError {
            path: dest.clone(),
            source: e,
        })?;

        let size = std::fs::metadata(&dest)
            .map_err(|e| KernelError::IoError {
                path: dest.clone(),
                source: e,
            })?
            .len();

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        // Step 5: Register in database.
        let record = kernel_db::register_kernel(
            conn,
            &KernelRecord {
                id: 0,
                version: version_label.to_string(),
                content_hash,
                source: KernelSource::Custom,
                arch,
                disk_path: dest,
                size_bytes: size,
                fc_version: None,
                created_at: now,
            },
        )?;

        Ok(record)
    }

    /// Find a kernel by its database ID. Convenience wrapper.
    pub fn find_by_id(
        &self,
        conn: &rusqlite::Connection,
        id: i64,
    ) -> Result<Option<KernelRecord>, KernelError> {
        kernel_db::find_by_id(conn, id)
    }

    /// Find a kernel by its content hash. Convenience wrapper.
    pub fn find_by_hash(
        &self,
        conn: &rusqlite::Connection,
        content_hash: &str,
    ) -> Result<Option<KernelRecord>, KernelError> {
        kernel_db::find_by_hash(conn, content_hash)
    }

    /// List all registered kernels. Convenience wrapper.
    pub fn list_registered(
        &self,
        conn: &rusqlite::Connection,
        source_filter: Option<KernelSource>,
    ) -> Result<Vec<KernelRecord>, KernelError> {
        kernel_db::list_kernels(conn, source_filter)
    }

    /// Get the storage directory path for kernel files.
    pub fn storage_dir(&self) -> &Path {
        &self.storage_dir
    }
}

/// Compute SHA-256 hash of a kernel file on disk.
fn hash_kernel_file(path: &Path) -> Result<String, KernelError> {
    use std::io::Read;
    let mut file =
        std::fs::File::open(path).map_err(|e| KernelError::IoError {
            path: path.to_path_buf(),
            source: e,
        })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];
    loop {
        let n = file.read(&mut buffer).map_err(|e| KernelError::IoError {
            path: path.to_path_buf(),
            source: e,
        })?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}
