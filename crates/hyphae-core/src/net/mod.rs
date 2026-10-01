//! Networking subsystem: TAP devices, subnet allocation, NAT rules.
//!
//! Provides RAII-based network resource management for Firecracker VMs:
//! - TAP device creation and teardown
//! - /30 subnet allocation from 172.16.0.0/16
//! - iptables NAT/masquerade rules
//! - MAC address generation
//! - Orchestrated per-VM network setup via [`setup_vm_network`]

pub mod ip_alloc;
pub mod nat;
pub mod tap;

pub use ip_alloc::{generate_mac, guest_ip_for_index, SubnetAllocation, SubnetAllocator};
pub use nat::{LanAllow, NatRules};
pub use tap::TapDevice;

use hyphae_errors::NetworkError;
use tracing::{info, warn};

/// Bundled network resources for a single VM.
///
/// Holds the TAP device, optional NAT rules, and the subnet allocation.
/// On drop, NAT rules are removed BEFORE the TAP device is deleted
/// (correct ordering: rules reference the TAP interface).
///
/// Note: The `SubnetAllocator` is NOT automatically updated on drop.
/// The caller is responsible for releasing the allocation index back to
/// the allocator when the VM is fully shut down.
pub struct VmNetworkResources {
    /// The TAP network device (deleted on drop).
    pub tap: TapDevice,
    /// Per-VM NAT rules (removed on drop if present).
    pub nat: Option<NatRules>,
    /// The subnet allocation details (index, IPs, MAC, TAP name).
    pub allocation: SubnetAllocation,
}

impl Drop for VmNetworkResources {
    fn drop(&mut self) {
        // Remove NAT rules BEFORE dropping TAP device.
        // Rules reference the TAP interface, so they must be removed first.
        if let Some(ref nat) = self.nat {
            let _ = nat.remove();
        }
        // TapDevice::drop() handles deleting the TAP interface.
    }
}

/// Orchestrate full network setup for a VM.
///
/// 1. Allocates a /30 subnet (index, IPs, MAC, TAP name)
/// 2. Creates the TAP device with the allocated IPs
/// 3. Optionally adds NAT rules for internet access, honoring the
///    destination-scoped [`LanAllow`] allowlist (empty = unchanged
///    LAN isolation)
///
/// On partial failure, cleans up all previously created resources:
/// - If TAP creation fails, releases the subnet allocation
/// - If NAT setup fails, releases the allocation and drops the TAP device
pub fn setup_vm_network(
    allocator: &mut SubnetAllocator,
    enable_nat: bool,
    lan_allow: &[LanAllow],
) -> Result<VmNetworkResources, NetworkError> {
    let alloc = allocator.allocate()?;

    let tap = match TapDevice::create(&alloc.tap_name, alloc.host_ip, alloc.guest_ip) {
        Ok(tap) => tap,
        Err(e) => {
            allocator.release(alloc.index);
            return Err(e);
        }
    };

    let nat = if enable_nat {
        match NatRules::add(&alloc.tap_name, alloc.guest_ip, lan_allow) {
            Ok(nat) => Some(nat),
            Err(e) => {
                allocator.release(alloc.index);
                drop(tap);
                return Err(e);
            }
        }
    } else {
        None
    };

    Ok(VmNetworkResources {
        tap,
        nat,
        allocation: alloc,
    })
}

/// Best-effort cleanup of orphaned `hyphae-tap*` devices.
///
/// Scans `/sys/class/net/` for TAP devices matching the `hyphae-tap` prefix
/// and deletes them via `ip link del`. This is useful on startup to clean up
/// devices left behind by a previous crash.
///
/// Errors are logged but do not propagate -- this is a best-effort operation.
///
/// Note: this removes only the TAP devices; per-VM NAT rules survive. Use
/// [`recover_orphan_network`] when the allowlist is known so rules are
/// removed too.
pub fn recover_orphan_taps() {
    for name in scan_orphan_tap_names() {
        match tap::run_ip_cmd(&["link", "del", &name]) {
            Ok(()) => {
                info!(tap = %name, "deleted orphan TAP device");
            }
            Err(e) => {
                warn!(tap = %name, error = %e, "failed to delete orphan TAP device");
            }
        }
    }
}

