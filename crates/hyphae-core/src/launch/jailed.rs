//! Jailed launch path for Firecracker VMs.
//!
//! Invokes the Firecracker jailer with chroot isolation, cgroup resource
//! limits, and privilege dropping. Resources are staged into the jail
//! directory before launch: kernel and config are hard-linked (read-only),
//! while writable disks (rootfs, workspace volume) are invocation-private
//! clones owned by the jail user.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use hyphae_errors::{HyphaeError, JailError, ProcessError};
use tracing::{debug, info, warn};

use crate::config::{DriveConfig, VmConfig};
use crate::jail::cgroup::CgroupConfig;
use crate::jail::{
    create_cgroup, create_jail_directory, stage_resources_into_jail, remove_cgroup,
    validate_jail_user, wait_for_pid_file, JailConfig,
};
use crate::mmds;
use crate::net::guest_boot_ip_arg;

use super::{LaunchConfig, LaunchResult};

/// Default boot arguments for the guest kernel.
const DEFAULT_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1";

/// API socket filename inside the jail (relative to jail root).
const JAIL_API_SOCK: &str = "run/firecracker.socket";

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct JailedVsockPaths {
    /// Path written to the Firecracker config, relative to the chroot.
    pub config_path: PathBuf,
    /// Path used by host processes to connect to the socket.
    pub host_path: PathBuf,
}

pub(crate) fn jailed_vsock_paths(jail_root: &Path, uds_path: &Path) -> JailedVsockPaths {
    let config_path = uds_path
        .file_name()
        .map(PathBuf::from)
        .unwrap_or_default();
    let host_path = jail_root.join(&config_path);

    JailedVsockPaths {
        config_path,
        host_path,
    }
}

/// Launch Firecracker through the jailer with full isolation.
///
/// Steps:
/// 1. Validate the hyphae system user
/// 2. Create a cgroup with CPU/memory limits
/// 3. Build VmConfig with relative paths (inside chroot)
/// 4. Write config to a temporary file
/// 5. Create jail directory and stage resources (read-only links for
///    kernel/config; invocation-private jail-user-owned clones for writable
///    disks)
/// 6. Build and spawn the jailer command
/// 7. Wait for Firecracker to write its PID file
/// 8. Return LaunchResult
///
/// On failure after cgroup creation, cleans up the cgroup and jail directory.
pub async fn launch_jailed(
    config: LaunchConfig,
    jail_config: JailConfig,
) -> Result<LaunchResult, HyphaeError> {
    // Step 1: Validate the hyphae system user.
    let user = validate_jail_user()?;
    debug!(
        uid = user.uid,
        gid = user.gid,
        name = %user.name,
        "validated jail user"
    );

    // Step 2: Create cgroup with CPU/memory limits.
    let cgroup_config = CgroupConfig {
        vcpus: config.vcpus,
        memory_mib: config.memory_mib,
        vm_id: config.vm_id.clone(),
    };

    let cgroup_dir = create_cgroup(&cgroup_config)?;
    debug!(cgroup = %cgroup_dir.display(), "created cgroup");

    // Everything from here on cleans up cgroup + jail on failure.
    match launch_jailed_inner(&config, &jail_config).await {
        Ok(result) => Ok(result),
        Err(e) => {
            warn!(
                vm_id = %config.vm_id,
                error = %e,
                "jailed launch failed, cleaning up"
            );
            cleanup_on_failure(&config.vm_id, &jail_config);
            Err(e)
        }
    }
}

