//! Image registry with SQLite-backed metadata and content-addressed storage.
//!
//! The [`Registry`] struct manages a SQLite database for image metadata and a
//! directory of content-addressed ext4 images. On open it configures WAL mode,
//! runs schema migrations, and performs an integrity check (with automatic
//! recovery from detected corruption).

pub mod db;
pub mod hash;
pub mod image;
pub mod storage;

pub mod vm_tracking;

pub use hash::{hash_source_directory, SKIP_DIRS};
pub use image::{derive_image_name, parse_image_ref, ImageRecord, NewImage};
pub use storage::{default_data_dir, image_file_path};
pub use vm_tracking::VmRecord;

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use hyphae_errors::RegistryError;
use rusqlite::Connection;

/// A handle to the image registry.
///
/// Owns a SQLite connection and knows the on-disk storage directory where
/// ext4 images are kept. Created via [`Registry::open`].
pub struct Registry {
    conn: Connection,
    storage_dir: PathBuf,
}

impl Registry {
    /// Open (or create) a registry rooted at `data_dir`.
    ///
    /// Creates `data_dir` and `data_dir/images` if they do not exist, opens
    /// (or creates) `data_dir/registry.db`, configures WAL-mode pragmas, runs
    /// schema migrations, and performs a quick integrity check.
    ///
    /// If the integrity check fails the corrupted database is renamed to
    /// `registry.db.corrupt-{unix_timestamp}` and a fresh database is
    /// initialised in its place.
    pub fn open(data_dir: &Path) -> Result<Self, RegistryError> {
        // Ensure data directory exists.
        std::fs::create_dir_all(data_dir).map_err(|e| RegistryError::StorageAccess {
            path: data_dir.to_path_buf(),
            message: e.to_string(),
        })?;

        let db_path = data_dir.join("registry.db");

        // Ensure images storage directory exists.
        let storage_dir = data_dir.join("images");
        std::fs::create_dir_all(&storage_dir).map_err(|e| RegistryError::StorageAccess {
            path: storage_dir.clone(),
            message: e.to_string(),
        })?;

        // Open SQLite connection.
        let mut conn =
            Connection::open(&db_path).map_err(|e| RegistryError::DatabaseOpen {
                path: db_path.clone(),
                message: e.to_string(),
            })?;

        db::init_pragmas(&conn)?;
        db::run_migrations(&mut conn)?;

        // Integrity check with corruption recovery.
        match db::check_integrity(&conn) {
            Ok(()) => {}
            Err(RegistryError::IntegrityCheckFailed { details }) => {
                // Drop the connection before renaming.
                drop(conn);

                let timestamp = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let corrupt_path =
                    data_dir.join(format!("registry.db.corrupt-{timestamp}"));
                std::fs::rename(&db_path, &corrupt_path).map_err(|e| {
                    RegistryError::StorageAccess {
                        path: db_path.clone(),
                        message: e.to_string(),
                    }
                })?;

                eprintln!(
                    "warning: registry database corruption detected ({details}). \
                     Corrupted file saved as {}. \
                     Reinitialising with empty database. \
                     Existing image files remain on disk and can be re-registered.",
                    corrupt_path.display(),
                );

                // Re-open fresh database.
                conn = Connection::open(&db_path).map_err(|e| {
                    RegistryError::DatabaseOpen {
                        path: db_path.clone(),
                        message: e.to_string(),
                    }
                })?;
                db::init_pragmas(&conn)?;
                db::run_migrations(&mut conn)?;
            }
            Err(other) => return Err(other),
        }

        Ok(Registry { conn, storage_dir })
    }

    /// Borrow the underlying database connection.
    ///
    /// Crate-internal: used by submodules (image, vm_tracking) that need
    /// direct database access through the owning `Registry`.
    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Path to the images storage directory.
    pub fn storage_dir(&self) -> &Path {
        &self.storage_dir
    }
}
