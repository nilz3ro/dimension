//! Orchestration configuration and CID allocation.
//!
//! [`OrchestrationConfig`] defines two-tier timeouts (boot + processing)
//! with sensible defaults and server-enforced maximums. Per-request
//! overrides are clamped to the server max.
//!
//! [`CidAllocator`] provides unique guest CIDs via an atomic counter,
//! starting at 3 (the first valid guest CID; 0-2 are reserved by vsock).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use hyphae_core::jail::{JailConfig, validate_jail_user};
use hyphae_core::net::LanAllow;
use hyphae_core::launch::LaunchMode;
use hyphae_errors::JailError;

// ---------------------------------------------------------------------------
// OrchestrationConfig
// ---------------------------------------------------------------------------

/// Configuration for VM lifecycle orchestration.
///
/// Defines server-wide defaults and maximums for timeouts, paths to
/// required binaries, and a mock flag for testing without real VMs.
#[derive(Debug, Clone)]
pub struct OrchestrationConfig {
    /// Default boot timeout (spawn + vsock connect phase).
    pub boot_timeout: Duration,

    /// Default processing timeout (guest work phase).
    pub processing_timeout: Duration,

    /// Maximum allowed boot timeout (server-enforced ceiling).
    pub max_boot_timeout: Duration,

    /// Maximum allowed processing timeout (server-enforced ceiling).
    pub max_processing_timeout: Duration,

    /// Path to the kernel binary used for VM boot.
    pub kernel_path: PathBuf,

    /// Path to the Firecracker binary.
    pub firecracker_bin: PathBuf,

    /// When true, use mock VM implementation (no real Firecracker).
    pub mock: bool,

    /// When true, create TAP devices and NAT rules for VM internet access.
    pub enable_network: bool,

    /// Destination-scoped LAN exceptions (`CIDR:PORT`, TCP only) applied
    /// when NAT is enabled. Empty preserves full LAN isolation. The
    /// gateway's own deployment orchestration does not run NAT'd local
    /// VMs, so this stays empty here; the worker populates it from
    /// `DIMENSION_LAN_ALLOW`.
    pub lan_allow: Vec<LanAllow>,

    /// When true, guest stderr output is suppressed (logged server-side only).
    /// When false, stderr is forwarded to the client as a BackendEvent::Message.
    /// Defaults to `true` to preserve existing behavior.
    pub suppress_guest_stderr: bool,

    /// Path to the Firecracker jailer binary.
    /// When set, VMs launch in jailed mode (chroot isolation).
    /// When `None`, VMs launch in direct mode (no jailer).
    pub jailer_bin: Option<PathBuf>,

    /// When true, refuse to launch VMs without jailer isolation.
    pub require_jail: bool,

    /// Base directory for jailer chroots (default: /srv/jailer).
    /// Only used when `jailer_bin` is set.
    pub chroot_base_dir: PathBuf,
}

impl Default for OrchestrationConfig {
    fn default() -> Self {
        Self {
            boot_timeout: Duration::from_secs(30),
            processing_timeout: Duration::from_secs(300),
            max_boot_timeout: Duration::from_secs(60),
            max_processing_timeout: Duration::from_secs(600),
            kernel_path: PathBuf::from("/opt/hyphae/kernel/vmlinux"),
            firecracker_bin: PathBuf::from("firecracker"),
            mock: false,
            enable_network: false,
            lan_allow: Vec::new(),
            suppress_guest_stderr: true,
            jailer_bin: None,
            require_jail: false,
            chroot_base_dir: PathBuf::from("/srv/jailer"),
        }
    }
}

