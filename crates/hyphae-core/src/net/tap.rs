//! TAP device RAII management.
//!
//! Creates TAP network interfaces via `ip` commands, assigns host-side /30 IPs,
//! and automatically deletes the device on drop.

use std::net::Ipv4Addr;
use std::process::Command;

use hyphae_errors::NetworkError;
use tracing::{info, trace, warn};

/// A TAP network device with RAII cleanup.
///
/// On creation, runs `ip tuntap add`, assigns an IP, and brings the link up.
/// On drop, runs `ip link del` to remove the device.
pub struct TapDevice {
    name: String,
    host_ip: Ipv4Addr,
    guest_ip: Ipv4Addr,
    prefix_len: u8,
}

impl TapDevice {
    /// Create a new TAP device.
    ///
    /// Runs the following `ip` commands in sequence:
    /// 1. `ip tuntap add {name} mode tap`
    /// 2. `ip addr add {host_ip}/30 dev {name}`
    /// 3. `ip link set {name} up`
    ///
    /// On partial failure, cleans up by deleting the device.
    pub fn create(name: &str, host_ip: Ipv4Addr, guest_ip: Ipv4Addr) -> Result<Self, NetworkError> {
        // Step 1: Create the TAP device
        run_ip_cmd(&["tuntap", "add", name, "mode", "tap"])?;

        // Step 2: Assign host IP (clean up on failure)
        let addr_arg = format!("{host_ip}/30");
        if let Err(e) = run_ip_cmd(&["addr", "add", &addr_arg, "dev", name]) {
            let _ = run_ip_cmd(&["link", "del", name]);
            return Err(e);
        }

        // Step 3: Bring link up (clean up on failure)
        if let Err(e) = run_ip_cmd(&["link", "set", name, "up"]) {
            let _ = run_ip_cmd(&["link", "del", name]);
            return Err(e);
        }

        info!(tap = name, host_ip = %host_ip, guest_ip = %guest_ip, "TAP device created");

        Ok(TapDevice {
            name: name.to_owned(),
            host_ip,
            guest_ip,
            prefix_len: 30,
        })
    }

    /// Returns the TAP device name (e.g. `hyphae-tap0`).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the host-side IP address of the /30 subnet.
    pub fn host_ip(&self) -> Ipv4Addr {
        self.host_ip
    }

    /// Returns the guest-side IP address of the /30 subnet.
    pub fn guest_ip(&self) -> Ipv4Addr {
        self.guest_ip
    }

    /// Returns the subnet prefix length (always 30).
    pub fn prefix_len(&self) -> u8 {
        self.prefix_len
    }
}

impl Drop for TapDevice {
    fn drop(&mut self) {
        match run_ip_cmd(&["link", "del", &self.name]) {
            Ok(()) => {
                trace!(tap = %self.name, "TAP device deleted");
            }
            Err(e) => {
                warn!(tap = %self.name, error = %e, "failed to delete TAP device on drop");
            }
        }
    }
}

/// Run an `ip` command with the given arguments.
///
/// Checks for permission-related errors and returns `InsufficientPrivileges`
/// with actionable guidance. Other failures return `IpCommandFailed`.
pub(crate) fn run_ip_cmd(args: &[&str]) -> Result<(), NetworkError> {
    let args_str = args.join(" ");

    let output = Command::new("ip")
        .args(args)
        .output()
        .map_err(|e| NetworkError::IpCommandFailed {
            args: args_str.clone(),
            message: e.to_string(),
        })?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);

    if stderr.contains("Operation not permitted")
        || stderr.contains("RTNETLINK answers: Operation not permitted")
    {
        return Err(NetworkError::InsufficientPrivileges {
            operation: format!("ip {args_str}"),
            hint: "Run with sudo or set CAP_NET_ADMIN capability".to_owned(),
        });
    }

    Err(NetworkError::IpCommandFailed {
        args: args_str,
        message: stderr.trim().to_owned(),
    })
}