/// Inner implementation that can fail; caller handles cleanup.
async fn launch_jailed_inner(
    config: &LaunchConfig,
    jail_config: &JailConfig,
) -> Result<LaunchResult, HyphaeError> {
    let jail_root = jail_config.jail_root();

    // Step 3: Build VmConfig with RELATIVE paths (inside chroot).
    // Inside the jail, the kernel and rootfs are at the root of the chroot.
    let kernel_filename = config
        .kernel_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    let rootfs_filename = config
        .rootfs_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();

    let mut vm_config = VmConfig::new(
        kernel_filename.as_ref(),
        rootfs_filename.as_ref(),
        config.vcpus,
        config.memory_mib,
    )
    .with_boot_args(DEFAULT_BOOT_ARGS);

    // Wire in network interface if present.
    if let Some(ref net) = config.network {
        vm_config = vm_config
            .with_network_interface("eth0", &net.tap_name, &net.guest_mac)
            .with_additional_boot_args(guest_boot_ip_arg(net.guest_ip, net.host_ip));
    }

    // Wire in vsock device if present.
    // IMPORTANT: In jailed mode, Firecracker runs inside a chroot.
    // The UDS path must be jail-relative (filename only), not host-absolute.
    // The host-side vsock proxy uses the absolute path; Firecracker uses the relative one.
    let vsock_host_path = if let Some(ref vsock_cfg) = config.vsock {
        let paths = jailed_vsock_paths(&jail_root, Path::new(&vsock_cfg.uds_path));
        // Remove any stale UDS file from the host-visible jail location —
        // Firecracker needs to create this socket itself and will fail with
        // EADDRINUSE if the file already exists.
        let _ = std::fs::remove_file(&paths.host_path);
        vm_config = vm_config.with_vsock(
            vsock_cfg.guest_cid,
            paths.config_path.to_string_lossy().as_ref(),
        );
        Some(paths.host_path)
    } else {
        None
    };

    // Append any extra boot arguments.
    if let Some(ref extra) = config.extra_boot_args {
        vm_config = vm_config.with_additional_boot_args(extra);
    }

    // Inject runtime env vars via kernel boot args, using the exact same
    // encoded construction as the direct path (`launch/direct.rs`): both
    // paths call the shared `apply_env_boot_args` helper so the
    // `hyphae.env.KEY=VALUE` arguments — percent-encoding `=`, spaces,
    // newlines, and `%` — are byte-identical in direct and jailed mode.
    // hyphae-init decodes them from /proc/cmdline at guest boot and applies
    // them to the agent process; previously only the direct path injected
    // these, so jailed runs (the production path) never saw manifest [env]
    // vars like MODEL_BASE_URL / MODEL_NAME.
    vm_config = super::apply_env_boot_args(vm_config, config.env_vars.as_ref());

    // Wire in volume drive if present (Phase 17 populates LaunchConfig.volume_drive).
    // In jailed mode, the drive path must be relative (just the filename) because
    // Firecracker runs inside a chroot. The actual file will be staged as an
    // invocation-private clone in the jail root in Step 5.
    let volume_filename_owned: Option<String> = config.volume_drive.as_ref().map(|p| {
        p.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    });
    if let Some(ref volume_filename) = volume_filename_owned {
        let mut drive = DriveConfig::new("workspace", volume_filename.as_str(), false);
        drive.cache_type = Some("Writeback".into());
        vm_config = vm_config.with_drive(drive);
        vm_config = vm_config.with_additional_boot_args("hyphae.volume=true");
    }

    // Wire in MMDS config when both network and mmds_payload are present.
    // MMDS requires a network interface to be accessible from the guest.
    if config.network.is_some() && config.mmds_payload.is_some() {
        vm_config = vm_config.with_mmds_config(
            "V2",
            vec!["eth0".to_string()],
            None,  // use default 169.254.169.254
        );
    }

    // Step 4: Write config to a temporary file.
    let config_json = vm_config.to_json().map_err(HyphaeError::from)?;

    let config_tmp_path = std::env::temp_dir().join(format!(
        "hyphae-{}-jail-config.json",
        config.vm_id
    ));
    std::fs::write(&config_tmp_path, &config_json).map_err(|e| {
        JailError::DirectoryCreation {
            path: config_tmp_path.clone(),
            reason: format!("failed to write temporary config: {e}"),
        }
    })?;

    // Step 5: Create the jail directory and stage resources.
    //
    // Kernel and config are hard-linked (read-only for the guest). The
    // rootfs and any workspace volume are writable disks: each launch gets
    // an invocation-private clone (reflink or copy) owned by the jail
    // UID/GID, so jailed Firecracker can open them read/write without ever
    // exposing the shared source images to guest mutations.
    create_jail_directory(&jail_root)?;
    stage_resources_into_jail(
        &jail_root,
        &config.kernel_path,
        &config.rootfs_path,
        Some(&config_tmp_path),
        config.volume_drive.as_deref(),
        jail_config.uid,
        jail_config.gid,
    )?;

    // Create the `run/` subdirectory for the API socket inside the jail.
    let run_dir = jail_root.join("run");
    std::fs::create_dir_all(&run_dir).map_err(|e| JailError::DirectoryCreation {
        path: run_dir.clone(),
        reason: e.to_string(),
    })?;

    // Step 6: Build and spawn the jailer command.
    // The jailer expects --config-file to be passed after -- as a Firecracker arg.
    // The config file path is relative to the jail root.
    let config_filename = config_tmp_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();

    let mut cmd = jail_config.build_command(JAIL_API_SOCK);
    cmd.arg("--config-file").arg(config_filename.as_ref());
    cmd.stdin(Stdio::null());

    // Create a console log file inside the jail root to capture stdout/stderr.
    let log_file_path = jail_root.join("console.log");
    let log_file = std::fs::File::create(&log_file_path).map_err(|e| {
        ProcessError::SpawnFailed(format!(
            "failed to create log file {}: {}",
            log_file_path.display(),
            e
        ))
    })?;
    let log_file_dup = log_file.try_clone().map_err(|e| {
        ProcessError::SpawnFailed(format!(
            "failed to duplicate log file handle: {}",
            e
        ))
    })?;
    cmd.stdout(Stdio::from(log_file));
    cmd.stderr(Stdio::from(log_file_dup));

    let child = cmd.spawn().map_err(|e| {
        JailError::JailerSpawnFailed(format!(
            "failed to spawn jailer {}: {e}",
            jail_config.jailer_bin.display()
        ))
    })?;

    debug!(
        jailer_pid = child.id(),
        vm_id = %config.vm_id,
        "jailer process spawned"
    );

    // The jailer with --daemonize exits quickly after forking Firecracker.
    // We need to wait for the PID file that Firecracker writes.
    // Step 7: Wait for Firecracker PID file.
    let pid_file_path = jail_root.join("firecracker.pid");
    let pid = wait_for_pid_file(&pid_file_path).await?;

    // Step 8: Push MMDS metadata via the jail's API socket if payload is present.
    let api_socket_path: PathBuf = jail_root.join(JAIL_API_SOCK);
    if let Some(ref payload) = config.mmds_payload {
        if config.network.is_some() {
            // Wait for the API socket to become available (up to 5 seconds).
            let wait_result = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if api_socket_path.exists() {
                        match tokio::net::UnixStream::connect(&api_socket_path).await {
                            Ok(_) => return,
                            Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
                        }
                    } else {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }
            })
            .await;

            if wait_result.is_err() {
                return Err(ProcessError::SpawnFailed(
                    "timed out waiting for jailed API socket before MMDS push".to_string(),
                )
                .into());
            }

            // Configure MMDS on the API socket first, then push the payload.
            let mmds_config = mmds::MmdsConfig::default();
            mmds::configure_mmds(&api_socket_path, &mmds_config).await?;
            mmds::set_metadata(&api_socket_path, payload).await?;
        }
    }

    info!(
        pid,
        vm_id = %config.vm_id,
        jail_root = %jail_root.display(),
        jailed = true,
        "launched VM in jailed mode"
    );

    // Clean up temporary config file (best-effort).
    let _ = std::fs::remove_file(&config_tmp_path);

    Ok(LaunchResult {
        pid,
        vm_id: config.vm_id.clone(),
        jail_root: Some(jail_root),
        jailed: true,
        log_file: Some(log_file_path.to_string_lossy().to_string()),
        api_socket_path: Some(api_socket_path),
        vsock_host_path,
    })
}

/// Clean up cgroup and jail directory after a failed jailed launch.
fn cleanup_on_failure(vm_id: &str, jail_config: &JailConfig) {
    // Remove cgroup (best-effort).
    if let Err(e) = remove_cgroup(vm_id) {
        warn!(
            vm_id,
            error = %e,
            "failed to remove cgroup during cleanup"
        );
    }

    // Remove jail directory (best-effort).
    if let Err(e) = jail_config.cleanup() {
        warn!(
            vm_id,
            error = %e,
            "failed to clean up jail directory during cleanup"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jailed_vsock_paths_use_filename_in_config_and_jail_path_on_host() {
        let jail_root = Path::new("/srv/jailer/firecracker/vm/root");
        let configured_path = Path::new("/some/runtime/vm/v.sock");

        let paths = jailed_vsock_paths(jail_root, configured_path);

        assert_eq!(paths.config_path, PathBuf::from("v.sock"));
        assert_eq!(paths.host_path, jail_root.join("v.sock"));
    }
}
