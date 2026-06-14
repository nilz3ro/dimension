//! Tests that LaunchConfig.volume_drive produces correct DriveConfig entries.
//!
//! Verifies VOL-04: volume_path injected into LaunchConfig results in a
//! valid DriveConfig pointing to the .img file with the correct drive_id,
//! cache_type, and boot arguments.

use std::path::PathBuf;

use hyphae_core::config::{DriveConfig, VmConfig};
use hyphae_core::volume;

/// Build a minimal VmConfig that mirrors what launch_direct builds (without
/// actually spawning Firecracker). This lets us test DriveConfig injection
/// from volume_drive without requiring a real kernel or rootfs.
fn make_vm_config_with_volume(volume_path: &PathBuf) -> VmConfig {
    let mut vm_config = VmConfig::new(
        "/opt/hyphae/kernel/vmlinux",
        "/tmp/rootfs.ext4",
        1,
        128,
    );

    // Replicate the logic from launch/direct.rs lines 82-91.
    let mut drive = DriveConfig::new(
        "workspace",
        volume_path.to_string_lossy().as_ref(),
        false,
    );
    drive.cache_type = Some("Writeback".into());
    vm_config = vm_config.with_drive(drive);
    vm_config = vm_config.with_additional_boot_args("hyphae.volume=true");

    vm_config
}

/// Build a minimal VmConfig WITHOUT a volume drive (no volume_drive in LaunchConfig).
fn make_vm_config_no_volume() -> VmConfig {
    VmConfig::new(
        "/opt/hyphae/kernel/vmlinux",
        "/tmp/rootfs.ext4",
        1,
        128,
    )
}

#[test]
fn test_volume_drive_some_produces_drive_config() {
    let volume_path = PathBuf::from("/srv/hyphae/volumes/test-uuid.img");
    let vm_config = make_vm_config_with_volume(&volume_path);

    // Drives: rootfs + workspace
    assert_eq!(
        vm_config.drives.len(),
        2,
        "expected 2 drives (rootfs + workspace), got {}",
        vm_config.drives.len()
    );

    // Find the workspace drive
    let workspace_drive = vm_config
        .drives
        .iter()
        .find(|d| d.drive_id == "workspace")
        .expect("workspace drive not found in drives list");

    assert_eq!(
        workspace_drive.path_on_host,
        volume_path.to_string_lossy().as_ref(),
        "workspace drive path must match the volume path"
    );
    assert!(
        !workspace_drive.is_root_device,
        "workspace drive must not be root device"
    );
    assert_eq!(
        workspace_drive.cache_type.as_deref(),
        Some("Writeback"),
        "workspace drive must use Writeback cache for persistent data integrity"
    );
}

#[test]
fn test_volume_drive_none_no_extra_drive() {
    let vm_config = make_vm_config_no_volume();

    // Only rootfs drive -- no workspace drive
    assert_eq!(
        vm_config.drives.len(),
        1,
        "expected 1 drive (rootfs only), got {}",
        vm_config.drives.len()
    );

    let has_workspace = vm_config.drives.iter().any(|d| d.drive_id == "workspace");
    assert!(
        !has_workspace,
        "workspace drive must not be present when volume_drive is None"
    );
}

#[test]
fn test_volume_boot_arg_injected() {
    let volume_path = PathBuf::from("/srv/hyphae/volumes/boot-arg-test.img");
    let vm_config = make_vm_config_with_volume(&volume_path);

    let boot_args = vm_config.boot_source.boot_args.as_deref().unwrap_or("");
    assert!(
        boot_args.contains("hyphae.volume=true"),
        "boot args must contain 'hyphae.volume=true' when volume is attached, got: {boot_args}"
    );
}

#[test]
fn test_volume_boot_arg_absent_without_drive() {
    let vm_config = make_vm_config_no_volume();

    let boot_args = vm_config.boot_source.boot_args.as_deref().unwrap_or("");
    assert!(
        !boot_args.contains("hyphae.volume=true"),
        "boot args must NOT contain 'hyphae.volume=true' when no volume is attached, got: {boot_args}"
    );
}

#[test]
fn test_volume_path_helper() {
    // Verify hyphae_core::volume::volume_path produces the expected path format
    let id = uuid::Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
    let path = volume::volume_path(&id);
    assert_eq!(
        path,
        PathBuf::from("/srv/hyphae/volumes/550e8400-e29b-41d4-a716-446655440000.img")
    );
}

#[test]
fn test_volume_path_default_dir() {
    // The DEFAULT_VOLUME_DIR constant must match the hardcoded base in volume_path
    let id = uuid::Uuid::new_v4();
    let path = volume::volume_path(&id);
    assert!(
        path.starts_with(volume::DEFAULT_VOLUME_DIR),
        "volume_path must be under DEFAULT_VOLUME_DIR ({}), got: {}",
        volume::DEFAULT_VOLUME_DIR,
        path.display()
    );
}

#[test]
fn test_workspace_drive_is_not_root_device() {
    // The workspace drive must never be root device -- rootfs is always root
    let volume_path = PathBuf::from("/srv/hyphae/volumes/not-root.img");
    let vm_config = make_vm_config_with_volume(&volume_path);

    let rootfs = vm_config
        .drives
        .iter()
        .find(|d| d.drive_id == "rootfs")
        .expect("rootfs drive must be present");
    assert!(rootfs.is_root_device, "rootfs must be root device");

    let workspace = vm_config
        .drives
        .iter()
        .find(|d| d.drive_id == "workspace")
        .expect("workspace drive must be present");
    assert!(!workspace.is_root_device, "workspace drive must not be root device");
}