/// Summary of a startup orphan-network recovery pass.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct NetworkRecoveryReport {
    /// Orphaned `hyphae-tap*` devices found.
    pub taps_found: usize,
    /// TAP devices successfully deleted.
    pub taps_deleted: usize,
    /// Per-VM NAT rule sets for which removal commands were issued.
    pub nat_rule_sets_removed: usize,
}

/// Recover network resources orphaned by a crashed worker.
///
/// For each orphaned `hyphae-tapN` device this removes the per-VM NAT +
/// LAN-isolation rules (reconstructed from the TAP index and the *current*
/// allowlist) and then deletes the TAP device. Run this at worker startup,
/// before constructing the [`SubnetAllocator`], so recovered indices are
/// returned to the pool.
///
/// Caveat: if `DIMENSION_LAN_ALLOW` changed between the orphan's rules being
/// installed and recovery, the scoped ACCEPT rule for the old entry is not
/// covered by the reconstruction and survives; it matches a nonexistent
/// `-i tapN` interface, but a flush of the FORWARD chain is the fix. This
/// is logged as a warning when the allowlist is non-empty.
pub fn recover_orphan_network(lan_allow: &[LanAllow]) -> NetworkRecoveryReport {
    let names = scan_orphan_tap_names();
    recover_orphan_network_with(
        lan_allow,
        &names,
        &|tap, guest_ip| {
            NatRules::reconstruct(tap, guest_ip, lan_allow)
                .remove()
                .is_ok()
        },
        &|tap| tap::run_ip_cmd(&["link", "del", tap]),
    )
}

/// Backend-injectable core of [`recover_orphan_network`], so orphan
/// recovery is unit-testable without root, iptables, or `/sys`.
fn recover_orphan_network_with(
    lan_allow: &[LanAllow],
    tap_names: &[String],
    remove_nat_rules: &dyn Fn(&str, std::net::Ipv4Addr) -> bool,
    delete_tap: &dyn Fn(&str) -> Result<(), NetworkError>,
) -> NetworkRecoveryReport {
    let mut report = NetworkRecoveryReport {
        taps_found: tap_names.len(),
        ..Default::default()
    };

    if !lan_allow.is_empty() && !tap_names.is_empty() {
        warn!(
            entries = lan_allow
                .iter()
                .map(|e| format!("{}:{}", e.cidr, e.port))
                .collect::<Vec<_>>()
                .join(","),
            "orphan recovery reconstructs NAT rules from the CURRENT allowlist; \
             stale scoped ACCEPTs from a previous allowlist may remain"
        );
    }

    for tap in tap_names {
        // NAT rules reference the TAP interface, so remove them first.
        let index = tap
            .strip_prefix("hyphae-tap")
            .and_then(|s| s.parse::<u32>().ok());
        match index.and_then(guest_ip_for_index) {
            Some(guest_ip) => {
                if remove_nat_rules(tap, guest_ip) {
                    report.nat_rule_sets_removed += 1;
                } else {
                    warn!(tap = %tap, "failed to remove orphan NAT rules");
                }
            }
            None => {
                warn!(
                    tap = %tap,
                    "orphan TAP has no derivable subnet index; skipping NAT rule removal"
                );
            }
        }

        match delete_tap(tap) {
            Ok(()) => {
                report.taps_deleted += 1;
                info!(tap = %tap, "deleted orphan TAP device");
            }
            Err(e) => {
                warn!(tap = %tap, error = %e, "failed to delete orphan TAP device");
            }
        }
    }

    report
}

/// Scan `/sys/class/net/` for TAP device names with the `hyphae-tap` prefix.
fn scan_orphan_tap_names() -> Vec<String> {
    let entries = match std::fs::read_dir("/sys/class/net/") {
        Ok(entries) => entries,
        Err(e) => {
            warn!(error = %e, "cannot scan /sys/class/net/ for orphan TAP devices");
            return Vec::new();
        }
    };

    entries
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name.starts_with("hyphae-tap"))
        .collect()
}

