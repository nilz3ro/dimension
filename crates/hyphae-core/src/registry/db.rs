//! Database initialisation helpers: pragmas, migrations, and integrity checks.

use hyphae_errors::RegistryError;
use rusqlite::Connection;
use rusqlite_migration::{Migrations, M};

/// Configure recommended SQLite pragmas for WAL-mode operation.
///
/// - `journal_mode = wal` -- write-ahead logging for concurrent reads.
/// - `synchronous = normal` -- safe with WAL; avoids fsync on every commit.
/// - `foreign_keys = on` -- enforce FK constraints (off by default in SQLite).
/// - `busy_timeout = 5000` -- wait up to 5 s before returning SQLITE_BUSY.
pub fn init_pragmas(conn: &Connection) -> Result<(), RegistryError> {
    let journal_mode: String = conn
        .pragma_update_and_check(None, "journal_mode", "wal", |row| row.get(0))
        .map_err(|e| RegistryError::Database(e.to_string()))?;

    if !journal_mode.eq_ignore_ascii_case("wal") {
        return Err(RegistryError::WalModeUnavailable {
            actual: journal_mode,
        });
    }

    conn.pragma_update(None, "synchronous", "normal")
        .map_err(|e| RegistryError::Database(e.to_string()))?;

    conn.pragma_update(None, "foreign_keys", "on")
        .map_err(|e| RegistryError::Database(e.to_string()))?;

    conn.pragma_update(None, "busy_timeout", 5000)
        .map_err(|e| RegistryError::Database(e.to_string()))?;

    Ok(())
}

/// Run schema migrations to bring the database up to the latest version.
///
/// Currently defines a single migration creating the `images` and
/// `running_vms` tables with all required indexes.
pub fn run_migrations(conn: &mut Connection) -> Result<(), RegistryError> {
    let migrations = Migrations::new(vec![
        // Migration 1: Create initial schema.
        M::up(
            "CREATE TABLE images (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                content_hash TEXT NOT NULL UNIQUE,
                name         TEXT NOT NULL,
                tag          TEXT NOT NULL DEFAULT 'latest',
                size_bytes   INTEGER NOT NULL,
                source_path  TEXT NOT NULL,
                init_config  TEXT,
                disk_path    TEXT NOT NULL,
                created_at   INTEGER NOT NULL
            );

            CREATE UNIQUE INDEX idx_images_name_tag
                ON images(name, tag);
            CREATE INDEX idx_images_content_hash
                ON images(content_hash);

            CREATE TABLE running_vms (
                vm_id      TEXT PRIMARY KEY,
                image_id   INTEGER NOT NULL,
                started_at INTEGER NOT NULL,
                FOREIGN KEY (image_id) REFERENCES images(id)
            );

            CREATE INDEX idx_running_vms_image_id
                ON running_vms(image_id);",
        ),
        // Migration 2: Add bundle VM defaults to images table.
        M::up(
            "ALTER TABLE images ADD COLUMN default_vcpus INTEGER NOT NULL DEFAULT 2;
             ALTER TABLE images ADD COLUMN default_memory_mib INTEGER NOT NULL DEFAULT 256;",
        ),
        // Migration 3: Create kernels table for kernel management.
        M::up(
            "CREATE TABLE kernels (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                version      TEXT NOT NULL,
                content_hash TEXT NOT NULL UNIQUE,
                source       TEXT NOT NULL CHECK(source IN ('managed', 'custom')),
                arch         TEXT NOT NULL CHECK(arch IN ('x86_64', 'aarch64')),
                disk_path    TEXT NOT NULL,
                size_bytes   INTEGER NOT NULL,
                fc_version   TEXT,
                created_at   INTEGER NOT NULL
            );

            CREATE INDEX idx_kernels_version ON kernels(version);
            CREATE INDEX idx_kernels_source ON kernels(source);",
        ),
        // Migration 4: Add owner_id to images for per-user bundle ownership.
        // NULL = platform bundle (visible to all users).
        // Non-NULL = user-owned bundle (private to that user).
        M::up(
            "ALTER TABLE images ADD COLUMN owner_id TEXT;
             CREATE INDEX idx_images_owner_id ON images(owner_id);",
        ),
        // Migration 5: Add manifest fields to images table.
        // Each section stored as JSON TEXT. NULL = no manifest (safe defaults apply).
        M::up(
            "ALTER TABLE images ADD COLUMN manifest_resources TEXT;
             ALTER TABLE images ADD COLUMN manifest_env TEXT;
             ALTER TABLE images ADD COLUMN manifest_secrets TEXT;
             ALTER TABLE images ADD COLUMN manifest_capabilities TEXT;
             ALTER TABLE images ADD COLUMN manifest_a2a TEXT;
             ALTER TABLE images ADD COLUMN manifest_timeout_secs INTEGER;",
        ),
        // Migration 6: Add manifest_volumes column for volume-enabled bundles.
        // Stores JSON-serialized VolumesSection from dimension.toml.
        // NULL = no [volumes] section in dimension.toml (volume-disabled bundle).
        M::up("ALTER TABLE images ADD COLUMN manifest_volumes TEXT;"),
    ]);

    migrations
        .to_latest(conn)
        .map_err(|e| RegistryError::MigrationFailed(e.to_string()))?;

    Ok(())
}

/// Run `PRAGMA quick_check` to detect database corruption.
///
/// Returns `Ok(())` when the database passes the check, or
/// `RegistryError::IntegrityCheckFailed` with the diagnostic string
/// returned by SQLite.
pub fn check_integrity(conn: &Connection) -> Result<(), RegistryError> {
    let result: String = conn
        .pragma_query_value(None, "quick_check", |row| row.get(0))
        .map_err(|e| RegistryError::Database(e.to_string()))?;

    if result.eq_ignore_ascii_case("ok") {
        Ok(())
    } else {
        Err(RegistryError::IntegrityCheckFailed { details: result })
    }
}
