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

use crate::config::{VmConfig, VsockConfig};
use crate::jail::JailConfig;

/// Percent-encode one component (key or value) of a `hyphae.env.*` boot arg.
///
/// Encodes `%` → `%25`, space → `%20`, newline → `%0A`. Keys additionally
/// encode `=` → `%3D` so the first `=` in the token always separates key
/// from value.
fn encode_env_component(s: &str, is_key: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '%' => out.push_str("%25"),
            ' ' => out.push_str("%20"),
            '\n' => out.push_str("%0A"),
            '=' if is_key => out.push_str("%3D"),
            c => out.push(c),
        }
    }
    out
}

/// Encode runtime env vars as `hyphae.env.KEY=VALUE` kernel boot arguments.
///
/// We use the kernel command line as the injection mechanism because the
/// launch paths spawn Firecracker without pre-boot API-socket MMDS setup.
/// Encoding: `hyphae.env.KEY=VALUE` per var, percent-encoding `=`, spaces,
/// newlines, and `%` to survive kernel cmdline parsing. `hyphae-init` reads
/// `/proc/cmdline` and strips the prefix to reconstruct vars (see
/// `hyphae-init/src/main.rs`).
///
/// Keys are emitted in sorted order so the encoding is deterministic.
/// Shared by both the direct and jailed launch paths — they must produce
/// byte-identical arguments.
pub fn env_boot_args(env_vars: &HashMap<String, String>) -> Vec<String> {
    let mut keys: Vec<&String> = env_vars.keys().collect();
    keys.sort();
    keys.into_iter()
        .map(|k| {
            let key = encode_env_component(k, true);
            let val = encode_env_component(env_vars.get(k).expect("key present"), false);
            format!("hyphae.env.{key}={val}")
        })
        .collect()
}

/// Append the encoded `hyphae.env.*` boot arguments to a [`VmConfig`], if any
/// env vars are set. Used by both launch paths so env injection stays in
/// lockstep between direct and jailed mode.
pub(crate) fn apply_env_boot_args(
    vm_config: VmConfig,
    env_vars: Option<&HashMap<String, String>>,
) -> VmConfig {
    match env_vars {
        Some(vars) if !vars.is_empty() => {
            vm_config.with_additional_boot_args(env_boot_args(vars).join(" "))
        }
        _ => vm_config,
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    fn varmap(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn env_boot_args_encodes_spaces_and_percent_in_keys_and_values() {
        let args = env_boot_args(&varmap(&[
            ("GREETING", "hello world"),
            ("SPICY", "100% done"),
        ]));

        assert_eq!(
            args,
            vec![
                "hyphae.env.GREETING=hello%20world".to_string(),
                "hyphae.env.SPICY=100%25%20done".to_string(),
            ]
        );
    }

    #[test]
    fn env_boot_args_encodes_equals_in_keys_and_newlines_in_values() {
        let args = env_boot_args(&varmap(&[("A=B", "line one\nline two")]));

        assert_eq!(
            args,
            vec!["hyphae.env.A%3DB=line%20one%0Aline%20two".to_string()]
        );
    }

    #[test]
    fn env_boot_args_emits_sorted_keys_in_golden_format() {
        // The exact vars the buzz-agent bundle ships; this is the byte format
        // the direct path has always produced and hyphae-init decodes.
        let args = env_boot_args(&varmap(&[
            ("MODEL_NAME", "muse-glimmer-30b"),
            ("MODEL_BASE_URL", "http://192.168.105.168:8000/v1"),
        ]));

        assert_eq!(
            args.join(" "),
            "hyphae.env.MODEL_BASE_URL=http://192.168.105.168:8000/v1 hyphae.env.MODEL_NAME=muse-glimmer-30b"
        );
    }

    #[test]
    fn env_boot_args_empty_map_yields_no_args() {
        assert!(env_boot_args(&HashMap::new()).is_empty());
    }

    #[test]
    fn apply_env_boot_args_skips_none_and_empty() {
        let base = VmConfig::new("kernel", "rootfs", 2, 1024).with_boot_args("console=ttyS0");

        let untouched = apply_env_boot_args(base.clone(), None);
        let empty = apply_env_boot_args(base.clone(), Some(&HashMap::<String, String>::new()));

        assert_eq!(
            untouched.boot_source.boot_args,
            Some("console=ttyS0".to_string())
        );
        assert_eq!(
            empty.boot_source.boot_args,
            Some("console=ttyS0".to_string())
        );
    }

    #[test]
    fn direct_and_jailed_paths_inject_identical_env_boot_args() {
        // Parity: both launch modules call apply_env_boot_args at the same
        // position in their construction sequence (after extra boot args,
        // before volume args). Simulating each module's VmConfig construction
        // up to that point must yield byte-identical boot_args afterwards —
        // only the drive paths differ between the modes, never the env args.
        let env_vars = varmap(&[
            ("MODEL_NAME", "muse glimmer 30b"),
            ("MODEL_BASE_URL", "http://192.168.105.168:8000/v1"),
            ("PROMPT", "key=val 100%\nmulti line"),
        ]);

        // direct.rs builds VmConfig with ABSOLUTE paths.
        let mut direct_vm = VmConfig::new("/srv/kernel/vmlinux", "/srv/rootfs/ext4.img", 2, 1024)
            .with_boot_args("console=ttyS0 reboot=k panic=1");
        direct_vm = apply_env_boot_args(direct_vm, Some(&env_vars));

        // jailed.rs builds VmConfig with RELATIVE (chroot) paths.
        let mut jailed_vm = VmConfig::new("vmlinux", "ext4.img", 2, 1024)
            .with_boot_args("console=ttyS0 reboot=k panic=1");
        jailed_vm = apply_env_boot_args(jailed_vm, Some(&env_vars));

        let direct_args = direct_vm.boot_source.boot_args.expect("set");
        let jailed_args = jailed_vm.boot_source.boot_args.expect("set");

        let env_tokens = |args: &str| -> Vec<String> {
            args.split_whitespace()
                .filter(|t| t.starts_with("hyphae.env."))
                .map(str::to_string)
                .collect()
        };

        assert_eq!(env_tokens(&direct_args), env_tokens(&jailed_args));
        assert_eq!(env_tokens(&direct_args).len(), 3);
        // The env segment always lands after the default args, space-separated.
        assert!(direct_args.starts_with("console=ttyS0 reboot=k panic=1 "));
        assert!(jailed_args.starts_with("console=ttyS0 reboot=k panic=1 "));
    }
}
