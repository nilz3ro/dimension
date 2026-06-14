//! Image record types, name:tag parsing, and CRUD operations on [`Registry`].
//!
//! This module defines the data structures that represent images in the
//! registry ([`ImageRecord`], [`NewImage`]) and adds query/mutation methods
//! to [`Registry`] via an `impl` block.

use std::path::Path;

use hyphae_errors::RegistryError;
use rusqlite::params;

use super::Registry;

/// A fully materialised image record read from the database.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ImageRecord {
    pub id: i64,
    pub content_hash: String,
    pub name: String,
    pub tag: String,
    pub size_bytes: u64,
    pub source_path: String,
    pub init_config: Option<String>,
    pub disk_path: String,
    pub created_at: i64,
    pub default_vcpus: i64,
    pub default_memory_mib: i64,
    /// NULL = platform bundle (visible to all users); UUID string = user-owned (private).
    pub owner_id: Option<String>,
    /// JSON-serialized ResourcesSection from dimension.toml. NULL = no manifest.
    pub manifest_resources: Option<String>,
    /// JSON-serialized EnvSection.vars HashMap from dimension.toml. NULL = no manifest.
    pub manifest_env: Option<String>,
    /// JSON-serialized SecretsSection from dimension.toml. NULL = no manifest.
    pub manifest_secrets: Option<String>,
    /// JSON-serialized CapabilitiesSection from dimension.toml. NULL = no manifest.
    pub manifest_capabilities: Option<String>,
    /// JSON-serialized A2aSection from dimension.toml. NULL = no manifest.
    pub manifest_a2a: Option<String>,
    /// Execution timeout in seconds from dimension.toml [resources].timeout_secs.
    /// Stored as a separate INTEGER column for efficient lookup at VM launch time.
    pub manifest_timeout_secs: Option<i64>,
    /// JSON-serialized VolumesSection from dimension.toml. NULL = no [volumes] section.
    pub manifest_volumes: Option<String>,
}

/// Input for registering a new image (no `id` -- assigned by the database).
#[derive(Debug, Clone)]
pub struct NewImage {
    pub content_hash: String,
    pub name: String,
    pub tag: String,
    pub size_bytes: u64,
    pub source_path: String,
    pub init_config: Option<String>,
    pub disk_path: String,
    pub created_at: i64,
    pub default_vcpus: i64,
    pub default_memory_mib: i64,
    /// NULL = platform bundle (visible to all users); UUID string = user-owned (private).
    pub owner_id: Option<String>,
    /// JSON-serialized ResourcesSection. None = no manifest (stores NULL).
    pub manifest_resources: Option<String>,
    /// JSON-serialized EnvSection.vars HashMap. None = no manifest (stores NULL).
    pub manifest_env: Option<String>,
    /// JSON-serialized SecretsSection. None = no manifest (stores NULL).
    pub manifest_secrets: Option<String>,
    /// JSON-serialized CapabilitiesSection. None = no manifest (stores NULL).
    pub manifest_capabilities: Option<String>,
    /// JSON-serialized A2aSection. None = no manifest (stores NULL).
    pub manifest_a2a: Option<String>,
    /// Timeout in seconds from [resources].timeout_secs. None = use server default.
    pub manifest_timeout_secs: Option<i64>,
    /// JSON-serialized VolumesSection. None = no [volumes] section (stores NULL).
    pub manifest_volumes: Option<String>,
}

/// Parse an image reference into `(name, tag)`.
///
/// Splits on the **last** colon so that path-like names with colons are
/// handled correctly. When no colon is found, or the tag portion after the
/// colon is empty, the original unsplit reference is returned with a default
/// tag of `"latest"`.
///
/// # Examples
///
/// ```
/// # use hyphae_core::registry::parse_image_ref;
/// assert_eq!(parse_image_ref("my-app:v1.0"), ("my-app".into(), "v1.0".into()));
/// assert_eq!(parse_image_ref("my-app"), ("my-app".into(), "latest".into()));
/// assert_eq!(parse_image_ref("my-app:"), ("my-app:".into(), "latest".into()));
/// ```
pub fn parse_image_ref(reference: &str) -> (String, String) {
    if let Some((name, tag)) = reference.rsplit_once(':') {
        if !name.is_empty() && !tag.is_empty() {
            return (name.to_string(), tag.to_string());
        }
    }
    (reference.to_string(), "latest".to_string())
}

/// Extract the image name from the last component of a source path.
///
/// # Errors
///
/// Returns [`RegistryError::InvalidSourcePath`] if the path has no file name
/// component or it cannot be converted to UTF-8.
pub fn derive_image_name(source_path: &Path) -> Result<String, RegistryError> {
    source_path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .ok_or_else(|| RegistryError::InvalidSourcePath {
            path: source_path.to_path_buf(),
        })
}