impl OrchestrationConfig {
    /// Resolve the configured launch mode for a VM.
    ///
    /// A configured jailer is always fail-closed: an invalid path or missing
    /// jail user returns an error instead of falling back to direct mode.
    pub fn resolve_launch_mode(&self, vm_id: String) -> Result<LaunchMode, JailError> {
        let Some(jailer_bin) = self.jailer_bin.as_ref() else {
            if self.require_jail {
                return Err(JailError::JailerNotFound {
                    path: PathBuf::from("DIMENSION_JAILER_BIN"),
                });
            }
            return Ok(LaunchMode::Direct);
        };

        if !jailer_bin.is_file() {
            return Err(JailError::JailerNotFound {
                path: jailer_bin.clone(),
            });
        }

        let jail_user = validate_jail_user()?;
        let mut jail_config = JailConfig::new(
            jailer_bin.clone(),
            self.firecracker_bin.clone(),
            vm_id,
            jail_user.uid,
            jail_user.gid,
        );
        jail_config.chroot_base_dir = self.chroot_base_dir.clone();

        Ok(LaunchMode::Jailed(jail_config))
    }

    /// Resolve the boot timeout for a request.
    ///
    /// If the request provides an override, it is clamped to the server
    /// maximum. Otherwise the server default is used.
    pub fn resolve_boot_timeout(&self, request_secs: Option<u64>) -> Duration {
        match request_secs {
            Some(secs) => {
                let requested = Duration::from_secs(secs);
                std::cmp::min(requested, self.max_boot_timeout)
            }
            None => self.boot_timeout,
        }
    }

    /// Resolve the processing timeout for a request.
    ///
    /// Three-tier precedence: per-request override > manifest timeout > server default.
    /// Both per-request and manifest values are clamped to `max_processing_timeout`.
    ///
    /// - `request_secs`: per-request override from the API call (highest priority)
    /// - `manifest_secs`: timeout from dimension.toml `[resources].timeout_secs` (mid priority)
    /// - server default (`self.processing_timeout`): fallback when neither is provided
    pub fn resolve_processing_timeout(
        &self,
        request_secs: Option<u64>,
        manifest_secs: Option<i64>,
    ) -> Duration {
        match request_secs {
            Some(secs) => {
                let requested = Duration::from_secs(secs);
                std::cmp::min(requested, self.max_processing_timeout)
            }
            None => match manifest_secs {
                Some(secs) if secs > 0 => {
                    let manifest = Duration::from_secs(secs as u64);
                    std::cmp::min(manifest, self.max_processing_timeout)
                }
                _ => self.processing_timeout,
            },
        }
    }
}

// ---------------------------------------------------------------------------
// CidAllocator
// ---------------------------------------------------------------------------

/// Allocates unique guest CIDs (Context Identifiers) for vsock.
///
/// CIDs 0, 1, and 2 are reserved:
/// - 0: hypervisor
/// - 1: reserved (loopback-like)
/// - 2: host
///
/// Guest CIDs start at 3 and increment atomically. The counter wraps
/// around at `u32::MAX` which is acceptable -- CIDs are only needed
/// for the lifetime of a single VM, and collisions at 4 billion are
/// practically impossible.
#[derive(Debug)]
pub struct CidAllocator {
    next: AtomicU32,
}

impl CidAllocator {
    /// Create a new allocator starting at CID 3.
    pub fn new() -> Self {
        Self {
            next: AtomicU32::new(3),
        }
    }

    /// Allocate the next unique guest CID.
    ///
    /// This is lock-free and safe to call from multiple threads.
    pub fn allocate(&self) -> u32 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }
}

