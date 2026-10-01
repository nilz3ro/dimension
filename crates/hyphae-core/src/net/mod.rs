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

pub use ip_alloc::{generate_mac, SubnetAllocation, SubnetAllocator};
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
pub fn recover_orphan_taps() {
    let entries = match std::fs::read_dir("/sys/class/net/") {
        Ok(entries) => entries,
        Err(e) => {
            warn!(error = %e, "cannot scan /sys/class/net/ for orphan TAP devices");
            return;
        }
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("hyphae-tap") {
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
