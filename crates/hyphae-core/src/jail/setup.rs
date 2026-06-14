//! Jail directory creation and resource hard-linking with copy fallback.
//!
//! The Firecracker jailer expects resources (kernel, rootfs, config) to be
//! present inside the jail's chroot directory before launch. This module
//! creates the jail directory structure and hard-links (or copies) resources
//! into place.

use std::path::{Path, PathBuf};

use hyphae_errors::JailError;
use tracing::{debug, trace, warn};

/// Compute the jail root directory path.
///
/// The jailer creates a directory structure:
/// `{chroot_base}/{exec_name}/{vm_id}/root/`
///
/// where `exec_name` is the filename of the Firecracker binary.
pub fn jail_root_dir(chroot_base: &Path, firecracker_bin: &Path, vm_id: &str) -> PathBuf {
    let exec_name = firecracker_bin
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    chroot_base.join(exec_name.as_ref()).join(vm_id).join("root")
}

/// Create the jail root directory and any necessary parent directories.
pub fn create_jail_directory(jail_root: &Path) -> Result<(), JailError> {
    std::fs::create_dir_all(jail_root).map_err(|e| JailError::DirectoryCreation {
        path: jail_root.to_path_buf(),
        reason: e.to_string(),
    })?;

    debug!(jail_root = %jail_root.display(), "created jail directory");
    Ok(())
}

/// Hard-link kernel, rootfs, and optionally config into the jail root.
///
/// Falls back to copy if hard-linking fails due to cross-device errors
/// (e.g., kernel on a different filesystem than the jail directory).
pub fn link_resources_into_jail(
    jail_root: &Path,
    kernel_path: &Path,
    rootfs_path: &Path,
    config_path: Option<&Path>,
) -> Result<(), JailError> {
    // Link kernel image.
    let kernel_dst = jail_root.join(
        kernel_path
            .file_name()
            .unwrap_or_default(),
    );
    link_or_copy(kernel_path, &kernel_dst)?;

    // Link rootfs image.
    let rootfs_dst = jail_root.join(
        rootfs_path
            .file_name()
            .unwrap_or_default(),
    );
    link_or_copy(rootfs_path, &rootfs_dst)?;

    // Optionally link config file.
    if let Some(cfg) = config_path {
        let config_dst = jail_root.join(
            cfg.file_name()
                .unwrap_or_default(),
        );
        link_or_copy(cfg, &config_dst)?;
    }

    debug!(
        jail_root = %jail_root.display(),
        "linked resources into jail"
    );

    Ok(())
}

/// Hard-link a file, falling back to copy on cross-device errors.
fn link_or_copy(src: &Path, dst: &Path) -> Result<(), JailError> {
    // Skip if destination already exists (idempotent).
    if dst.exists() {
        trace!(
            src = %src.display(),
            dst = %dst.display(),
            "destination already exists, skipping link"
        );
        return Ok(());
    }

    match std::fs::hard_link(src, dst) {
        Ok(()) => {
            trace!(
                src = %src.display(),
                dst = %dst.display(),
                "hard-linked resource into jail"
            );
            Ok(())
        }
        Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
            // Cross-device link: fall back to copy.
            warn!(
                src = %src.display(),
                dst = %dst.display(),
                "cross-device link, falling back to copy"
            );
            std::fs::copy(src, dst).map_err(|copy_err| JailError::HardLinkFailed {
                src: src.to_path_buf(),
                dst: dst.to_path_buf(),
                reason: format!("hard_link: {e}, copy fallback: {copy_err}"),
            })?;
            Ok(())
        }
        Err(e) => {
            // Non-cross-device error: try copy as well before failing.
            warn!(
                src = %src.display(),
                dst = %dst.display(),
                error = %e,
                "hard-link failed, attempting copy fallback"
            );
            std::fs::copy(src, dst).map_err(|copy_err| JailError::HardLinkFailed {
                src: src.to_path_buf(),
                dst: dst.to_path_buf(),
                reason: format!("hard_link: {e}, copy fallback: {copy_err}"),
            })?;
            Ok(())
        }
    }
}

/// Remove the entire jail directory tree for a given VM.
///
/// Removes `{chroot_base}/{exec_name}/{vm_id}/` and everything beneath it.
pub fn cleanup_jail(
    chroot_base: &Path,
    firecracker_bin: &Path,
    vm_id: &str,
) -> Result<(), JailError> {
    let exec_name = firecracker_bin
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    let vm_dir = chroot_base.join(exec_name.as_ref()).join(vm_id);

    if !vm_dir.exists() {
        return Ok(());
    }

    std::fs::remove_dir_all(&vm_dir).map_err(|e| JailError::CleanupFailed {
        path: vm_dir.clone(),
        source: e,
    })?;

    debug!(path = %vm_dir.display(), "cleaned up jail directory");
    Ok(())
}
