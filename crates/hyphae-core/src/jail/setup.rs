//! Jail directory creation and per-invocation resource staging.
//!
//! The Firecracker jailer expects resources (kernel, rootfs, config) to be
//! present inside the jail's chroot directory before launch. This module
//! creates the jail directory structure and stages resources with distinct
//! semantics:
//!
//! - **Read-only resources** (kernel, config): hard-linked into the jail
//!   (copy fallback on cross-device errors). Guests cannot write them, so
//!   sharing the inode with the source is safe and avoids a copy.
//! - **Writable resources** (rootfs, volume images): cloned per invocation
//!   — reflink (`FICLONE`) where the filesystem supports it, full copy
//!   otherwise — and chowned to the jail UID/GID with owner write access.
//!   Hard-linking these would (a) leave root-owned `0644` files that the
//!   jailed Firecracker, running as the unprivileged jail user, cannot open
//!   read/write, and (b) let one invocation mutate the shared source image
//!   for every future invocation.

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

/// Stage kernel, rootfs, config, and optional volume into the jail root.
///
/// - Kernel and config are read-only for the guest: hard-linked into the
///   jail (copy fallback on cross-device errors), keeping the source file's
///   read-only mode.
/// - Writable disks (rootfs and any workspace volume) are
///   invocation-private: each launch gets its own clone (reflink where the
///   filesystem supports it, otherwise a full copy) owned by the jail
///   UID/GID with owner read/write access.
pub fn stage_resources_into_jail(
    jail_root: &Path,
    kernel_path: &Path,
    rootfs_path: &Path,
    config_path: Option<&Path>,
    volume_path: Option<&Path>,
    jail_uid: u32,
    jail_gid: u32,
) -> Result<(), JailError> {
    // Link kernel image (read-only for the guest).
    let kernel_dst = jail_root.join(
        kernel_path
            .file_name()
            .unwrap_or_default(),
    );
    link_or_copy(kernel_path, &kernel_dst)?;

    // Optionally link config file (read-only for the guest).
    if let Some(cfg) = config_path {
        let config_dst = jail_root.join(
            cfg.file_name()
                .unwrap_or_default(),
        );
        link_or_copy(cfg, &config_dst)?;
    }

    // Stage the rootfs (writable disk): invocation-private clone owned by
    // the jail user.
    let rootfs_dst = jail_root.join(
        rootfs_path
            .file_name()
            .unwrap_or_default(),
    );
    stage_writable_resource(rootfs_path, &rootfs_dst, jail_uid, jail_gid)?;

    // Stage any workspace volume (writable disk) with the same semantics.
    if let Some(volume) = volume_path {
        let volume_dst = jail_root.join(
            volume
                .file_name()
                .unwrap_or_default(),
        );
        stage_writable_resource(volume, &volume_dst, jail_uid, jail_gid)?;
    }

    debug!(
        jail_root = %jail_root.display(),
        "staged resources into jail"
    );

    Ok(())
}

/// Clone a writable disk into the jail root for this invocation.
///
/// The staged file is private to the invocation (reflink or full copy, never
/// a hard link) and is owned by the jail UID/GID with `0600` permissions so
/// the jailed Firecracker process can open it read/write. Failure to set
/// ownership is an error, not a silent fallback: a disk the jail user
/// cannot write would fail the launch anyway.
fn stage_writable_resource(
    src: &Path,
    dst: &Path,
    jail_uid: u32,
    jail_gid: u32,
) -> Result<(), JailError> {
    // Never reuse a stale staged file: a leftover from a previous launch may
    // contain guest mutations. Remove it so the clone below starts fresh.
    match std::fs::remove_file(dst) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(JailError::StageFailed {
                src: src.to_path_buf(),
                dst: dst.to_path_buf(),
                reason: format!("failed to remove stale staged file: {e}"),
            });
        }
    }

    // Reflink first (cheap, copy-on-write); fall back to a full copy when
    // the filesystem does not support FICLONE (e.g. ext4) or on non-Linux.
    if let Err(e) = reflink(src, dst) {
        trace!(
            src = %src.display(),
            dst = %dst.display(),
            error = %e,
            "reflink unavailable, falling back to full copy"
        );
        std::fs::copy(src, dst).map_err(|copy_err| JailError::StageFailed {
            src: src.to_path_buf(),
            dst: dst.to_path_buf(),
            reason: format!("reflink: {e}; copy fallback: {copy_err}"),
        })?;
    }

    // Owner-only access: the jailed Firecracker runs as the jail user.
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dst, std::fs::Permissions::from_mode(0o600)).map_err(|e| {
        JailError::StageFailed {
            src: src.to_path_buf(),
            dst: dst.to_path_buf(),
            reason: format!("failed to set 0600 permissions: {e}"),
        }
    })?;

    // Hand ownership to the jail user. The worker runs as root on the
    // deployment host, so failure here means the guest cannot write its
    // disks: fail closed rather than launching a VM that cannot boot.
    nix::unistd::chown(
        dst,
        Some(nix::unistd::Uid::from_raw(jail_uid)),
        Some(nix::unistd::Gid::from_raw(jail_gid)),
    )
    .map_err(|e| JailError::StageFailed {
        src: src.to_path_buf(),
        dst: dst.to_path_buf(),
        reason: format!("failed to chown to {jail_uid}:{jail_gid}: {e}"),
    })?;

    trace!(
        src = %src.display(),
        dst = %dst.display(),
        "staged invocation-private writable disk"
    );

    Ok(())
}

