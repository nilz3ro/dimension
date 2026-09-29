//! Unified launch path for Firecracker VMs.
//!
//! Provides a single [`launch`] entry point that dispatches to either a
//! direct (non-jailed) or jailed launch path based on [`LaunchMode`].
//! Network configuration is wired into the Firecracker JSON config when
//! present.

pub mod direct;
pub mod jailed;

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::path::PathBuf;

use hyphae_errors::HyphaeError;
use tracing::warn;

use crate::config::VsockConfig;
use crate::jail::JailConfig;

/// Selects whether the VM runs with or without jailer isolation.
pub enum LaunchMode {
    /// Launch Firecracker directly without the jailer.
    /// Logs a warning about missing sandbox isolation.
    Direct,
    /// Launch through the Firecracker jailer with chroot and cgroup isolation.
    Jailed(JailConfig),
}

/// Network configuration for a VM launch.
///
/// Drives TAP device name, MAC address, and guest IP configuration
/// in the Firecracker config and kernel boot arguments.
pub struct NetworkConfig {
    /// TAP device name on the host (e.g. `hyphae-tap0`).
    pub tap_name: String,
    /// Host-side IP address (gateway for the guest).
    pub host_ip: Ipv4Addr,
    /// Guest-side IP address.
    pub guest_ip: Ipv4Addr,
    /// Guest MAC address (e.g. `AA:FC:00:00:00:00`).
    pub guest_mac: String,
    /// Whether NAT rules were configured for internet access.
    pub enable_nat: bool,
}

/// Unified configuration for launching a Firecracker VM.
///
/// Contains all parameters needed for both direct and jailed launch paths.
/// The [`LaunchMode`] determines which path is taken and is stored as a
/// field so callers construct a single config struct.
pub struct LaunchConfig {
    /// Direct or jailed launch mode.
    pub mode: LaunchMode,
    /// Path to the Firecracker binary.
    pub firecracker_bin: PathBuf,
    /// Path to the kernel image.
    pub kernel_path: PathBuf,
    /// Path to the root filesystem image.
    pub rootfs_path: PathBuf,
    /// Number of vCPUs.
    pub vcpus: u8,
    /// Memory allocation in MiB.
    pub memory_mib: u64,
    /// Optional network configuration.
    pub network: Option<NetworkConfig>,
    /// VM identifier (used for runtime directory, cgroup, jail).
    pub vm_id: String,
    /// Additional kernel boot arguments (appended to defaults).
    pub extra_boot_args: Option<String>,
    /// Optional vsock device configuration (guest CID + UDS path).
    pub vsock: Option<VsockConfig>,
    /// Optional runtime environment variables to inject into the guest.
    ///
    /// These are passed via kernel boot arguments as `hyphae.env.KEY=VALUE`
    /// and are read by `hyphae-init` from `/proc/cmdline` at boot time.
    /// Runtime env vars (manifest `[env]`) should be set here — they override
    /// build-time env vars from `/etc/hyphae/env` when keys collide.
    pub env_vars: Option<HashMap<String, String>>,

    /// Optional path to a persistent volume image to attach as `/dev/vdb`.
    ///
    /// When set, the launch path:
    /// 1. Adds a secondary `DriveConfig` with `drive_id: "workspace"` and `cache_type: "Writeback"`
    /// 2. Appends `hyphae.volume=true` to the kernel boot arguments
    ///
    /// `hyphae-init` reads this flag and mounts `/dev/vdb` at `/workspace` as ext4
    /// before starting the agent. Populated by Phase 17 (Persistent Volumes) when
    /// the session has an attached volume; `None` here means no volume is attached.
    pub volume_drive: Option<PathBuf>,

    /// Optional MMDS payload to push to the VM after Firecracker starts.
    ///
    /// When set (and `network` is also `Some`), the launch path:
    /// 1. Adds an `mmds-config` section to the Firecracker config JSON
    /// 2. After spawning Firecracker, waits for the API socket and pushes
    ///    this payload via `PUT /mmds`
    ///
    /// The guest reads this payload from `http://169.254.169.254/` via MMDS.
    pub mmds_payload: Option<serde_json::Value>,
}

/// Result of a successful VM launch.
pub struct LaunchResult {
    /// Firecracker process PID.
    pub pid: u32,
    /// VM identifier.
    pub vm_id: String,
    /// Jail root directory (only set for jailed launches).
    pub jail_root: Option<PathBuf>,
    /// Whether the VM is running inside a jailer.
    pub jailed: bool,
    /// Path to the console log file capturing Firecracker stdout/stderr.
    pub log_file: Option<String>,
    /// Path to the Firecracker API socket (for post-launch operations).
    pub api_socket_path: Option<PathBuf>,
    /// Actual host-side path where the vsock Unix socket will exist.
    pub vsock_host_path: Option<PathBuf>,
}

/// Launch a Firecracker VM using the unified launch API.
///
/// Dispatches to [`direct::launch_direct`] or [`jailed::launch_jailed`]
/// based on the [`LaunchMode`] in the config. For direct mode, logs a
/// warning about missing sandbox isolation.
pub async fn launch(mut config: LaunchConfig) -> Result<LaunchResult, HyphaeError> {
    // Extract mode so we can pass config by value to the sub-functions.
    // Replace with Direct as a dummy -- it won't be read again.
    let mode = std::mem::replace(&mut config.mode, LaunchMode::Direct);

    match mode {
        LaunchMode::Direct => {
            warn!("VM running without sandbox isolation (--no-jail)");
            direct::launch_direct(config).await
        }
        LaunchMode::Jailed(jail_config) => jailed::launch_jailed(config, jail_config).await,
    }
}