/// Map a database row to an [`ImageRecord`].
///
/// Column order must match `SELECT *` on the `images` table.
fn row_to_image_record(row: &rusqlite::Row) -> rusqlite::Result<ImageRecord> {
    let size_i64: i64 = row.get("size_bytes")?;
    Ok(ImageRecord {
        id: row.get("id")?,
        content_hash: row.get("content_hash")?,
        name: row.get("name")?,
        tag: row.get("tag")?,
        size_bytes: size_i64 as u64,
        source_path: row.get("source_path")?,
        init_config: row.get("init_config")?,
        disk_path: row.get("disk_path")?,
        created_at: row.get("created_at")?,
        default_vcpus: row.get("default_vcpus")?,
        default_memory_mib: row.get("default_memory_mib")?,
        owner_id: row.get("owner_id")?,
        manifest_resources: row.get("manifest_resources")?,
        manifest_env: row.get("manifest_env")?,
        manifest_secrets: row.get("manifest_secrets")?,
        manifest_capabilities: row.get("manifest_capabilities")?,
        manifest_a2a: row.get("manifest_a2a")?,
        manifest_timeout_secs: row.get("manifest_timeout_secs")?,
        manifest_volumes: row.get("manifest_volumes")?,
    })
}

// ---------------------------------------------------------------------------
// CRUD methods on Registry (impl block)
// ---------------------------------------------------------------------------

impl Registry {
    /// Register a new image in the registry.
    ///
    /// **Cache hit:** if an image with the same `content_hash` already exists
    /// the existing record is returned immediately (no insert).
    ///
    /// **Re-tag:** if `name:tag` already exists but with a different content
    /// hash, the old record is removed (and its disk file cleaned up if it
    /// differs from the new one) before inserting the replacement.
    pub fn register_image(&self, record: &NewImage) -> Result<ImageRecord, RegistryError> {
        // 1. Cache hit -- identical content already registered.
        if let Some(existing) = self.find_by_hash(&record.content_hash)? {
            return Ok(existing);
        }

        // 2. Re-tag -- same name:tag, different content.
        if let Some(old) = self.find_by_name_tag(&record.name, &record.tag)? {
            let old_disk = old.disk_path.clone();
            self.delete_image_record(old.id)?;

            // Best-effort cleanup of the old disk file if it differs.
            if old_disk != record.disk_path {
                let path = Path::new(&old_disk);
                if path.exists() {
                    if let Err(e) = std::fs::remove_file(path) {
                        eprintln!(
                            "warning: failed to clean up old image file {}: {}",
                            old_disk, e
                        );
                    }
                }
            }
        }

        // 3. Insert new record.
        self.conn()
            .execute(
                "INSERT INTO images (content_hash, name, tag, size_bytes, source_path, \
                 init_config, disk_path, created_at, default_vcpus, default_memory_mib, owner_id, \
                 manifest_resources, manifest_env, manifest_secrets, manifest_capabilities, \
                 manifest_a2a, manifest_timeout_secs, manifest_volumes) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
                params![
                    record.content_hash,
                    record.name,
                    record.tag,
                    record.size_bytes as i64,
                    record.source_path,
                    record.init_config,
                    record.disk_path,
                    record.created_at,
                    record.default_vcpus,
                    record.default_memory_mib,
                    record.owner_id,
                    record.manifest_resources,
                    record.manifest_env,
                    record.manifest_secrets,
                    record.manifest_capabilities,
                    record.manifest_a2a,
                    record.manifest_timeout_secs,
                    record.manifest_volumes,
                ],
            )
            .map_err(|e| RegistryError::Database(e.to_string()))?;

        let id = self.conn().last_insert_rowid();
        self.find_by_id(id)?
            .ok_or(RegistryError::InsertFailed)
    }

    /// Find an image by its content hash.
    pub fn find_by_hash(&self, hash: &str) -> Result<Option<ImageRecord>, RegistryError> {
        let mut stmt = self
            .conn()
            .prepare("SELECT * FROM images WHERE content_hash = ?1")
            .map_err(|e| RegistryError::Database(e.to_string()))?;

        let mut rows = stmt
            .query_map(params![hash], row_to_image_record)
            .map_err(|e| RegistryError::Database(e.to_string()))?;

        match rows.next() {
            Some(Ok(record)) => Ok(Some(record)),
            Some(Err(e)) => Err(RegistryError::Database(e.to_string())),
            None => Ok(None),
        }
    }

