//! Firecracker VM configuration types.
//!
//! This module provides typed Rust structs that serialize to valid
//! Firecracker `--config-file` JSON. The top-level [`VmConfig`] struct
//! uses kebab-case for section names (matching Firecracker's format),
//! while inner structs use snake_case field names.

pub mod boot_source;
pub mod drive;
pub mod logging;
pub mod machine;
pub mod network;
pub mod vsock;

use serde::{Deserialize, Serialize};

use hyphae_errors::{ConfigError, ConfigValidationError};

pub use boot_source::BootSource;
pub use drive::DriveConfig;
pub use logging::{LoggerConfig, MetricsConfig};
pub use machine::MachineConfig;
pub use network::NetworkInterface;
pub use vsock::VsockConfig;

/// MMDS (MicroVM Metadata Service) configuration for the Firecracker config file.
///
/// This struct represents the `mmds-config` section in the Firecracker
/// `--config-file` JSON. It tells Firecracker which network interfaces to
/// attach MMDS to, what protocol version to use, and what IPv4 address to
/// serve metadata on.
///
/// Note: This configures MMDS availability on the guest network interface.
/// The actual metadata payload is pushed separately via the API socket
/// using `PUT /mmds` after Firecracker starts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MmdsFirecrackerConfig {
    /// MMDS protocol version (`"V1"` or `"V2"`). V2 uses session tokens.
    pub version: String,
    /// Network interface IDs that MMDS should be accessible through.
    pub network_interfaces: Vec<String>,
    /// IPv4 address for the MMDS endpoint (default: `169.254.169.254`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ipv4_address: Option<String>,
}

/// Top-level Firecracker VM configuration.
///
/// Serializes to JSON matching Firecracker's `--config-file` format.
/// Top-level keys use kebab-case (`boot-source`, `machine-config`, etc.)
/// while nested field names use snake_case (`kernel_image_path`, `vcpu_count`, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct VmConfig {
    pub boot_source: BootSource,
    pub machine_config: MachineConfig,
    pub drives: Vec<DriveConfig>,

    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub network_interfaces: Vec<NetworkInterface>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub vsock: Option<VsockConfig>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub logger: Option<LoggerConfig>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics: Option<MetricsConfig>,

    /// MMDS configuration. When present, enables MMDS on the specified
    /// network interfaces so the guest can read metadata from the MMDS
    /// IPv4 address (default 169.254.169.254).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mmds_config: Option<MmdsFirecrackerConfig>,
}

impl VmConfig {
    /// Create a new VmConfig with the four required fields.
    ///
    /// This constructor is infallible -- validation runs on [`to_json()`].
    /// The rootfs drive is automatically created with `drive_id: "rootfs"`,
    /// `is_root_device: true`, and `is_read_only: false`.
    pub fn new(
        kernel_image_path: impl Into<String>,
        rootfs_path: impl Into<String>,
        vcpu_count: u8,
        mem_size_mib: u64,
    ) -> Self {
        let rootfs = DriveConfig {
            drive_id: "rootfs".to_string(),
            path_on_host: rootfs_path.into(),
            is_root_device: true,
            is_read_only: Some(false),
            cache_type: None,
            io_engine: None,
            partuuid: None,
            rate_limiter: None,
            socket: None,
        };

        VmConfig {
            boot_source: BootSource {
                kernel_image_path: kernel_image_path.into(),
                boot_args: None,
                initrd_path: None,
            },
            machine_config: MachineConfig {
                vcpu_count,
                mem_size_mib,
                smt: None,
                track_dirty_pages: None,
            },
            drives: vec![rootfs],
            network_interfaces: Vec::new(),
            vsock: None,
            logger: None,
            metrics: None,
            mmds_config: None,
        }
    }

    /// Add boot arguments (e.g., "console=ttyS0 reboot=k panic=1").
    pub fn with_boot_args(mut self, args: impl Into<String>) -> Self {
        self.boot_source.boot_args = Some(args.into());
        self
    }

