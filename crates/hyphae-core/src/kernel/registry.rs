//! Database CRUD operations for kernel records.
//!
//! Functions take a `&Connection` directly rather than coupling to the
//! [`Registry`] struct, allowing the orchestrator to pass the connection
//! from whatever registry it already holds.

use crate::kernel::types::{KernelRecord, KernelSource};
use hyphae_errors::KernelError;
use rusqlite::{Connection, OptionalExtension};
use std::path::PathBuf;

/// Insert a new kernel record into the database.
///
/// The `id` field on the input record is ignored; the database assigns
/// an auto-incremented id which is returned in the result.
pub fn register_kernel(
    conn: &Connection,
    record: &KernelRecord,
) -> Result<KernelRecord, KernelError> {
    conn.execute(
        "INSERT INTO kernels (version, content_hash, source, arch, disk_path, size_bytes, fc_version, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            record.version,
            record.content_hash,
            record.source.to_string(),
            record.arch,
            record.disk_path.to_string_lossy().as_ref(),
            record.size_bytes as i64,
            record.fc_version,
            record.created_at,
        ],
    )
    .map_err(|e| KernelError::RegistryError(e.to_string()))?;

    let id = conn.last_insert_rowid();
    Ok(KernelRecord {
        id,
        ..record.clone()
    })
}

/// Find a kernel by its database ID.
pub fn find_by_id(
    conn: &Connection,
    id: i64,
) -> Result<Option<KernelRecord>, KernelError> {
    conn.prepare(
        "SELECT id, version, content_hash, source, arch, disk_path, size_bytes, fc_version, created_at
         FROM kernels WHERE id = ?1",
    )
    .map_err(|e| KernelError::RegistryError(e.to_string()))?
    .query_row(rusqlite::params![id], row_to_kernel_record)
    .optional()
    .map_err(|e| KernelError::RegistryError(e.to_string()))
}

/// Find a kernel by its content hash (SHA-256).
pub fn find_by_hash(
    conn: &Connection,
    content_hash: &str,
) -> Result<Option<KernelRecord>, KernelError> {
    conn.prepare(
        "SELECT id, version, content_hash, source, arch, disk_path, size_bytes, fc_version, created_at
         FROM kernels WHERE content_hash = ?1",
    )
    .map_err(|e| KernelError::RegistryError(e.to_string()))?
    .query_row(rusqlite::params![content_hash], row_to_kernel_record)
    .optional()
    .map_err(|e| KernelError::RegistryError(e.to_string()))
}

/// List all kernel records, optionally filtered by source.
///
/// Results are sorted by `created_at DESC` (newest first).
pub fn list_kernels(
    conn: &Connection,
    source_filter: Option<KernelSource>,
) -> Result<Vec<KernelRecord>, KernelError> {
    let (sql, params): (&str, Vec<Box<dyn rusqlite::types::ToSql>>) = match source_filter {
        Some(source) => (
            "SELECT id, version, content_hash, source, arch, disk_path, size_bytes, fc_version, created_at
             FROM kernels WHERE source = ?1 ORDER BY created_at DESC",
            vec![Box::new(source.to_string())],
        ),
        None => (
            "SELECT id, version, content_hash, source, arch, disk_path, size_bytes, fc_version, created_at
             FROM kernels ORDER BY created_at DESC",
            vec![],
        ),
    };

    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| KernelError::RegistryError(e.to_string()))?;

    let params_refs: Vec<&dyn rusqlite::types::ToSql> =
        params.iter().map(|p| p.as_ref()).collect();
    let rows = stmt
        .query_map(params_refs.as_slice(), row_to_kernel_record)
        .map_err(|e| KernelError::RegistryError(e.to_string()))?;

    let mut kernels = Vec::new();
    for row in rows {
        kernels.push(row.map_err(|e| KernelError::RegistryError(e.to_string()))?);
    }
    Ok(kernels)
}

/// Convert a database row to a [`KernelRecord`].
fn row_to_kernel_record(row: &rusqlite::Row) -> rusqlite::Result<KernelRecord> {
    let source_str: String = row.get(3)?;
    let disk_path_str: String = row.get(5)?;
    let size_bytes: i64 = row.get(6)?;

    Ok(KernelRecord {
        id: row.get(0)?,
        version: row.get(1)?,
        content_hash: row.get(2)?,
        source: KernelSource::from_str_value(&source_str).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                3,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
            )
        })?,
        arch: row.get(4)?,
        disk_path: PathBuf::from(disk_path_str),
        size_bytes: size_bytes as u64,
        fc_version: row.get(7)?,
        created_at: row.get(8)?,
    })
}