impl Default for CidAllocator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_expected_timeouts() {
        let config = OrchestrationConfig::default();
        assert_eq!(config.boot_timeout, Duration::from_secs(30));
        assert_eq!(config.processing_timeout, Duration::from_secs(300));
        assert_eq!(config.max_boot_timeout, Duration::from_secs(60));
        assert_eq!(config.max_processing_timeout, Duration::from_secs(600));
    }

    #[test]
    fn default_config_uses_direct_launch_mode() {
        let config = OrchestrationConfig::default();
        assert!(matches!(
            config.resolve_launch_mode("test-vm".to_string()),
            Ok(LaunchMode::Direct)
        ));
    }

    #[test]
    fn required_jail_without_binary_fails_closed() {
        let config = OrchestrationConfig {
            require_jail: true,
            ..OrchestrationConfig::default()
        };
        assert!(matches!(
            config.resolve_launch_mode("test-vm".to_string()),
            Err(JailError::JailerNotFound { .. })
        ));
    }

    #[test]
    fn resolve_boot_timeout_uses_default_when_none() {
        let config = OrchestrationConfig::default();
        assert_eq!(config.resolve_boot_timeout(None), Duration::from_secs(30));
    }

    #[test]
    fn resolve_boot_timeout_clamps_to_max() {
        let config = OrchestrationConfig::default();
        // Request 120s, max is 60s -> should clamp to 60s
        assert_eq!(
            config.resolve_boot_timeout(Some(120)),
            Duration::from_secs(60)
        );
    }

    #[test]
    fn resolve_boot_timeout_allows_under_max() {
        let config = OrchestrationConfig::default();
        // Request 15s, max is 60s -> should use 15s
        assert_eq!(
            config.resolve_boot_timeout(Some(15)),
            Duration::from_secs(15)
        );
    }

    #[test]
    fn resolve_processing_timeout_uses_default_when_none() {
        let config = OrchestrationConfig::default();
        assert_eq!(
            config.resolve_processing_timeout(None, None),
            Duration::from_secs(300)
        );
    }

    #[test]
    fn resolve_processing_timeout_clamps_to_max() {
        let config = OrchestrationConfig::default();
        // Request 900s, max is 600s -> should clamp to 600s
        assert_eq!(
            config.resolve_processing_timeout(Some(900), None),
            Duration::from_secs(600)
        );
    }

    #[test]
    fn resolve_processing_timeout_allows_under_max() {
        let config = OrchestrationConfig::default();
        assert_eq!(
            config.resolve_processing_timeout(Some(120), None),
            Duration::from_secs(120)
        );
    }

    #[test]
    fn resolve_processing_timeout_uses_manifest_when_no_request() {
        let config = OrchestrationConfig::default();
        // No per-request override, manifest says 60s -> should use 60s
        assert_eq!(
            config.resolve_processing_timeout(None, Some(60)),
            Duration::from_secs(60)
        );
    }

    #[test]
    fn resolve_processing_timeout_request_overrides_manifest() {
        let config = OrchestrationConfig::default();
        // Per-request 120s wins over manifest 60s
        assert_eq!(
            config.resolve_processing_timeout(Some(120), Some(60)),
            Duration::from_secs(120)
        );
    }

    #[test]
    fn resolve_processing_timeout_clamps_manifest_to_max() {
        let config = OrchestrationConfig::default();
        // Manifest 9999s, max is 600s -> should clamp to 600s
        assert_eq!(
            config.resolve_processing_timeout(None, Some(9999)),
            Duration::from_secs(600)
        );
    }

    #[test]
    fn cid_allocator_starts_at_3() {
        let alloc = CidAllocator::new();
        assert_eq!(alloc.allocate(), 3);
    }

    #[test]
    fn cid_allocator_increments() {
        let alloc = CidAllocator::new();
        assert_eq!(alloc.allocate(), 3);
        assert_eq!(alloc.allocate(), 4);
        assert_eq!(alloc.allocate(), 5);
    }

    #[test]
    fn cid_allocator_is_unique_across_calls() {
        let alloc = CidAllocator::new();
        let cids: Vec<u32> = (0..100).map(|_| alloc.allocate()).collect();
        let mut sorted = cids.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), cids.len(), "all CIDs must be unique");
        assert_eq!(cids[0], 3, "first CID must be 3");
    }

    #[test]
    fn cid_allocator_default_starts_at_3() {
        let alloc = CidAllocator::default();
        assert_eq!(alloc.allocate(), 3);
    }
}
