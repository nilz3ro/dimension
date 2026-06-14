//! iptables NAT rule management for VM networking.
//!
//! Manages per-VM masquerade and forwarding rules via `iptables` commands.
//! A shared conntrack rule for ESTABLISHED/RELATED connections is added once
//! and never removed by individual VM cleanup.

use std::net::Ipv4Addr;
use std::process::Command;

use hyphae_errors::NetworkError;
use tracing::{info, trace};

/// Private/LAN CIDR ranges to block in FORWARD.
///
/// VMs can reach the internet via masquerade, but traffic to these
/// destinations is dropped — preventing lateral movement to the LAN,
/// other VMs, or link-local services.
const LAN_CIDRS: &[&str] = &[
    "10.0.0.0/8",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "169.254.0.0/16",
];

/// Per-VM iptables NAT rules with LAN isolation and cleanup on removal.
///
/// On creation, adds POSTROUTING masquerade, FORWARD DROP rules for
/// private/LAN ranges, and a FORWARD ACCEPT for internet-bound traffic.
/// `remove()` cleans up all per-VM rules without touching the shared
/// conntrack rule.
pub struct NatRules {
    tap_name: String,
    guest_ip: Ipv4Addr,
}

impl NatRules {
    /// Add NAT + LAN-isolation rules for a VM's TAP device.
    ///
    /// 1. Enables IP forwarding via `/proc/sys/net/ipv4/ip_forward`
    /// 2. Adds POSTROUTING masquerade rule for the guest subnet
    /// 3. Adds FORWARD DROP rules for private/LAN ranges (per-TAP)
    /// 4. Adds FORWARD ACCEPT rule for the TAP interface (internet)
    /// 5. Adds shared conntrack ESTABLISHED/RELATED rule (idempotent)
    ///
    /// Rule ordering in FORWARD chain (per VM):
    /// ```text
    ///   DROP  -i tapN -d 10.0.0.0/8
    ///   DROP  -i tapN -d 172.16.0.0/12
    ///   DROP  -i tapN -d 192.168.0.0/16
    ///   DROP  -i tapN -d 169.254.0.0/16
    ///   ACCEPT -i tapN                    ← only internet survives
    ///   ACCEPT -m conntrack --ctstate RELATED,ESTABLISHED
    /// ```
    pub fn add(tap_name: &str, guest_ip: Ipv4Addr) -> Result<Self, NetworkError> {
        // Enable IP forwarding
        std::fs::write("/proc/sys/net/ipv4/ip_forward", "1")
            .map_err(|e| NetworkError::IpForwardFailed(e.to_string()))?;

        // POSTROUTING masquerade rule (idempotent)
        let masq_rule = format!("-s {guest_ip}/30 -j MASQUERADE");
        if !iptables_rule_exists("nat", "POSTROUTING", &masq_rule) {
            run_iptables(&[
                "-t", "nat", "-A", "POSTROUTING", "-s",
                &format!("{guest_ip}/30"), "-j", "MASQUERADE",
            ])?;
        }

        // ── LAN isolation: DROP private ranges before ACCEPT ───────────
        // These rules are per-TAP, so each VM gets its own set.
        // Order matters: DROP rules must precede the ACCEPT for the same TAP.
        for cidr in LAN_CIDRS {
            let drop_rule = format!("-i {tap_name} -d {cidr} -j DROP");
            if !iptables_rule_exists("filter", "FORWARD", &drop_rule) {
                run_iptables(&[
                    "-A", "FORWARD", "-i", tap_name, "-d", cidr, "-j", "DROP",
                ])?;
            }
        }

        // FORWARD accept rule for TAP interface — only internet-bound
        // traffic reaches this rule (private ranges already dropped above).
        let fwd_rule = format!("-i {tap_name} -j ACCEPT");
        if !iptables_rule_exists("filter", "FORWARD", &fwd_rule) {
            run_iptables(&["-A", "FORWARD", "-i", tap_name, "-j", "ACCEPT"])?;
        }

        // Shared conntrack rule for ESTABLISHED/RELATED (idempotent)
        let ct_rule = "-m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT";
        if !iptables_rule_exists("filter", "FORWARD", ct_rule) {
            run_iptables(&[
                "-A", "FORWARD", "-m", "conntrack", "--ctstate",
                "RELATED,ESTABLISHED", "-j", "ACCEPT",
            ])?;
        }

        info!(tap = tap_name, guest_ip = %guest_ip, "NAT + LAN isolation rules added");

        Ok(NatRules {
            tap_name: tap_name.to_owned(),
            guest_ip,
        })
    }

    /// Remove per-VM NAT + LAN-isolation rules.
    ///
    /// Deletes the masquerade, LAN DROP, and FORWARD ACCEPT rules specific
    /// to this VM. Does NOT remove the shared conntrack ESTABLISHED/RELATED
    /// rule, as other VMs may depend on it.
    pub fn remove(&self) -> Result<(), NetworkError> {
        // Delete POSTROUTING masquerade rule (ignore if already gone)
        let _ = run_iptables(&[
            "-t", "nat", "-D", "POSTROUTING", "-s",
            &format!("{}/30", self.guest_ip), "-j", "MASQUERADE",
        ]);

        // Delete LAN isolation DROP rules (ignore if already gone)
        for cidr in LAN_CIDRS {
            let _ = run_iptables(&[
                "-D", "FORWARD", "-i", &self.tap_name, "-d", cidr, "-j", "DROP",
            ]);
        }

        // Delete FORWARD accept rule (ignore if already gone)
        let _ = run_iptables(&[
            "-D", "FORWARD", "-i", &self.tap_name, "-j", "ACCEPT",
        ]);

        trace!(tap = %self.tap_name, "NAT + LAN isolation rules removed");

        Ok(())
    }
}

/// Run an `iptables` command with the given arguments.
fn run_iptables(args: &[&str]) -> Result<(), NetworkError> {
    let output = Command::new("iptables")
        .args(args)
        .output()
        .map_err(|e| NetworkError::IptablesFailed(format!("iptables {}: {e}", args.join(" "))))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);

    if stderr.contains("Operation not permitted")
        || stderr.contains("Permission denied")
    {
        return Err(NetworkError::InsufficientPrivileges {
            operation: format!("iptables {}", args.join(" ")),
            hint: "Run with sudo or set CAP_NET_ADMIN capability".to_owned(),
        });
    }

    Err(NetworkError::IptablesFailed(format!(
        "iptables {} failed: {}",
        args.join(" "),
        stderr.trim()
    )))
}

/// Check if an iptables rule already exists in the given table and chain.
///
/// Uses `iptables -t {table} -C {chain} {rule}` to test for existence.
/// Returns `false` on any error (including permission errors) to allow
/// the subsequent append to produce the real error.
fn iptables_rule_exists(table: &str, chain: &str, rule: &str) -> bool {
    // Split rule string into args for the -C (check) command
    let rule_args: Vec<&str> = rule.split_whitespace().collect();
    let mut args = vec!["-t", table, "-C", chain];
    args.extend_from_slice(&rule_args);

    Command::new("iptables")
        .args(&args)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