    /// Find an image by name and tag.
    pub fn find_by_name_tag(
        &self,
        name: &str,
        tag: &str,
    ) -> Result<Option<ImageRecord>, RegistryError> {
        let mut stmt = self
            .conn()
            .prepare("SELECT * FROM images WHERE name = ?1 AND tag = ?2")
            .map_err(|e| RegistryError::Database(e.to_string()))?;

        let mut rows = stmt
            .query_map(params![name, tag], row_to_image_record)
            .map_err(|e| RegistryError::Database(e.to_string()))?;

        match rows.next() {
            Some(Ok(record)) => Ok(Some(record)),
            Some(Err(e)) => Err(RegistryError::Database(e.to_string())),
            None => Ok(None),
        }
    }

    /// Find an image by its primary key.
    pub fn find_by_id(&self, id: i64) -> Result<Option<ImageRecord>, RegistryError> {
        let mut stmt = self
            .conn()
            .prepare("SELECT * FROM images WHERE id = ?1")
            .map_err(|e| RegistryError::Database(e.to_string()))?;

        let mut rows = stmt
            .query_map(params![id], row_to_image_record)
            .map_err(|e| RegistryError::Database(e.to_string()))?;

        match rows.next() {
            Some(Ok(record)) => Ok(Some(record)),
            Some(Err(e)) => Err(RegistryError::Database(e.to_string())),
            None => Ok(None),
        }
    }

    /// List images with optional name, tag, and date range filters.
    ///
    /// Results are sorted by `created_at DESC` (newest first).
    pub fn list_images(
        &self,
        name_filter: Option<&str>,
        tag_filter: Option<&str>,
        created_after: Option<i64>,
        created_before: Option<i64>,
    ) -> Result<Vec<ImageRecord>, RegistryError> {
        let mut sql = String::from("SELECT * FROM images WHERE 1=1");
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(name) = name_filter {
            sql.push_str(" AND name = ?");
            param_values.push(Box::new(name.to_string()));
        }
        if let Some(tag) = tag_filter {
            sql.push_str(" AND tag = ?");
            param_values.push(Box::new(tag.to_string()));
        }
        if let Some(after) = created_after {
            sql.push_str(" AND created_at >= ?");
            param_values.push(Box::new(after));
        }
        if let Some(before) = created_before {
            sql.push_str(" AND created_at <= ?");
            param_values.push(Box::new(before));
        }
        sql.push_str(" ORDER BY created_at DESC");

        let params_refs: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|p| p.as_ref()).collect();

        let mut stmt = self
            .conn()
            .prepare(&sql)
            .map_err(|e| RegistryError::Database(e.to_string()))?;