    /// Add a network interface.
    pub fn with_network(
        mut self,
        iface_id: impl Into<String>,
        host_dev_name: impl Into<String>,
    ) -> Self {
        self.network_interfaces.push(NetworkInterface {
            iface_id: iface_id.into(),
            host_dev_name: host_dev_name.into(),
            guest_mac: None,
            rx_rate_limiter: None,
            tx_rate_limiter: None,
        });
        self
    }

    /// Add a network interface with an explicit guest MAC address.
    pub fn with_network_interface(
        mut self,
        iface_id: impl Into<String>,
        host_dev_name: impl Into<String>,
        guest_mac: impl Into<String>,
    ) -> Self {
        self.network_interfaces.push(NetworkInterface {
            iface_id: iface_id.into(),
            host_dev_name: host_dev_name.into(),
            guest_mac: Some(guest_mac.into()),
            rx_rate_limiter: None,
            tx_rate_limiter: None,
        });
        self
    }

    /// Append additional boot arguments to any existing boot_args.
    ///
    /// If boot_args is already set, the new args are appended with a space separator.
    /// If boot_args is None, the new args become the sole boot_args value.
    pub fn with_additional_boot_args(mut self, extra_args: impl Into<String>) -> Self {
        let extra = extra_args.into();
        self.boot_source.boot_args = Some(match self.boot_source.boot_args.take() {
            Some(existing) => format!("{existing} {extra}"),
            None => extra,
        });
        self
    }

    /// Add a vsock device.
    pub fn with_vsock(mut self, guest_cid: u32, uds_path: impl Into<String>) -> Self {
        self.vsock = Some(VsockConfig {
            guest_cid,
            uds_path: uds_path.into(),
        });
        self
    }

    /// Add an additional drive (non-rootfs).
    pub fn with_drive(mut self, drive: DriveConfig) -> Self {
        self.drives.push(drive);
        self
    }

    /// Add logger configuration.
    pub fn with_logger(
        mut self,
        log_path: impl Into<String>,
        level: impl Into<String>,
    ) -> Self {
        self.logger = Some(LoggerConfig {
            log_path: Some(log_path.into()),
            level: Some(level.into()),
            show_level: None,
            show_log_origin: None,
            module: None,
        });
        self
    }

    /// Add metrics configuration.
    pub fn with_metrics(mut self, metrics_path: impl Into<String>) -> Self {
        self.metrics = Some(MetricsConfig {
            metrics_path: metrics_path.into(),
        });
        self
    }

    /// Add MMDS configuration for guest-side metadata access.
    ///
    /// Enables MMDS on the specified network interfaces so the guest VM
    /// can read metadata from the MMDS IPv4 address. The metadata payload
    /// itself is pushed separately via the API socket after Firecracker starts.
    ///
    /// `version` should be `"V1"` or `"V2"`. `iface_ids` lists the network
    /// interface IDs (e.g., `["eth0"]`). `ipv4_address` overrides the default
    /// `169.254.169.254` endpoint address.
    pub fn with_mmds_config(
        mut self,
        version: impl Into<String>,
        iface_ids: Vec<String>,
        ipv4_address: Option<String>,
    ) -> Self {
        self.mmds_config = Some(MmdsFirecrackerConfig {
            version: version.into(),
            network_interfaces: iface_ids,
            ipv4_address,
        });
        self
    }

    /// Validate and serialize to pretty-printed JSON.
    ///
    /// Validation collects ALL errors before returning. If any validation
    /// fails, returns `ConfigError::ValidationFailed` containing every
    /// detected issue. If validation passes, returns the serialized JSON.
    pub fn to_json(&self) -> Result<String, ConfigError> {
        self.validate()?;
        serde_json::to_string_pretty(self)
            .map_err(|e| ConfigError::Serialization(e.to_string()))
    }

