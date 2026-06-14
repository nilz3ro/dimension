//! VM-to-image association tracking.
//!
//! Records which VMs are currently running against which images so that
//! [`super::Registry::delete_image`] can refuse deletion while a VM is
//! still active (REGR-04 safety check).

use std::time::{SystemTime, UNIX_EPOCH};

use hyphae_errors::RegistryError;
use rusqlite::params;

use super::Registry;

/// A record of a running VM associated with an image.
#[derive(Debug, Clone, serde::Serialize)]
pub struct VmRecord {
    pub vm_id: String,
    pub image_id: i64,
    pub started_at: i64,
}

/// Map a database row to a [`VmRecord`].
fn row_to_vm_record(row: &rusqlite::Row) -> rusqlite::Result<VmRecord> {
    Ok(VmRecord {
        vm_id: row.get("vm_id")?,
        image_id: row.get("image_id")?,
        started_at: row.get("started_at")?,
    })
}

impl Registry {
    /// Record a VM as running against an image.
    ///
    /// Validates that `image_id` refers to an existing image before inserting.
    /// The `started_at` timestamp is set to the current Unix epoch in seconds.
    ///
    /// # Errors
    ///
    /// - [`RegistryError::ImageNotFound`] if no image exists with the given id.
    /// - [`RegistryError::Database`] on SQL errors (e.g. duplicate `vm_id`).
    pub fn track_vm(&self, vm_id: &str, image_id: i64) -> Result<(), RegistryError> {
        // Verify the image exists.
        if self.find_by_id(image_id)?.is_none() {
            return Err(RegistryError::ImageNotFound { id: image_id });
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        self.conn()
            .execute(
                "INSERT INTO running_vms (vm_id, image_id, started_at) VALUES (?1, ?2, ?3)",
                params![vm_id, image_id, now],
            )
            .map_err(|e| RegistryError::Database(e.to_string()))?;

        Ok(())
    }

    /// Remove a VM tracking record.
    ///
    /// Returns `true` if a record was deleted, `false` if `vm_id` was not
    /// found. This is intentionally idempotent -- untracking a VM that is
    /// not tracked is not an error.
    pub fn untrack_vm(&self, vm_id: &str) -> Result<bool, RegistryError> {
        let changes = self
            .conn()
            .execute(
                "DELETE FROM running_vms WHERE vm_id = ?1",
                params![vm_id],
            )
            .map_err(|e| RegistryError::Database(e.to_string()))?;

        Ok(changes > 0)
    }

    /// Check whether any VMs are currently running against an image.
    pub fn image_has_running_vms(&self, image_id: i64) -> Result<bool, RegistryError> {
        let count: i64 = self
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM running_vms WHERE image_id = ?1",
                params![image_id],
                |row| row.get(0),
            )
            .map_err(|e| RegistryError::Database(e.to_string()))?;

        Ok(count > 0)
    }

