//! Root-gated integration test for jailed launch failure cleanup.
//!
//! Proves that a jailed launch which fails *after* the cgroup, jail
//! directory, and staged resources exist leaves no jail directory and no
//! cgroup behind. The failure is injected at the jailer spawn step, so the
//! full staging path (read-only links for kernel/config, invocation-private
//! jail-user-owned clones for writable disks) runs before the failure.
//!
//! This test requires Linux, root, and a writable cgroup v2 hierarchy —
//! i.e. it runs for real on the deployment host (`sudo cargo test`), and
//! self-skips everywhere else. The runtime-directory half of the guarantee
//! is covered by the worker `teardown_vm` tests.

#![cfg(target_os = "linux")]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use hyphae_core::jail::JailConfig;
use hyphae_core::launch::{launch, LaunchConfig, LaunchMode};

#[tokio::test]
async fn failed_jailed_launch_removes_jail_and_cgroup_state() {
    let is_root = nix::unistd::getuid().as_raw() == 0;
    let cgroup2 = Path::new("/sys/fs/cgroup/cgroup.controllers").exists();

    if !(is_root && cgroup2) {
        eprintln!(
            "skipping: requires root and cgroup v2 (is_root={is_root}, cgroup2={cgroup2}); \
             run with `sudo cargo test` on the deployment host"
        );
        return;
    }

    let temp = tempfile::tempdir().expect("create temp dir");
    let chroot_base = temp.path().join("jails");
    std::fs::create_dir_all(&chroot_base).expect("create chroot base");

    // Garbage-content images: staging must still succeed (clone + chown),
    // proving the staging path itself is not what fails.
    let kernel = write_image(temp.path(), "vmlinux", b"not-a-kernel");
    let rootfs = write_image(temp.path(), "rootfs.ext4", b"not-a-rootfs");
    let volume = write_image(temp.path(), "workspace.ext4", b"not-a-volume");

    let vm_id = format!(
        "failtest-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );

    let mut jail_config = JailConfig::new(
        // Deliberately nonexistent jailer: the spawn step fails after the
        // cgroup, jail directory, and staged resources already exist.
        temp.path().join("nonexistent-jailer"),
        PathBuf::from("firecracker"),
        vm_id.clone(),
        nix::unistd::getuid().as_raw(),
        nix::unistd::getgid().as_raw(),
    );
    jail_config.chroot_base_dir = chroot_base.clone();

    let launch_config = LaunchConfig {
        mode: LaunchMode::Jailed(jail_config),
        firecracker_bin: PathBuf::from("firecracker"),
        kernel_path: kernel,
        rootfs_path: rootfs,
        volume_drive: Some(volume),
        vcpus: 1,
        memory_mib: 128,
        vm_id: vm_id.clone(),
        network: None,
        extra_boot_args: None,
        vsock: None,
        env_vars: None,
        mmds_payload: None,
    };

    let error = match launch(launch_config).await {
        Err(e) => e,
        Ok(_) => panic!("launch must fail with a nonexistent jailer binary"),
    };
    eprintln!("launch failed as expected: {error}");

    // Complete state removal: no jail directory, no cgroup.
    let vm_dir = chroot_base.join("firecracker").join(&vm_id);
    assert!(
        !vm_dir.exists(),
        "jail directory must be removed after launch failure: {}",
        vm_dir.display()
    );
    let cgroup_dir = Path::new("/sys/fs/cgroup/hyphae").join(&vm_id);
    assert!(
        !cgroup_dir.exists(),
        "cgroup must be removed after launch failure: {}",
        cgroup_dir.display()
    );
}

fn write_image(dir: &Path, name: &str, content: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, content).expect("write image");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444))
        .expect("set read-only mode");
    path
}
