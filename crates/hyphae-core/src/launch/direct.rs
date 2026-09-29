//! Direct (non-jailed) launch path for Firecracker VMs.
//!
//! Spawns Firecracker directly without the jailer. This is intended
//! for development and testing; production deployments should use the
//! jailed path for sandbox isolation.

use std::path::{Path, PathBuf};

use hyphae_errors::HyphaeError;
use tracing::info;

use crate::config::{DriveConfig, VmConfig};
use crate::net::guest_boot_ip_arg;
use crate::process::{self, SpawnConfig};

use super::{LaunchConfig, LaunchResult};

/// Default boot arguments for the guest kernel.
const DEFAULT_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1";

pub(crate) fn direct_vsock_host_path(uds_path: &Path) -> PathBuf {
    uds_path.to_path_buf()
}

/// Launch Firecracker directly without the jailer.
///
/// Builds a VmConfig from the LaunchConfig parameters, writes it to a
/// temporary config file, and spawns Firecracker via the Phase 3 process
/// module. Network interfaces and boot arguments are wired in when present.
///
/// Uses ABSOLUTE paths in the Firecracker config (no chroot).
pub async fn launch_direct(config: LaunchConfig) -> Result<LaunchResult, HyphaeError> {
    let vsock_host_path = config
        .vsock
        .as_ref()
        .map(|vsock| direct_vsock_host_path(Path::new(&vsock.uds_path)));

    // Build VmConfig with absolute paths.
    let mut vm_config = VmConfig::new(
        config.kernel_path.to_string_lossy().as_ref(),
        config.rootfs_path.to_string_lossy().as_ref(),
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
    if let Some(ref vsock_cfg) = config.vsock {
        // Remove any stale UDS file — Firecracker needs to create this socket
        // itself and will fail with EADDRINUSE if the file already exists.
        let _ = std::fs::remove_file(&vsock_cfg.uds_path);
        vm_config = vm_config.with_vsock(vsock_cfg.guest_cid, &vsock_cfg.uds_path);
    }

    // Append any extra boot arguments.
    if let Some(ref extra) = config.extra_boot_args {
        vm_config = vm_config.with_additional_boot_args(extra);
    }

    // Inject runtime env vars via kernel boot args.
    //
    // We use the kernel command line as the injection mechanism because the
    // direct launch path writes a JSON config file and spawns Firecracker
    // without using the pre-boot API socket. MMDS requires pre-boot API calls
    // (PUT /mmds/config, PUT /mmds) which are not possible in this flow.
    //
    // Encoding: `hyphae.env.KEY=VALUE` per var, percent-encoding `=` and
    // spaces in values to survive kernel cmdline parsing. hyphae-init reads
    // /proc/cmdline and strips the prefix to reconstruct vars.
    if let Some(ref env_vars) = config.env_vars {
        if !env_vars.is_empty() {
            let boot_args: Vec<String> = env_vars
                .iter()
                .map(|(k, v)| {
                    // Percent-encode characters that would break kernel cmdline parsing:
                    // spaces → %20, equals → %3D, newlines → %0A
                    let encoded_key = k.replace('%', "%25").replace(' ', "%20").replace('=', "%3D").replace('\n', "%0A");
                    let encoded_val = v.replace('%', "%25").replace(' ', "%20").replace('\n', "%0A");
                    format!("hyphae.env.{}={}", encoded_key, encoded_val)
                })
                .collect();
            vm_config = vm_config.with_additional_boot_args(boot_args.join(" "));
        }
    }

    // Attach secondary volume drive if present (Phase 17 populates this).
    // The drive appears as /dev/vdb inside the guest; hyphae-init mounts it
    // at /workspace when the hyphae.volume=true boot arg is also present.
    if let Some(ref volume_path) = config.volume_drive {
        let mut drive = DriveConfig::new(
            "workspace",
            volume_path.to_string_lossy().as_ref(),
            false,
        );
        drive.cache_type = Some("Writeback".into());
        vm_config = vm_config.with_drive(drive);
        vm_config = vm_config.with_additional_boot_args("hyphae.volume=true");
    }

    // Serialize the config JSON.
    let config_json = vm_config.to_json().map_err(HyphaeError::from)?;

    // Write config to a temp file. DO NOT delete this file after spawn --
    // Firecracker reads it asynchronously after fork+exec, and deleting it
    // before Firecracker opens it causes a race condition.
    // The file is small (< 4 KB) and cleaned up on next boot or VM stop.
    let config_file_path = std::env::temp_dir().join(format!(
        "hyphae-{}-config.json",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::write(&config_file_path, &config_json).map_err(|e| {
        hyphae_errors::ProcessError::SpawnFailed(format!(
            "failed to write VM config to {}: {e}",
            config_file_path.display()
        ))
    })?;

    // Spawn Firecracker using the existing process module.
    let spawn_config = SpawnConfig {
        firecracker_bin: config.firecracker_bin,
        config_file: Some(config_file_path),
        runtime_base_dir: None,
        log_file: None,
    };

    let vm_process = process::spawn(&spawn_config).await?;
    let pid = vm_process.pid();
    let vm_id = vm_process.vm_id().simple().to_string();
    let api_socket_path: PathBuf = vm_process.api_socket_path().to_path_buf();

    // Detach so the VmProcess Drop doesn't kill the process.
    // The caller manages lifecycle through the returned LaunchResult.
    let mut vm_process = vm_process;
    vm_process.detach();

    info!(
        pid,
        vm_id = %vm_id,
        jailed = false,
        "launched VM in direct (non-jailed) mode"
    );

    let log_file = vm_process
        .log_file()
        .map(|p| p.to_string_lossy().to_string());

    Ok(LaunchResult {
        pid,
        vm_id,
        jail_root: None,
        jailed: false,
        log_file,
        api_socket_path: Some(api_socket_path),
        vsock_host_path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_vsock_host_path_preserves_configured_absolute_path() {
        let configured_path = Path::new("/some/runtime/vm/v.sock");

        assert_eq!(direct_vsock_host_path(configured_path), configured_path);
    }
}