    /// List all running VMs for a specific image.
    ///
    /// Results are sorted by `started_at DESC` (most recently started first).
    pub fn list_vms_for_image(&self, image_id: i64) -> Result<Vec<VmRecord>, RegistryError> {
        let mut stmt = self
            .conn()
            .prepare("SELECT * FROM running_vms WHERE image_id = ?1 ORDER BY started_at DESC")
            .map_err(|e| RegistryError::Database(e.to_string()))?;

        let rows = stmt
            .query_map(params![image_id], row_to_vm_record)
            .map_err(|e| RegistryError::Database(e.to_string()))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| RegistryError::Database(e.to_string()))?);
        }
        Ok(results)
    }

    /// List all running VMs across all images.
    ///
    /// Results are sorted by `started_at DESC` (most recently started first).
    pub fn list_all_vms(&self) -> Result<Vec<VmRecord>, RegistryError> {
        let mut stmt = self
            .conn()
            .prepare("SELECT * FROM running_vms ORDER BY started_at DESC")
            .map_err(|e| RegistryError::Database(e.to_string()))?;

        let rows = stmt
            .query_map([], row_to_vm_record)
            .map_err(|e| RegistryError::Database(e.to_string()))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| RegistryError::Database(e.to_string()))?);
        }
        Ok(results)
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

    fn sample_image(hash: &str) -> crate::registry::image::NewImage {
        crate::registry::image::NewImage {
            content_hash: hash.to_string(),
            name: "test-app".to_string(),
            tag: "latest".to_string(),
            size_bytes: 1024,
            source_path: "/tmp/src".to_string(),
            init_config: None,
            disk_path: "/tmp/img.ext4".to_string(),
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
    fn test_track_and_untrack_vm() {
        let (reg, _dir) = test_registry();
        let img = reg.register_image(&sample_image("abc123")).unwrap();

        reg.track_vm("vm-001", img.id).unwrap();

        let vms = reg.list_vms_for_image(img.id).unwrap();
        assert_eq!(vms.len(), 1, "should have 1 VM tracked");
        assert_eq!(vms[0].vm_id, "vm-001");
        assert_eq!(vms[0].image_id, img.id);

        let removed = reg.untrack_vm("vm-001").unwrap();
        assert!(removed, "untrack should return true when VM existed");

        let vms = reg.list_vms_for_image(img.id).unwrap();
        assert!(vms.is_empty(), "should have 0 VMs after untrack");
    }

    #[test]
    fn test_track_vm_invalid_image() {
        let (reg, _dir) = test_registry();

        let result = reg.track_vm("vm-001", 9999);
        assert!(result.is_err(), "track_vm with invalid image_id should fail");

        let err = result.unwrap_err();
        let err_msg = err.to_string();
        assert!(
            err_msg.contains("not found"),
            "error should indicate image not found, got: {err_msg}"
        );
    }

    #[test]
    fn test_untrack_nonexistent_vm() {
        let (reg, _dir) = test_registry();

        let removed = reg.untrack_vm("nonexistent-vm").unwrap();
        assert!(
            !removed,
            "untrack of nonexistent VM should return false, not error"
        );
    }

    #[test]
    fn test_list_all_vms() {
        let (reg, _dir) = test_registry();
        let img1 = reg
            .register_image(&crate::registry::image::NewImage {
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
            })
            .unwrap();
        let img2 = reg
            .register_image(&crate::registry::image::NewImage {
                content_hash: "hash2".to_string(),
                name: "app-b".to_string(),
                tag: "v1".to_string(),
                size_bytes: 2048,
                source_path: "/tmp/b".to_string(),
                init_config: None,
                disk_path: "/tmp/b.ext4".to_string(),
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
            })
            .unwrap();

        reg.track_vm("vm-a1", img1.id).unwrap();
        reg.track_vm("vm-b1", img2.id).unwrap();

        let all = reg.list_all_vms().unwrap();
        assert_eq!(all.len(), 2, "should list all VMs across images");

        // Both images should be represented.
        let image_ids: Vec<i64> = all.iter().map(|v| v.image_id).collect();
        assert!(image_ids.contains(&img1.id), "should contain img1 VM");
        assert!(image_ids.contains(&img2.id), "should contain img2 VM");
    }

    #[test]
    fn test_image_has_running_vms() {
        let (reg, _dir) = test_registry();
        let img = reg.register_image(&sample_image("abc123")).unwrap();

        assert!(
            !reg.image_has_running_vms(img.id).unwrap(),
            "no VMs tracked yet"
        );

        reg.track_vm("vm-001", img.id).unwrap();
        assert!(
            reg.image_has_running_vms(img.id).unwrap(),
            "should have running VMs after track"
        );

        reg.untrack_vm("vm-001").unwrap();
        assert!(
            !reg.image_has_running_vms(img.id).unwrap(),
            "no VMs after untrack"
        );
    }
}