/// Generate a Linux kernel `ip=` boot parameter for the guest.
///
/// Format: `ip={guest_ip}::{host_ip}:{netmask}::eth0:off`
///
/// This tells the guest kernel to configure `eth0` with the given IP,
/// using the host IP as the default gateway. The `off` at the end
/// disables DHCP/autoconf.
pub fn guest_boot_ip_arg(guest_ip: std::net::Ipv4Addr, host_ip: std::net::Ipv4Addr) -> String {
    format!("ip={guest_ip}::{host_ip}:255.255.255.252::eth0:off")
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::net::Ipv4Addr;

    use super::*;

    #[test]
    fn recover_orphan_network_removes_nat_rules_then_deletes_taps() {
        let allow = vec![LanAllow {
            cidr: "192.168.105.168/32".to_owned(),
            port: 8000,
        }];
        let taps = vec!["hyphae-tap0".to_string(), "hyphae-tap3".to_string()];

        let nat_calls: RefCell<Vec<(String, Ipv4Addr)>> = RefCell::new(Vec::new());
        let tap_deletes: RefCell<Vec<String>> = RefCell::new(Vec::new());
        let order: RefCell<Vec<String>> = RefCell::new(Vec::new());

        let remove_nat = |tap: &str, guest_ip: Ipv4Addr| {
            nat_calls.borrow_mut().push((tap.to_owned(), guest_ip));
            order.borrow_mut().push(tap.to_owned());
            true
        };
        let delete_tap = |tap: &str| -> Result<(), NetworkError> {
            tap_deletes.borrow_mut().push(tap.to_owned());
            order.borrow_mut().push(tap.to_owned());
            Ok(())
        };

        let report = recover_orphan_network_with(&allow, &taps, &remove_nat, &delete_tap);

        // NAT removal uses the guest IP derived from the TAP index
        // (network 172.16.0.0/30 → guest .2; index 3 → 172.16.0.14).
        assert_eq!(
            nat_calls.into_inner(),
            vec![
                ("hyphae-tap0".to_owned(), Ipv4Addr::new(172, 16, 0, 2)),
                ("hyphae-tap3".to_owned(), Ipv4Addr::new(172, 16, 0, 14)),
            ]
        );
        // Both TAPs deleted, and for each orphan NAT removal precedes
        // the TAP deletion (rules reference the interface).
        let order: Vec<String> = order.into_inner();
        assert_eq!(
            order,
            vec!["hyphae-tap0", "hyphae-tap0", "hyphae-tap3", "hyphae-tap3"]
        );
        assert_eq!(
            tap_deletes.into_inner(),
            vec!["hyphae-tap0".to_string(), "hyphae-tap3".to_string()]
        );
        assert_eq!(
            report,
            NetworkRecoveryReport {
                taps_found: 2,
                taps_deleted: 2,
                nat_rule_sets_removed: 2,
            }
        );
    }

    #[test]
    fn recover_orphan_network_skips_nat_for_underivable_index() {
        // "hyphae-tapX" has no numeric suffix: NAT removal is skipped,
        // the TAP is still deleted, and the failure is visible in the report.
        let taps = vec!["hyphae-tapX".to_string()];
        let nat_calls: RefCell<Vec<String>> = RefCell::new(Vec::new());

        let report = recover_orphan_network_with(
            &[],
            &taps,
            &|tap, _| {
                nat_calls.borrow_mut().push(tap.to_owned());
                true
            },
            &|_tap| Ok(()),
        );

        assert!(nat_calls.borrow().is_empty());
        assert_eq!(
            report,
            NetworkRecoveryReport {
                taps_found: 1,
                taps_deleted: 1,
                nat_rule_sets_removed: 0,
            }
        );
    }

    #[test]
    fn recover_orphan_network_counts_nat_failures_and_tap_failures() {
        let taps = vec!["hyphae-tap1".to_string()];
        let report = recover_orphan_network_with(&[], &taps, &|_tap, _ip| false, &|_tap| {
            Err(NetworkError::IpCommandFailed {
                args: "link del".to_owned(),
                message: "mock".to_owned(),
            })
        });
        assert_eq!(
            report,
            NetworkRecoveryReport {
                taps_found: 1,
                taps_deleted: 0,
                nat_rule_sets_removed: 0,
            }
        );
    }
}