    /// Validate all config fields, collecting all errors.
    ///
    /// Does NOT short-circuit on the first error. All validation rules
    /// are checked and all failures are returned together.
    fn validate(&self) -> Result<(), ConfigError> {
        let mut errors = Vec::new();

        // Machine config: vcpu_count must be 1-32 and either 1 or even
        if self.machine_config.vcpu_count == 0
            || self.machine_config.vcpu_count > 32
            || (self.machine_config.vcpu_count != 1
                && self.machine_config.vcpu_count % 2 != 0)
        {
            errors.push(ConfigValidationError::InvalidVcpuCount {
                value: self.machine_config.vcpu_count,
            });
        }

        // Machine config: mem_size_mib must be >= 8
        if self.machine_config.mem_size_mib < 8 {
            errors.push(ConfigValidationError::InvalidMemSize {
                value: self.machine_config.mem_size_mib,
            });
        }

        // Vsock: guest_cid must be >= 3
        if let Some(ref vsock) = self.vsock {
            if vsock.guest_cid < 3 {
                errors.push(ConfigValidationError::InvalidGuestCid {
                    value: vsock.guest_cid,
                });
            }
        }

        // Drives: must have at least one root device
        let has_root = self.drives.iter().any(|d| d.is_root_device);
        if !has_root {
            errors.push(ConfigValidationError::NoRootDevice);
        }

        // Drives: no empty path_on_host
        for drive in &self.drives {
            if drive.path_on_host.is_empty() {
                errors.push(ConfigValidationError::MissingDrivePath {
                    drive_id: drive.drive_id.clone(),
                });
            }
        }

        // Drives: no duplicate drive_ids
        let mut seen_ids = std::collections::HashSet::new();
        for drive in &self.drives {
            if !seen_ids.insert(&drive.drive_id) {
                errors.push(ConfigValidationError::DuplicateDriveId {
                    drive_id: drive.drive_id.clone(),
                });
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(ConfigError::ValidationFailed(errors))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // === Serialization tests ===

    #[test]
    fn minimal_config_serializes_to_valid_json() {
        let config = VmConfig::new("vmlinux.bin", "rootfs.ext4", 2, 256);
        let json = config.to_json().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        // Top-level keys are kebab-case
        assert!(parsed.get("boot-source").is_some(), "missing boot-source key");
        assert!(parsed.get("machine-config").is_some(), "missing machine-config key");
        assert!(parsed.get("drives").is_some(), "missing drives key");

        // Nested fields are snake_case
        assert_eq!(parsed["boot-source"]["kernel_image_path"], "vmlinux.bin");
        assert_eq!(parsed["machine-config"]["vcpu_count"], 2);
        assert_eq!(parsed["machine-config"]["mem_size_mib"], 256);

        // Rootfs drive is present
        assert_eq!(parsed["drives"][0]["drive_id"], "rootfs");
        assert_eq!(parsed["drives"][0]["path_on_host"], "rootfs.ext4");
        assert_eq!(parsed["drives"][0]["is_root_device"], true);
        assert_eq!(parsed["drives"][0]["is_read_only"], false);
    }

    #[test]
    fn optional_fields_omitted_when_not_set() {
        let config = VmConfig::new("vmlinux.bin", "rootfs.ext4", 1, 128);
        let json = config.to_json().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert!(parsed.get("vsock").is_none(), "vsock should be omitted");
        assert!(parsed.get("logger").is_none(), "logger should be omitted");
        assert!(parsed.get("metrics").is_none(), "metrics should be omitted");
        assert!(
            parsed.get("network-interfaces").is_none(),
            "network-interfaces should be omitted when empty"
        );
    }

    #[test]
    fn full_config_with_all_devices() {
        let config = VmConfig::new("vmlinux.bin", "rootfs.ext4", 2, 512)
            .with_boot_args("console=ttyS0 reboot=k panic=1")
            .with_network("eth0", "tap0")
            .with_vsock(3, "/tmp/vsock.sock")
            .with_logger("/tmp/firecracker.log", "Info")
            .with_metrics("/tmp/metrics.json");

        let json = config.to_json().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        // Boot args present
        assert_eq!(
            parsed["boot-source"]["boot_args"],
            "console=ttyS0 reboot=k panic=1"
        );

        // Network interface present
        assert_eq!(parsed["network-interfaces"][0]["iface_id"], "eth0");
        assert_eq!(parsed["network-interfaces"][0]["host_dev_name"], "tap0");

        // Vsock present
        assert_eq!(parsed["vsock"]["guest_cid"], 3);
        assert_eq!(parsed["vsock"]["uds_path"], "/tmp/vsock.sock");

        // Logger present
        assert_eq!(parsed["logger"]["log_path"], "/tmp/firecracker.log");
        assert_eq!(parsed["logger"]["level"], "Info");

        // Metrics present
        assert_eq!(parsed["metrics"]["metrics_path"], "/tmp/metrics.json");
    }

    #[test]
    fn with_drive_adds_additional_drive() {
        let data_drive = DriveConfig::new("data", "/mnt/data.ext4", false);
        let config = VmConfig::new("vmlinux.bin", "rootfs.ext4", 2, 256)
            .with_drive(data_drive);

        let json = config.to_json().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed["drives"].as_array().unwrap().len(), 2);
        assert_eq!(parsed["drives"][0]["drive_id"], "rootfs");
        assert_eq!(parsed["drives"][1]["drive_id"], "data");
        assert_eq!(parsed["drives"][1]["is_read_only"], false);
    }

    // === Validation tests ===

    #[test]
    fn zero_vcpus_produces_validation_error() {
        let config = VmConfig::new("vmlinux.bin", "rootfs.ext4", 0, 256);
        let result = config.to_json();
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("E201"), "expected E201 error code, got: {err}");
    }

    #[test]
    fn odd_vcpus_other_than_one_rejected() {
        let config = VmConfig::new("vmlinux.bin", "rootfs.ext4", 3, 256);
        let result = config.to_json();
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("E201"), "expected E201 for odd vcpu count 3, got: {err}");
    }

