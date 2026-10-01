//! Subnet allocation for VM networking.
//!
//! Assigns /30 subnets from the 172.16.0.0/16 range. Each /30 provides four
//! addresses: network (base), host IP (base+1), guest IP (base+2), and
//! broadcast (base+3). With a /16 range, there are 16,384 possible /30 subnets.

use std::collections::HashSet;
use std::net::Ipv4Addr;

use hyphae_errors::NetworkError;

/// The base address of the allocation range (172.16.0.0).
const BASE_ADDR: u32 = 0xAC10_0000; // 172.16.0.0

/// Maximum number of /30 subnets in a /16 range.
const MAX_SUBNETS: u32 = 16_384; // 65536 / 4

/// A single /30 subnet allocation for a VM.
pub struct SubnetAllocation {
    /// The allocation index (0..16384).
    pub index: u32,
    /// Host-side IP (base + 1).
    pub host_ip: Ipv4Addr,
    /// Guest-side IP (base + 2).
    pub guest_ip: Ipv4Addr,
    /// Unique MAC address derived from the index.
    pub mac: String,
    /// TAP device name (e.g. `hyphae-tap0`).
    pub tap_name: String,
}

/// Allocates /30 subnets from 172.16.0.0/16.
///
/// Uses lowest-free-index strategy via a `HashSet`. On construction, scans
/// `/sys/class/net/` for existing `hyphae-tapN` devices to avoid collisions
/// with orphaned TAP devices from previous crashes.
pub struct SubnetAllocator {
    allocated: HashSet<u32>,
}

impl SubnetAllocator {
    /// Create a new allocator, scanning for existing `hyphae-tap*` devices.
    pub fn new() -> Self {
        let mut allocated = HashSet::new();

        // Scan for existing hyphae-tapN interfaces to avoid collisions.
        if let Ok(entries) = std::fs::read_dir("/sys/class/net/") {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if let Some(idx_str) = name.strip_prefix("hyphae-tap") {
                    if let Ok(idx) = idx_str.parse::<u32>() {
                        if idx < MAX_SUBNETS {
                            allocated.insert(idx);
                        }
                    }
                }
            }
        }

        SubnetAllocator { allocated }
    }

    /// Allocate the next available /30 subnet.
    ///
    /// Returns the lowest unused index. Returns `SubnetExhausted` if all
    /// 16,384 subnets are in use.
    pub fn allocate(&mut self) -> Result<SubnetAllocation, NetworkError> {
        let index = (0..MAX_SUBNETS)
            .find(|i| !self.allocated.contains(i))
            .ok_or(NetworkError::SubnetExhausted)?;

        self.allocated.insert(index);

        let guest_ip =
            guest_ip_for_index(index).expect("index was validated against MAX_SUBNETS above");
        let host_ip = Ipv4Addr::from(u32::from(guest_ip) - 1);
        let mac = generate_mac(index)?;
        let tap_name = format!("hyphae-tap{index}");

        Ok(SubnetAllocation {
            index,
            host_ip,
            guest_ip,
            mac,
            tap_name,
        })
    }

    /// Release a previously allocated index back to the pool.
    pub fn release(&mut self, index: u32) {
        self.allocated.remove(&index);
    }

    /// Check whether a given index is currently allocated.
    pub fn is_allocated(&self, index: u32) -> bool {
        self.allocated.contains(&index)
    }
}

/// Derive the guest IP (network + 2) for a /30 allocation index.
///
/// Mirrors the address math in [`SubnetAllocator::allocate`] so callers
/// that only know an orphaned TAP device name (`hyphae-tapN`) can
/// reconstruct the per-VM rule set. Returns `None` for out-of-range
/// indices.
pub fn guest_ip_for_index(index: u32) -> Option<Ipv4Addr> {
    if index >= MAX_SUBNETS {
        return None;
    }
    Some(Ipv4Addr::from(BASE_ADDR + (index * 4) + 2))
}

/// Generate a unique MAC address from a TAP index.
///
/// Format: `AA:FC:00:xx:xx:xx` where `xx:xx:xx` is the 24-bit index.
/// The `AA` prefix has the locally-administered bit set (bit 1 of first octet),
/// ensuring no conflict with globally-assigned MACs.
pub fn generate_mac(index: u32) -> Result<String, NetworkError> {
    if index > 0x00FF_FFFF {
        return Err(NetworkError::MacAddressOverflow { index });
    }
    let b1 = ((index >> 16) & 0xFF) as u8;
    let b2 = ((index >> 8) & 0xFF) as u8;
    let b3 = (index & 0xFF) as u8;
    Ok(format!("AA:FC:00:{b1:02X}:{b2:02X}:{b3:02X}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_mac_index_zero() {
        let mac = generate_mac(0).unwrap();
        assert_eq!(mac, "AA:FC:00:00:00:00");
    }

    #[test]
    fn test_generate_mac_index_one() {
        let mac = generate_mac(1).unwrap();
        assert_eq!(mac, "AA:FC:00:00:00:01");
    }

    #[test]
    fn test_generate_mac_index_256() {
        let mac = generate_mac(256).unwrap();
        assert_eq!(mac, "AA:FC:00:00:01:00");
    }

    #[test]
    fn test_generate_mac_max_valid() {
        let mac = generate_mac(0x00FF_FFFF).unwrap();
        assert_eq!(mac, "AA:FC:00:FF:FF:FF");
    }

    #[test]
    fn test_generate_mac_overflow() {
        let result = generate_mac(0x0100_0000);
        assert!(result.is_err());
    }

    #[test]
    fn test_allocator_sequential() {
        let mut alloc = SubnetAllocator {
            allocated: HashSet::new(),
        };

        let a0 = alloc.allocate().unwrap();
        assert_eq!(a0.index, 0);
        assert_eq!(a0.host_ip, Ipv4Addr::new(172, 16, 0, 1));
        assert_eq!(a0.guest_ip, Ipv4Addr::new(172, 16, 0, 2));
        assert_eq!(a0.tap_name, "hyphae-tap0");
        assert_eq!(a0.mac, "AA:FC:00:00:00:00");

        let a1 = alloc.allocate().unwrap();
        assert_eq!(a1.index, 1);
        assert_eq!(a1.host_ip, Ipv4Addr::new(172, 16, 0, 5));
        assert_eq!(a1.guest_ip, Ipv4Addr::new(172, 16, 0, 6));
        assert_eq!(a1.tap_name, "hyphae-tap1");
    }

    #[test]
    fn test_allocator_reuse_freed() {
        let mut alloc = SubnetAllocator {
            allocated: HashSet::new(),
        };

        let a0 = alloc.allocate().unwrap();
        let _a1 = alloc.allocate().unwrap();
        let _a2 = alloc.allocate().unwrap();

        // Free index 0
        alloc.release(a0.index);

        // Next allocation should reuse index 0 (lowest-free)
        let a3 = alloc.allocate().unwrap();
        assert_eq!(a3.index, 0);
    }

    #[test]
    fn test_allocator_exhaustion() {
        let mut alloc = SubnetAllocator {
            allocated: (0..MAX_SUBNETS).collect(),
        };
        let result = alloc.allocate();
        assert!(result.is_err());
    }

    #[test]
    fn test_is_allocated() {
        let mut alloc = SubnetAllocator {
            allocated: HashSet::new(),
        };
        assert!(!alloc.is_allocated(0));
        let _ = alloc.allocate().unwrap();
        assert!(alloc.is_allocated(0));
        alloc.release(0);
        assert!(!alloc.is_allocated(0));
    }
}