/// Reflink `src` to `dst` via `FICLONE` (Linux only).
#[cfg(target_os = "linux")]
fn reflink(src: &Path, dst: &Path) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;

    let src_file = std::fs::File::open(src)?;
    let dst_file = std::fs::File::create(dst)?;
    let ret =
        unsafe { libc::ioctl(dst_file.as_raw_fd(), libc::FICLONE, src_file.as_raw_fd()) };
    if ret < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Reflink is a Linux `FICLONE` operation; elsewhere it always falls back
/// to a full copy.
#[cfg(not(target_os = "linux"))]
fn reflink(_src: &Path, _dst: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "reflink requires Linux FICLONE",
    ))
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

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use super::*;

    // Tests run as an unprivileged user on dev machines, so the "jail user"
    // is the current user: chown to it succeeds, and read/write access can
    // be checked exactly like the jail user's on the deployment host.
    fn test_jail_uid() -> u32 {
        nix::unistd::getuid().as_raw()
    }

    fn test_jail_gid() -> u32 {
        nix::unistd::getgid().as_raw()
    }

    /// Create a source image with fixed content and read-only mode,
    /// mirroring published kernel/rootfs/volume images.
    fn make_source(dir: &Path, name: &str, content: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, content).expect("write source image");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444))
            .expect("set read-only mode on source");
        path
    }

    fn stage_all(
        jail_root: &Path,
        sources: &Sources,
        volume: Option<&Path>,
    ) -> Result<(), JailError> {
        stage_resources_into_jail(
            jail_root,
            &sources.kernel,
            &sources.rootfs,
            Some(&sources.config),
            volume,
            test_jail_uid(),
            test_jail_gid(),
        )
    }

    struct Sources {
        kernel: PathBuf,
        rootfs: PathBuf,
        config: PathBuf,
        volume: PathBuf,
        rootfs_content: Vec<u8>,
        volume_content: Vec<u8>,
    }

    fn make_sources(dir: &Path) -> Sources {
        Sources {
            kernel: make_source(dir, "vmlinux", b"kernel-image"),
            rootfs: make_source(dir, "rootfs.ext4", b"rootfs-image"),
            config: make_source(dir, "vmconfig.json", b"{\"drives\":[]}"),
            volume: make_source(dir, "workspace.ext4", b"volume-image"),
            rootfs_content: b"rootfs-image".to_vec(),
            volume_content: b"volume-image".to_vec(),
        }
    }

    // Requirement 1: writable disks are invocation-private.
    #[test]
    fn writable_disks_are_invocation_private() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let sources = make_sources(temp.path());

        let jail1 = temp.path().join("jail1");
        let jail2 = temp.path().join("jail2");
        for jail in [&jail1, &jail2] {
            std::fs::create_dir_all(jail).expect("create jail dir");
        }
        stage_all(&jail1, &sources, Some(&sources.volume)).expect("stage jail 1");
        stage_all(&jail2, &sources, Some(&sources.volume)).expect("stage jail 2");

        let staged1 = jail1.join("rootfs.ext4");
        let staged2 = jail2.join("rootfs.ext4");

        // Each invocation gets its own file, never a hard link to the source
        // or to another invocation's staged copy.
        assert_ne!(
            staged1.metadata().unwrap().ino(),
            sources.rootfs.metadata().unwrap().ino(),
            "staged rootfs must not share an inode with the source image"
        );
        assert_ne!(
            staged1.metadata().unwrap().ino(),
            staged2.metadata().unwrap().ino(),
            "two invocations must not share a staged rootfs inode"
        );

        // Guest mutations of one invocation's disk must not leak into the
        // source image or another invocation's disk.
        let mut f = OpenOptions::new()
            .append(true)
            .open(&staged1)
            .expect("open staged rootfs for writing");
        f.write_all(b"guest-mutation").expect("mutate staged rootfs");
        drop(f);

        assert_eq!(
            std::fs::read(&sources.rootfs).unwrap(),
            sources.rootfs_content,
            "source rootfs must remain unchanged"
        );
        assert_eq!(
            std::fs::read(&staged2).unwrap(),
            sources.rootfs_content,
            "other invocation's rootfs must remain unchanged"
        );
    }

    // Requirement 2: the jail user can open writable disks read/write.
    #[test]
    fn jail_user_can_open_writable_disks_read_write() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let sources = make_sources(temp.path());
        let jail = temp.path().join("jail");
        std::fs::create_dir_all(&jail).expect("create jail dir");
        stage_all(&jail, &sources, Some(&sources.volume)).expect("stage jail");

        for name in ["rootfs.ext4", "workspace.ext4"] {
            let staged = jail.join(name);
            let meta = staged.metadata().expect("stat staged disk");

            assert_eq!(meta.uid(), test_jail_uid(), "{name} must be jail-user owned");
            assert_eq!(meta.gid(), test_jail_gid(), "{name} must be jail-group owned");

            let mode = meta.permissions().mode();
            assert_ne!(mode & 0o200, 0, "{name} must be owner-writable");
            assert_eq!(mode & 0o777, 0o600, "{name} must have 0600 access bits");

            // The actual open the jailed Firecracker performs.
            let f = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&staged)
                .unwrap_or_else(|e| panic!("jail user must open {name} read/write: {e}"));
            drop(f);
        }
    }

    // Requirement 3: source images remain unchanged.
    #[test]
    fn source_images_remain_unchanged() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let sources = make_sources(temp.path());
        let jail = temp.path().join("jail");
        std::fs::create_dir_all(&jail).expect("create jail dir");
        stage_all(&jail, &sources, Some(&sources.volume)).expect("stage jail");

        // Mutate every writable staged disk.
        for name in ["rootfs.ext4", "workspace.ext4"] {
            let mut f = OpenOptions::new()
                .append(true)
                .open(jail.join(name))
                .expect("open staged disk for writing");
            f.write_all(b"mutation").expect("mutate staged disk");
        }

        assert_eq!(
            std::fs::read(&sources.rootfs).unwrap(),
            sources.rootfs_content
        );
        assert_eq!(
            std::fs::read(&sources.volume).unwrap(),
            sources.volume_content
        );
    }

    // Requirement 4: kernel and config remain non-writable hard links.
    #[test]
    fn kernel_and_config_remain_read_only() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let sources = make_sources(temp.path());
        let jail = temp.path().join("jail");
        std::fs::create_dir_all(&jail).expect("create jail dir");
        stage_all(&jail, &sources, None).expect("stage jail");

        for (name, source) in [("vmlinux", &sources.kernel), ("vmconfig.json", &sources.config)] {
            let staged = jail.join(name);
            let meta = staged.metadata().expect("stat staged read-only resource");

            assert_eq!(
                meta.ino(),
                source.metadata().unwrap().ino(),
                "{name} should stay a hard link to the source"
            );
            assert_eq!(
                meta.permissions().mode() & 0o777,
                0o444,
                "{name} must remain non-writable"
            );
            assert!(OpenOptions::new()
                .write(true)
                .open(&staged)
                .is_err(), "{name} must not be openable for writing");
        }
    }

    // A stale staged disk from a previous (unclean) launch must be replaced,
    // never reused: it may contain guest mutations.
    #[test]
    fn stale_staged_disk_is_replaced() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let sources = make_sources(temp.path());
        let jail = temp.path().join("jail");
        std::fs::create_dir_all(&jail).expect("create jail dir");

        let stale = jail.join("rootfs.ext4");
        std::fs::write(&stale, b"stale mutated content").expect("write stale staged disk");

        stage_all(&jail, &sources, None).expect("stage jail");

        assert_eq!(
            std::fs::read(&stale).unwrap(),
            sources.rootfs_content,
            "stale staged disk must be replaced with a fresh clone"
        );
    }

    // Staging failures are errors, never a silent fallback to hard links.
    #[test]
    fn missing_writable_source_is_an_error() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let sources = make_sources(temp.path());
        let jail = temp.path().join("jail");
        std::fs::create_dir_all(&jail).expect("create jail dir");

        let result = stage_resources_into_jail(
            &jail,
            &sources.kernel,
            &temp.path().join("missing-rootfs.ext4"),
            Some(&sources.config),
            None,
            test_jail_uid(),
            test_jail_gid(),
        );

        assert!(result.is_err(), "missing rootfs source must fail staging");
    }
}