    #[test]
    fn vcpu_count_one_is_valid() {
        let config = VmConfig::new("vmlinux.bin", "rootfs.ext4", 1, 256);
        assert!(config.to_json().is_ok(), "vcpu_count = 1 should be valid");
    }

    #[test]
    fn vcpu_count_32_is_valid() {
        let config = VmConfig::new("vmlinux.bin", "rootfs.ext4", 32, 256);
        assert!(config.to_json().is_ok(), "vcpu_count = 32 should be valid");
    }

    #[test]
    fn vcpu_count_above_32_rejected() {
        let config = VmConfig::new("vmlinux.bin", "rootfs.ext4", 34, 256);
        let result = config.to_json();
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("E201"), "expected E201 for vcpu_count > 32, got: {err}");
    }

    #[test]
    fn mem_below_8mb_produces_validation_error() {
        let config = VmConfig::new("vmlinux.bin", "rootfs.ext4", 2, 4);
        let result = config.to_json();
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("E202"), "expected E202 error code, got: {err}");
    }

    #[test]
    fn mem_exactly_8mb_is_valid() {
        let config = VmConfig::new("vmlinux.bin", "rootfs.ext4", 2, 8);
        assert!(config.to_json().is_ok(), "mem_size_mib = 8 should be valid");
    }

    #[test]
    fn invalid_vsock_guest_cid_produces_error() {
        let config = VmConfig::new("vmlinux.bin", "rootfs.ext4", 2, 256)
            .with_vsock(1, "/tmp/vsock.sock");
        let result = config.to_json();
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("E203"), "expected E203 error code, got: {err}");
    }

    #[test]
    fn vsock_guest_cid_3_is_valid() {
        let config = VmConfig::new("vmlinux.bin", "rootfs.ext4", 2, 256)
            .with_vsock(3, "/tmp/vsock.sock");
        assert!(config.to_json().is_ok(), "guest_cid = 3 should be valid");
    }

    #[test]
    fn duplicate_drive_ids_produce_error() {
        let dup_drive = DriveConfig::new("rootfs", "/mnt/other.ext4", false);
        let config = VmConfig::new("vmlinux.bin", "rootfs.ext4", 2, 256)
            .with_drive(dup_drive);
        let result = config.to_json();
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("E205"), "expected E205 error code, got: {err}");
    }

    #[test]
    fn multiple_validation_errors_collected_together() {
        let config = VmConfig::new("vmlinux.bin", "rootfs.ext4", 0, 4)
            .with_vsock(1, "/tmp/vsock.sock");
        let result = config.to_json();
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        // All three errors should be present
        assert!(err.contains("E201"), "expected E201 (vcpu), got: {err}");
        assert!(err.contains("E202"), "expected E202 (mem), got: {err}");
        assert!(err.contains("E203"), "expected E203 (vsock cid), got: {err}");
    }

    #[test]
    fn config_fields_are_mutable_after_construction() {
        let mut config = VmConfig::new("vmlinux.bin", "rootfs.ext4", 2, 256);
        config.machine_config.vcpu_count = 4;
        config.machine_config.mem_size_mib = 1024;
        config.boot_source.boot_args = Some("console=ttyS0".to_string());

        let json = config.to_json().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["machine-config"]["vcpu_count"], 4);
        assert_eq!(parsed["machine-config"]["mem_size_mib"], 1024);
        assert_eq!(parsed["boot-source"]["boot_args"], "console=ttyS0");
    }

    // === Builder chaining test ===

    #[test]
    fn builder_methods_chain() {
        // Verify all builder methods return Self for chaining
        let _config = VmConfig::new("vmlinux.bin", "rootfs.ext4", 2, 256)
            .with_boot_args("console=ttyS0")
            .with_network("eth0", "tap0")
            .with_vsock(3, "/tmp/vsock.sock")
            .with_drive(DriveConfig::new("data", "/mnt/data.ext4", false))
            .with_logger("/tmp/fc.log", "Warning")
            .with_metrics("/tmp/metrics.json")
            .with_mmds_config("V2", vec!["eth0".to_string()], None);
        // If this compiles, chaining works
    }

    // === MMDS config tests ===

    #[test]
    fn mmds_config_serializes_with_kebab_case_key() {
        let config = VmConfig::new("vmlinux.bin", "rootfs.ext4", 2, 256)
            .with_network("eth0", "tap0")
            .with_mmds_config("V2", vec!["eth0".to_string()], None);

        let json = config.to_json().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        // The key must be kebab-case "mmds-config" (not "mmds_config")
        assert!(
            parsed.get("mmds-config").is_some(),
            "missing mmds-config key (kebab-case); keys: {:?}",
            parsed.as_object().unwrap().keys().collect::<Vec<_>>()
        );

        let mmds = &parsed["mmds-config"];
        assert_eq!(mmds["version"], "V2");
        assert_eq!(mmds["network_interfaces"][0], "eth0");
        // ipv4_address should be omitted when None
        assert!(
            mmds.get("ipv4_address").is_none(),
            "ipv4_address should be omitted when None"
        );
    }

    #[test]
    fn mmds_config_includes_ipv4_address_when_set() {
        let config = VmConfig::new("vmlinux.bin", "rootfs.ext4", 2, 256)
            .with_network("eth0", "tap0")
            .with_mmds_config(
                "V1",
                vec!["eth0".to_string()],
                Some("169.254.169.100".to_string()),
            );

        let json = config.to_json().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        let mmds = &parsed["mmds-config"];
        assert_eq!(mmds["version"], "V1");
        assert_eq!(mmds["ipv4_address"], "169.254.169.100");
    }

    #[test]
    fn mmds_config_omitted_when_not_set() {
        let config = VmConfig::new("vmlinux.bin", "rootfs.ext4", 2, 256);
        let json = config.to_json().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert!(
            parsed.get("mmds-config").is_none(),
            "mmds-config should be omitted when not set"
        );
    }

    #[test]
    fn mmds_config_with_multiple_interfaces() {
        let config = VmConfig::new("vmlinux.bin", "rootfs.ext4", 2, 256)
            .with_network("eth0", "tap0")
            .with_mmds_config(
                "V2",
                vec!["eth0".to_string(), "eth1".to_string()],
                None,
            );

        let json = config.to_json().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        let ifaces = parsed["mmds-config"]["network_interfaces"]
            .as_array()
            .unwrap();
        assert_eq!(ifaces.len(), 2);
        assert_eq!(ifaces[0], "eth0");
        assert_eq!(ifaces[1], "eth1");
    }
}
