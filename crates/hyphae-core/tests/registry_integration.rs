//! Integration tests for the image registry module.
//!
//! These tests exercise the full registry as a black box from outside the
//! crate, covering the end-to-end lifecycle: registration, VM tracking,
//! deletion safety, cache hits, re-tagging, filtering, DB reopening, and
//! JSON serialization.

use hyphae_core::registry::{image_file_path, NewImage, Registry};

/// Helper: open a registry rooted in a fresh temp directory.
fn open_temp_registry() -> (Registry, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::open(dir.path()).unwrap();
    (reg, dir)
}

/// Helper: build a [`NewImage`] with the given distinguishing fields.
fn make_image(hash: &str, name: &str, tag: &str, disk_path: &str) -> NewImage {
    NewImage {
        content_hash: hash.to_string(),
        name: name.to_string(),
        tag: tag.to_string(),
        size_bytes: 4096,
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

// -------------------------------------------------------------------------
// Test 1: Full lifecycle
// -------------------------------------------------------------------------

#[test]
fn test_full_lifecycle() {
    let (reg, dir) = open_temp_registry();

    // Create a real disk file so delete_image can remove it.
    let storage = dir.path().join("images");
    let disk_path = image_file_path(&storage, "fullhash");
    std::fs::write(&disk_path, b"fake rootfs data").unwrap();
    assert!(disk_path.exists(), "disk file should exist before test");

    // 1. Register an image.
    let img = make_image("fullhash", "lifecycle-app", "v1", disk_path.to_str().unwrap());
    let registered = reg.register_image(&img).unwrap();
    assert_eq!(registered.name, "lifecycle-app");

    // 2. Track a VM against the image.
    reg.track_vm("vm-lifecycle-1", registered.id).unwrap();

    // 3. Attempt to delete -- should fail with ImageInUse.
    let del_result = reg.delete_image(registered.id);
    assert!(del_result.is_err(), "delete should be blocked while VM is running");
    let err_str = del_result.unwrap_err().to_string();
    assert!(
        err_str.contains("still running"),
        "error should mention running VMs, got: {err_str}"
    );

    // 4. Untrack the VM.
    let removed = reg.untrack_vm("vm-lifecycle-1").unwrap();
    assert!(removed, "untrack should return true");

    // 5. Delete should now succeed.
    reg.delete_image(registered.id).unwrap();

    // 6. Record gone.
    assert!(
        reg.find_by_id(registered.id).unwrap().is_none(),
        "image record should be deleted from database"
    );

    // 7. Disk file removed.
    assert!(
        !disk_path.exists(),
        "disk file should be removed after delete_image"
    );
}

// -------------------------------------------------------------------------
// Test 2: Cache hit returns existing record with same id
// -------------------------------------------------------------------------

#[test]
fn test_cache_hit_behavior() {
    let (reg, _dir) = open_temp_registry();

    let img1 = make_image("abc123", "my-app", "latest", "/tmp/img.ext4");
    let first = reg.register_image(&img1).unwrap();

    // Same content_hash, different name:tag -- should return existing.
    let img2 = make_image("abc123", "other-app", "v2", "/tmp/other.ext4");
    let cached = reg.register_image(&img2).unwrap();

    assert_eq!(
        cached.id, first.id,
        "cache hit should return the same record id"
    );
    assert_eq!(
        cached.name, "my-app",
        "cache hit should preserve original name"
    );

    // Only 1 image in the registry.
    let all = reg.list_images(None, None, None, None).unwrap();
    assert_eq!(all.len(), 1, "cache hit should not create a second record");
}

// -------------------------------------------------------------------------
// Test 3: Re-tag replaces old record
// -------------------------------------------------------------------------

#[test]
fn test_retag_replaces_old() {
    let (reg, dir) = open_temp_registry();

    let storage = dir.path().join("images");

    // Create disk files for both hashes.
    let disk_a = image_file_path(&storage, "hash_old");
    let disk_b = image_file_path(&storage, "hash_new");
    std::fs::write(&disk_a, b"old").unwrap();
    std::fs::write(&disk_b, b"new").unwrap();

    // Register first image.
    let img_a = make_image("hash_old", "my-app", "latest", disk_a.to_str().unwrap());
    let a = reg.register_image(&img_a).unwrap();

    // Register with same name:tag but different content -- re-tag.
    let img_b = make_image("hash_new", "my-app", "latest", disk_b.to_str().unwrap());
    let b = reg.register_image(&img_b).unwrap();

    // New record should have a different id and the new hash.
    assert_ne!(b.id, a.id, "re-tag should create a new record");
    assert_eq!(b.content_hash, "hash_new");

    // find_by_name_tag should return the new one.
    let found = reg
        .find_by_name_tag("my-app", "latest")
        .unwrap()
        .expect("should find re-tagged image");
    assert_eq!(found.content_hash, "hash_new");

    // Only 1 image total (old was replaced).
    let all = reg.list_images(None, None, None, None).unwrap();
    assert_eq!(all.len(), 1, "re-tag should replace, not append");

    // Old disk file should be cleaned up.
    assert!(
        !disk_a.exists(),
        "old disk file should be removed on re-tag"
    );
}

// -------------------------------------------------------------------------
// Test 4: Multiple images with VM filtering
// -------------------------------------------------------------------------

#[test]
fn test_multiple_images_and_filtering() {
    let (reg, _dir) = open_temp_registry();

    // Register 3 images with distinct hashes.
    let imgs = [
        make_image("h1", "app-a", "latest", "/tmp/a1.ext4"),
        make_image("h2", "app-a", "v1.0", "/tmp/a2.ext4"),
        make_image("h3", "app-b", "latest", "/tmp/b1.ext4"),
    ];
    let mut ids = Vec::new();
    for img in &imgs {
        let r = reg.register_image(img).unwrap();
        ids.push(r.id);
    }

    // Listing filters.
    assert_eq!(reg.list_images(None, None, None, None).unwrap().len(), 3);
    assert_eq!(reg.list_images(Some("app-a"), None, None, None).unwrap().len(), 2);
    assert_eq!(reg.list_images(None, Some("latest"), None, None).unwrap().len(), 2);
    assert_eq!(reg.list_images(Some("app-a"), Some("v1.0"), None, None).unwrap().len(), 1);
    assert_eq!(
        reg.list_images(Some("nonexistent"), None, None, None).unwrap().len(),
        0
    );

    // Track VMs against different images.
    reg.track_vm("vm-a1", ids[0]).unwrap();
    reg.track_vm("vm-a2", ids[0]).unwrap();
    reg.track_vm("vm-b1", ids[2]).unwrap();

    // list_vms_for_image filtering.
    assert_eq!(
        reg.list_vms_for_image(ids[0]).unwrap().len(),
        2,
        "app-a:latest should have 2 VMs"
    );
    assert_eq!(
        reg.list_vms_for_image(ids[1]).unwrap().len(),
        0,
        "app-a:v1.0 should have 0 VMs"
    );
    assert_eq!(
        reg.list_vms_for_image(ids[2]).unwrap().len(),
        1,
        "app-b:latest should have 1 VM"
    );

    // list_all_vms.
    assert_eq!(
        reg.list_all_vms().unwrap().len(),
        3,
        "total VMs should be 3"
    );

    // Untrack all and verify.
    reg.untrack_vm("vm-a1").unwrap();
    reg.untrack_vm("vm-a2").unwrap();
    reg.untrack_vm("vm-b1").unwrap();
    assert!(reg.list_all_vms().unwrap().is_empty());
}

// -------------------------------------------------------------------------
// Test 5: Date range filtering
// -------------------------------------------------------------------------

#[test]
fn test_date_range_filtering() {
    let (reg, _dir) = open_temp_registry();

    // Register images with different created_at timestamps.
    let mut img1 = make_image("date1", "app-d", "v1", "/tmp/d1.ext4");
    img1.created_at = 1000;
    let mut img2 = make_image("date2", "app-d", "v2", "/tmp/d2.ext4");
    img2.created_at = 2000;
    let mut img3 = make_image("date3", "app-d", "v3", "/tmp/d3.ext4");
    img3.created_at = 3000;

    reg.register_image(&img1).unwrap();
    reg.register_image(&img2).unwrap();
    reg.register_image(&img3).unwrap();

    // Filter: created_after=1500 -> should get img2 and img3.
    let after = reg.list_images(None, None, Some(1500), None).unwrap();
    assert_eq!(after.len(), 2, "created_after=1500 should return 2 images");

    // Filter: created_before=2500 -> should get img1 and img2.
    let before = reg.list_images(None, None, None, Some(2500)).unwrap();
    assert_eq!(before.len(), 2, "created_before=2500 should return 2 images");

    // Filter: created_after=1500 AND created_before=2500 -> should get img2 only.
    let range = reg.list_images(None, None, Some(1500), Some(2500)).unwrap();
    assert_eq!(range.len(), 1, "date range should return 1 image");
    assert_eq!(range[0].content_hash, "date2");

    // Combined: name + date range.
    let combined = reg.list_images(Some("app-d"), None, Some(2500), None).unwrap();
    assert_eq!(combined.len(), 1, "name + date filter should return 1 image");
    assert_eq!(combined[0].tag, "v3");
}

// -------------------------------------------------------------------------
// Test 6: DB reopening passes integrity
// -------------------------------------------------------------------------

#[test]
fn test_integrity_check_passes_on_valid_db() {
    let dir = tempfile::tempdir().unwrap();

    // First open -- creates DB.
    {
        let reg = Registry::open(dir.path()).unwrap();
        let img = make_image("persist1", "persisted-app", "v1", "/tmp/p.ext4");
        reg.register_image(&img).unwrap();
    }

    // Second open -- reopens existing DB, runs integrity check.
    {
        let reg = Registry::open(dir.path()).unwrap();
        let found = reg
            .find_by_name_tag("persisted-app", "v1")
            .unwrap()
            .expect("data should persist across reopens");
        assert_eq!(found.content_hash, "persist1");
    }
}

// -------------------------------------------------------------------------
// Test 6: JSON serialization
// -------------------------------------------------------------------------

#[test]
fn test_json_serialization() {
    let (reg, _dir) = open_temp_registry();

    let img = make_image("ser123", "json-app", "v1", "/tmp/ser.ext4");
    let registered = reg.register_image(&img).unwrap();

    // Serialize ImageRecord.
    let img_json = serde_json::to_string(&registered).unwrap();
    assert!(
        img_json.contains("\"content_hash\""),
        "ImageRecord JSON should contain content_hash"
    );
    assert!(
        img_json.contains("\"name\""),
        "ImageRecord JSON should contain name"
    );
    assert!(
        img_json.contains("\"tag\""),
        "ImageRecord JSON should contain tag"
    );
    assert!(
        img_json.contains("\"size_bytes\""),
        "ImageRecord JSON should contain size_bytes"
    );

    // Track a VM and serialize VmRecord.
    reg.track_vm("vm-json-1", registered.id).unwrap();
    let vms = reg.list_vms_for_image(registered.id).unwrap();
    assert_eq!(vms.len(), 1);

    let vm_json = serde_json::to_string(&vms[0]).unwrap();
    assert!(
        vm_json.contains("\"vm_id\""),
        "VmRecord JSON should contain vm_id"
    );
    assert!(
        vm_json.contains("\"image_id\""),
        "VmRecord JSON should contain image_id"
    );
    assert!(
        vm_json.contains("\"started_at\""),
        "VmRecord JSON should contain started_at"
    );
}