        let rows = stmt
            .query_map(params_refs.as_slice(), row_to_image_record)
            .map_err(|e| RegistryError::Database(e.to_string()))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| RegistryError::Database(e.to_string()))?);
        }
        Ok(results)
    }

    /// Delete an image from the registry and remove its disk file.
    ///
    /// Fails with [`RegistryError::ImageInUse`] if any VMs reference this
    /// image. The database record is deleted **before** the disk file so that
    /// a disk-deletion failure leaves an orphaned file rather than a dangling
    /// DB record.
    pub fn delete_image(&self, id: i64) -> Result<(), RegistryError> {
        // 1. Check for running VMs.
        let vm_count: i64 = self
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM running_vms WHERE image_id = ?1",
                params![id],
                |row| row.get(0),
            )
            .map_err(|e| RegistryError::Database(e.to_string()))?;

        if vm_count > 0 {
            return Err(RegistryError::ImageInUse {
                image_id: id,
                vm_count: vm_count as u32,
            });
        }

        // 2. Fetch the record to get disk_path.
        let record = self
            .find_by_id(id)?
            .ok_or(RegistryError::ImageNotFound { id })?;

        // 3. Delete the database record first.
        self.delete_image_record(id)?;

        // 4. Delete the disk file (best-effort after DB delete).
        let disk = Path::new(&record.disk_path);
        if disk.exists() {
            std::fs::remove_file(disk).map_err(|e| RegistryError::DiskCleanupFailed {
                path: disk.to_path_buf(),
                message: e.to_string(),
            })?;
        }

        Ok(())
    }

    /// List images visible to a specific user.
    ///
    /// Returns platform bundles (`owner_id IS NULL`) plus the user's own bundles.
    /// Admins see all bundles regardless of ownership.
    pub fn list_images_for_user(
        &self,
        user_id: Option<&str>,
        is_admin: bool,
    ) -> Result<Vec<ImageRecord>, RegistryError> {
        if is_admin {
            // Admins see everything
            return self.list_images(None, None, None, None);
        }

        let user_id = user_id.unwrap_or("");
        let mut stmt = self
            .conn()
            .prepare(
                "SELECT * FROM images WHERE owner_id IS NULL OR owner_id = ?1 \
                 ORDER BY created_at DESC",
            )
            .map_err(|e| RegistryError::Database(e.to_string()))?;

        let rows = stmt
            .query_map(params![user_id], row_to_image_record)
            .map_err(|e| RegistryError::Database(e.to_string()))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| RegistryError::Database(e.to_string()))?);
        }
        Ok(results)
    }

    /// Check if a user can access a specific bundle.
    ///
    /// Returns `Ok(ImageRecord)` if allowed, `Err` if not found or forbidden.
    /// Rules:
    /// - Platform bundles (`owner_id=NULL`) are accessible to all users.
    /// - User-owned bundles are only accessible to the owner.
    /// - Admins bypass all ownership checks.
    pub fn check_bundle_access(
        &self,
        bundle_name: &str,
        bundle_tag: &str,
        user_id: &str,
        is_admin: bool,
    ) -> Result<ImageRecord, RegistryError> {
        let image = self
            .find_by_name_tag(bundle_name, bundle_tag)?
            .ok_or(RegistryError::ImageNotFound { id: -1 })?;

        if is_admin {
            return Ok(image);
        }

        match &image.owner_id {
            None => Ok(image), // Platform bundle -- accessible to all
            Some(owner) if owner == user_id => Ok(image), // User's own bundle
            _ => Err(RegistryError::AccessDenied {
                message: "You don't have access to this bundle".to_string(),
            }),
        }
    }

    /// Update the `owner_id` of an existing image.
    ///
    /// - Pass `Some(user_id)` to assign the image to a user (private bundle).
    /// - Pass `None` to make the image a platform bundle (visible to all users).
    pub fn set_owner(&self, id: i64, owner_id: Option<&str>) -> Result<(), RegistryError> {
        self.conn()
            .execute(
                "UPDATE images SET owner_id = ?1 WHERE id = ?2",
                params![owner_id, id],
            )
            .map_err(|e| RegistryError::Database(e.to_string()))?;
        Ok(())
    }

    /// Update the manifest columns for an existing image.
    ///
    /// Called by the upload pipeline after a successful build to store the
    /// parsed manifest sections as JSON. Pass `None` for all parameters when
    /// there is no `dimension.toml` in the bundle (stores NULL columns).
    ///
    /// # Arguments
    ///
    /// * `id` - Primary key of the image record to update.
    /// * `resources` - JSON-serialized ResourcesSection, or None.
    /// * `env` - JSON-serialized EnvSection.vars HashMap, or None.
    /// * `secrets` - JSON-serialized SecretsSection, or None.
    /// * `capabilities` - JSON-serialized CapabilitiesSection, or None.
    /// * `a2a` - JSON-serialized A2aSection, or None.
    /// * `timeout_secs` - Execution timeout from [resources].timeout_secs, or None.
    /// * `volumes` - JSON-serialized VolumesSection, or None.
    #[allow(clippy::too_many_arguments)]
    pub fn update_manifest(
        &self,
        id: i64,
        resources: Option<&str>,
        env: Option<&str>,
        secrets: Option<&str>,
        capabilities: Option<&str>,
        a2a: Option<&str>,
        timeout_secs: Option<i64>,
        volumes: Option<&str>,
    ) -> Result<(), RegistryError> {
        self.conn()
            .execute(
                "UPDATE images SET \
                 manifest_resources = ?1, \
                 manifest_env = ?2, \
                 manifest_secrets = ?3, \
                 manifest_capabilities = ?4, \
                 manifest_a2a = ?5, \
                 manifest_timeout_secs = ?6, \
                 manifest_volumes = ?7 \
                 WHERE id = ?8",
                params![resources, env, secrets, capabilities, a2a, timeout_secs, volumes, id],
            )
            .map_err(|e| RegistryError::Database(e.to_string()))?;
        Ok(())
    }

    /// List all versions of an image by name and owner, sorted by `created_at DESC`.
    ///
    /// Returns all image records matching the given name and owner_id
    /// (owner_id=None matches platform bundles). Newest versions come first.
    pub fn list_versions_by_name(
        &self,
        name: &str,
        owner_id: Option<&str>,
    ) -> Result<Vec<ImageRecord>, RegistryError> {
        let (sql, param_values): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = match owner_id {
            Some(oid) => (
                "SELECT * FROM images WHERE name = ?1 AND owner_id = ?2 ORDER BY created_at DESC"
                    .to_string(),
                vec![
                    Box::new(name.to_string()) as Box<dyn rusqlite::types::ToSql>,
                    Box::new(oid.to_string()),
                ],
            ),
            None => (
                "SELECT * FROM images WHERE name = ?1 AND owner_id IS NULL ORDER BY created_at DESC"
                    .to_string(),
                vec![Box::new(name.to_string()) as Box<dyn rusqlite::types::ToSql>],
            ),
        };

        let params_refs: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|p| p.as_ref()).collect();

        let mut stmt = self
            .conn()
            .prepare(&sql)
            .map_err(|e| RegistryError::Database(e.to_string()))?;

        let rows = stmt
            .query_map(params_refs.as_slice(), row_to_image_record)
            .map_err(|e| RegistryError::Database(e.to_string()))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| RegistryError::Database(e.to_string()))?);
        }
        Ok(results)
    }

    /// Enforce a maximum number of versions per bundle name/owner.
    ///
    /// Keeps the `max_versions` most recent versions (by `created_at`) and
    /// deletes older ones — both the database record and the on-disk ext4 file.
    /// Returns the number of versions garbage-collected.
    pub fn enforce_version_limit(
        &self,
        name: &str,
        owner_id: Option<&str>,
        max_versions: usize,
    ) -> Result<usize, RegistryError> {
        let versions = self.list_versions_by_name(name, owner_id)?;
        if versions.len() <= max_versions {
            return Ok(0);
        }

        let mut gc_count = 0;
        for old in &versions[max_versions..] {
            // Delete database record (no VM checks — these are old versions being GC'd).
            self.delete_image_record(old.id)?;

            // Best-effort cleanup of the on-disk ext4 file.
            let disk = Path::new(&old.disk_path);
            if disk.exists() {
                if let Err(e) = std::fs::remove_file(disk) {
                    eprintln!(
                        "warning: failed to clean up old version file {}: {}",
                        old.disk_path, e
                    );
                }
            }
            gc_count += 1;
        }

        Ok(gc_count)
    }

    /// Delete a database record without VM checks or disk cleanup.
    ///
    /// Used internally by [`register_image`] for re-tag conflict resolution.
    fn delete_image_record(&self, id: i64) -> Result<(), RegistryError> {
        self.conn()
            .execute("DELETE FROM images WHERE id = ?1", params![id])
            .map_err(|e| RegistryError::Database(e.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_registry() -> (Registry, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::open(dir.path()).unwrap();
        (registry, dir)
    }

    fn sample_image(hash: &str, name: &str, tag: &str, disk_path: &str) -> NewImage {
        NewImage {
            content_hash: hash.to_string(),
            name: name.to_string(),
            tag: tag.to_string(),
            size_bytes: 1024,
            source_path: "/tmp/src".to_string(),
            init_config: None,
            disk_path: disk_path.to_string(),
            created_at: 1000,
            default_vcpus: 2,
            default_memory_mib: 256,
            owner_id: None,
            manifest_resources: None,
            manifest_env: None,
            manifest_secrets: None,
            manifest_capabilities: None,
            manifest_a2a: None,
            manifest_timeout_secs: None,
            manifest_volumes: None,
        }
    }

    #[test]
    fn test_parse_image_ref_with_tag() {
        let (name, tag) = parse_image_ref("my-app:v1.0");
        assert_eq!(name, "my-app");
        assert_eq!(tag, "v1.0");
    }

    #[test]
    fn test_parse_image_ref_without_tag() {
        let (name, tag) = parse_image_ref("my-app");
        assert_eq!(name, "my-app");
        assert_eq!(tag, "latest");
    }

    #[test]
    fn test_parse_image_ref_empty_tag() {
        let (name, tag) = parse_image_ref("my-app:");
        assert_eq!(name, "my-app:");
        assert_eq!(tag, "latest");
    }

    #[test]
    fn test_derive_image_name() {
        let name = derive_image_name(Path::new("/home/user/my-app")).unwrap();
        assert_eq!(name, "my-app");
    }

    #[test]
    fn test_register_and_find() {
        let (reg, _dir) = test_registry();
        let img = sample_image("abc123", "my-app", "v1", "/tmp/img.ext4");

        let registered = reg.register_image(&img).unwrap();
        assert_eq!(registered.content_hash, "abc123");
        assert_eq!(registered.name, "my-app");
        assert_eq!(registered.tag, "v1");

        // find_by_hash
        let found = reg.find_by_hash("abc123").unwrap().unwrap();
        assert_eq!(found.id, registered.id);

        // find_by_name_tag
        let found = reg.find_by_name_tag("my-app", "v1").unwrap().unwrap();
        assert_eq!(found.id, registered.id);

        // find_by_id
        let found = reg.find_by_id(registered.id).unwrap().unwrap();
        assert_eq!(found.content_hash, "abc123");
    }

    #[test]
    fn test_cache_hit() {
        let (reg, _dir) = test_registry();
        let img1 = sample_image("abc123", "my-app", "v1", "/tmp/img.ext4");
        let registered = reg.register_image(&img1).unwrap();

        // Same content_hash, different name:tag -- should return existing record.
        let img2 = sample_image("abc123", "other-app", "v2", "/tmp/other.ext4");
        let cached = reg.register_image(&img2).unwrap();

        assert_eq!(cached.id, registered.id);
        assert_eq!(cached.name, "my-app"); // Original name, not the new one.
    }

    #[test]
    fn test_list_images_with_filters() {
        let (reg, _dir) = test_registry();

        let img1 = NewImage {
            content_hash: "hash1".to_string(),
            name: "app-a".to_string(),
            tag: "v1".to_string(),
            size_bytes: 1024,
            source_path: "/tmp/a".to_string(),
            init_config: None,
            disk_path: "/tmp/a.ext4".to_string(),
            created_at: 1000,
            default_vcpus: 2,
            default_memory_mib: 256,
            owner_id: None,
            manifest_resources: None,
            manifest_env: None,
            manifest_secrets: None,
            manifest_capabilities: None,
            manifest_a2a: None,
            manifest_timeout_secs: None,
            manifest_volumes: None,
        };
        let img2 = NewImage {
            content_hash: "hash2".to_string(),
            name: "app-a".to_string(),
            tag: "v2".to_string(),
            size_bytes: 2048,
            source_path: "/tmp/a".to_string(),
            init_config: None,
            disk_path: "/tmp/a2.ext4".to_string(),
            created_at: 2000,
            default_vcpus: 2,
            default_memory_mib: 256,
            owner_id: None,
            manifest_resources: None,
            manifest_env: None,
            manifest_secrets: None,
            manifest_capabilities: None,
            manifest_a2a: None,
            manifest_timeout_secs: None,
            manifest_volumes: None,
        };
        let img3 = NewImage {
            content_hash: "hash3".to_string(),
            name: "app-b".to_string(),
            tag: "v1".to_string(),
            size_bytes: 4096,
            source_path: "/tmp/b".to_string(),
            init_config: None,
            disk_path: "/tmp/b.ext4".to_string(),
            created_at: 3000,
            default_vcpus: 2,
            default_memory_mib: 256,
            owner_id: None,
            manifest_resources: None,
            manifest_env: None,
            manifest_secrets: None,
            manifest_capabilities: None,
            manifest_a2a: None,
            manifest_timeout_secs: None,
            manifest_volumes: None,
        };

        reg.register_image(&img1).unwrap();
        reg.register_image(&img2).unwrap();
        reg.register_image(&img3).unwrap();

        // No filter -- all 3, newest first.
        let all = reg.list_images(None, None, None, None).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].name, "app-b"); // created_at=3000

        // Name filter.
        let filtered = reg.list_images(Some("app-a"), None, None, None).unwrap();
        assert_eq!(filtered.len(), 2);

        // Tag filter.
        let filtered = reg.list_images(None, Some("v1"), None, None).unwrap();
        assert_eq!(filtered.len(), 2);

        // Both filters.
        let filtered = reg.list_images(Some("app-a"), Some("v1"), None, None).unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].content_hash, "hash1");
    }

    #[test]
    fn test_delete_image() {
        let (reg, dir) = test_registry();

        // Create a fake disk file so delete_image can remove it.
        let disk_path = dir.path().join("images").join("test.ext4");
        std::fs::write(&disk_path, "fake image data").unwrap();

        let img = sample_image("del123", "to-delete", "v1", disk_path.to_str().unwrap());
        let registered = reg.register_image(&img).unwrap();

        reg.delete_image(registered.id).unwrap();

        // Record gone.
        assert!(reg.find_by_id(registered.id).unwrap().is_none());
        // Disk file gone.
        assert!(!disk_path.exists());
    }

    fn make_image_with_owner(
        hash: &str,
        name: &str,
        tag: &str,
        owner_id: Option<&str>,
    ) -> NewImage {
        NewImage {
            content_hash: hash.to_string(),
            name: name.to_string(),
            tag: tag.to_string(),
            size_bytes: 1024,
            source_path: "/tmp/src".to_string(),
            init_config: None,
            disk_path: format!("/tmp/{hash}.ext4"),
            created_at: 1000,
            default_vcpus: 2,
            default_memory_mib: 256,
            owner_id: owner_id.map(|s| s.to_string()),
            manifest_resources: None,
            manifest_env: None,
            manifest_secrets: None,
            manifest_capabilities: None,
            manifest_a2a: None,
            manifest_timeout_secs: None,
            manifest_volumes: None,
        }
    }

    #[test]
    fn test_list_images_for_user_sees_platform_and_own() {
        let (reg, _dir) = test_registry();

        // Platform bundle (owner_id=None)
        reg.register_image(&make_image_with_owner("h1", "platform-app", "v1", None))
            .unwrap();
        // user-a bundle
        reg.register_image(&make_image_with_owner("h2", "user-a-app", "v1", Some("user-a")))
            .unwrap();
        // user-b bundle
        reg.register_image(&make_image_with_owner("h3", "user-b-app", "v1", Some("user-b")))
            .unwrap();

        // user-a sees: platform + user-a's own (2)
        let user_a = reg.list_images_for_user(Some("user-a"), false).unwrap();
        assert_eq!(user_a.len(), 2, "user-a should see platform + own bundle");

        // user-b sees: platform + user-b's own (2)
        let user_b = reg.list_images_for_user(Some("user-b"), false).unwrap();
        assert_eq!(user_b.len(), 2, "user-b should see platform + own bundle");

        // user-a does not see user-b's bundle
        let user_a_names: Vec<_> = user_a.iter().map(|r| &r.name).collect();
        assert!(!user_a_names.contains(&&"user-b-app".to_string()), "user-a must not see user-b's bundle");
    }

    #[test]
    fn test_list_images_for_admin_sees_all() {
        let (reg, _dir) = test_registry();

        reg.register_image(&make_image_with_owner("h1", "platform-app", "v1", None))
            .unwrap();
        reg.register_image(&make_image_with_owner("h2", "user-a-app", "v1", Some("user-a")))
            .unwrap();
        reg.register_image(&make_image_with_owner("h3", "user-b-app", "v1", Some("user-b")))
            .unwrap();

        // Admin sees all 3
        let all = reg.list_images_for_user(None, true).unwrap();
        assert_eq!(all.len(), 3, "admin should see all bundles");
    }

    #[test]
    fn test_check_bundle_access_platform() {
        let (reg, _dir) = test_registry();

        reg.register_image(&make_image_with_owner("h1", "platform", "v1", None))
            .unwrap();

        // Any user can access platform bundles
        let result = reg.check_bundle_access("platform", "v1", "any-user", false);
        assert!(result.is_ok(), "platform bundle should be accessible to any user");
    }

    #[test]
    fn test_check_bundle_access_own() {
        let (reg, _dir) = test_registry();

        reg.register_image(&make_image_with_owner("h1", "my-bundle", "v1", Some("user-a")))
            .unwrap();

        // Owner can access own bundle
        let result = reg.check_bundle_access("my-bundle", "v1", "user-a", false);
        assert!(result.is_ok(), "owner should be able to access own bundle");
    }

    #[test]
    fn test_check_bundle_access_other_user_denied() {
        let (reg, _dir) = test_registry();

        reg.register_image(&make_image_with_owner("h1", "user-a-bundle", "v1", Some("user-a")))
            .unwrap();

        // user-b cannot access user-a's bundle
        let result = reg.check_bundle_access("user-a-bundle", "v1", "user-b", false);
        assert!(
            matches!(result, Err(hyphae_errors::RegistryError::AccessDenied { .. })),
            "user-b should be denied access to user-a's bundle, got: {:?}",
            result
        );
    }

    #[test]
    fn test_check_bundle_access_admin_bypasses() {
        let (reg, _dir) = test_registry();

        reg.register_image(&make_image_with_owner("h1", "user-a-bundle", "v1", Some("user-a")))
            .unwrap();

        // Admin bypasses ownership check
        let result = reg.check_bundle_access("user-a-bundle", "v1", "any-admin", true);
        assert!(result.is_ok(), "admin should bypass ownership check");
    }

    // --- list_versions_by_name and enforce_version_limit tests ---

    #[test]
    fn test_list_versions_by_name() {
        let (reg, _dir) = test_registry();

        // Register 3 versions of the same bundle name, different tags and hashes.
        for i in 1..=3 {
            let img = NewImage {
                content_hash: format!("ver-hash-{i}"),
                name: "my-service".to_string(),
                tag: format!("v{i}"),
                size_bytes: 1024,
                source_path: "/tmp/src".to_string(),
                init_config: None,
                disk_path: format!("/tmp/ver-{i}.ext4"),
                created_at: i * 1000,
                default_vcpus: 2,
                default_memory_mib: 256,
                owner_id: Some("user-a".to_string()),
                manifest_resources: None,
                manifest_env: None,
                manifest_secrets: None,
                manifest_capabilities: None,
                manifest_a2a: None,
                manifest_timeout_secs: None,
                manifest_volumes: None,
            };
            reg.register_image(&img).unwrap();
        }

        // Also register a bundle with a different owner — should not appear.
        let other = NewImage {
            content_hash: "other-hash".to_string(),
            name: "my-service".to_string(),
            tag: "other-v1".to_string(),
            size_bytes: 1024,
            source_path: "/tmp/src".to_string(),
            init_config: None,
            disk_path: "/tmp/other.ext4".to_string(),
            created_at: 4000,
            default_vcpus: 2,
            default_memory_mib: 256,
            owner_id: Some("user-b".to_string()),
            manifest_resources: None,
            manifest_env: None,
            manifest_secrets: None,
            manifest_capabilities: None,
            manifest_a2a: None,
            manifest_timeout_secs: None,
            manifest_volumes: None,
        };
        reg.register_image(&other).unwrap();

        let versions = reg.list_versions_by_name("my-service", Some("user-a")).unwrap();
        assert_eq!(versions.len(), 3, "should list 3 versions for user-a");
        // Newest first
        assert_eq!(versions[0].created_at, 3000);
        assert_eq!(versions[2].created_at, 1000);
    }

    #[test]
    fn test_enforce_version_limit_registers_4_keeps_3() {
        let (reg, dir) = test_registry();

        // Register 4 versions with distinct content hashes and disk files.
        for i in 1..=4 {
            let disk_path = dir.path().join("images").join(format!("ver-{i}.ext4"));
            std::fs::write(&disk_path, format!("data-{i}")).unwrap();

            let img = NewImage {
                content_hash: format!("limit-hash-{i}"),
                name: "gc-test".to_string(),
                tag: format!("v{i}"),
                size_bytes: 1024,
                source_path: "/tmp/src".to_string(),
                init_config: None,
                disk_path: disk_path.to_string_lossy().to_string(),
                created_at: i * 1000,
                default_vcpus: 2,
                default_memory_mib: 256,
                owner_id: Some("user-gc".to_string()),
                manifest_resources: None,
                manifest_env: None,
                manifest_secrets: None,
                manifest_capabilities: None,
                manifest_a2a: None,
                manifest_timeout_secs: None,
                manifest_volumes: None,
            };
            reg.register_image(&img).unwrap();
        }

        // Verify 4 versions exist before enforcement.
        let before = reg.list_versions_by_name("gc-test", Some("user-gc")).unwrap();
        assert_eq!(before.len(), 4);

        // Enforce limit of 3.
        let gc_count = reg.enforce_version_limit("gc-test", Some("user-gc"), 3).unwrap();
        assert_eq!(gc_count, 1, "should GC exactly 1 old version");

        // Verify 3 versions remain.
        let after = reg.list_versions_by_name("gc-test", Some("user-gc")).unwrap();
        assert_eq!(after.len(), 3, "should keep exactly 3 versions after GC");

        // The oldest version (created_at=1000) should have been removed.
        assert!(
            after.iter().all(|v| v.created_at >= 2000),
            "oldest version (created_at=1000) should have been GC'd"
        );

        // The old disk file should have been cleaned up.
        let old_disk = dir.path().join("images").join("ver-1.ext4");
        assert!(!old_disk.exists(), "GC'd version disk file should be removed");
    }

    #[test]
    fn test_enforce_version_limit_noop_when_within_limit() {
        let (reg, _dir) = test_registry();

        // Register 2 versions (within limit of 3).
        for i in 1..=2 {
            let img = NewImage {
                content_hash: format!("noop-hash-{i}"),
                name: "small-app".to_string(),
                tag: format!("v{i}"),
                size_bytes: 1024,
                source_path: "/tmp/src".to_string(),
                init_config: None,
                disk_path: format!("/tmp/noop-{i}.ext4"),
                created_at: i * 1000,
                default_vcpus: 2,
                default_memory_mib: 256,
                owner_id: Some("user-x".to_string()),
                manifest_resources: None,
                manifest_env: None,
                manifest_secrets: None,
                manifest_capabilities: None,
                manifest_a2a: None,
                manifest_timeout_secs: None,
                manifest_volumes: None,
            };
            reg.register_image(&img).unwrap();
        }

        let gc_count = reg.enforce_version_limit("small-app", Some("user-x"), 3).unwrap();
        assert_eq!(gc_count, 0, "should not GC when within version limit");
    }
}
