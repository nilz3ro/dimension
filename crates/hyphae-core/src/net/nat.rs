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

/// A destination-scoped exception to the LAN isolation rules.
///
/// Guests may reach exactly one TCP port on one destination CIDR,
/// expressed as `CIDR:PORT` (e.g. `192.168.105.168/32:8000`).
/// The ACCEPT rule is inserted before the LAN DROP rules, so every
/// other private-range destination remains blocked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanAllow {
    /// Destination IPv4 CIDR (e.g. `192.168.105.168/32`).
    pub cidr: String,
    /// Destination TCP port.
    pub port: u16,
}

impl LanAllow {
    /// Parse a `CIDR:PORT` specification.
    ///
    /// Fails closed on anything that is not a valid IPv4 CIDR followed by
    /// a non-zero TCP port, so configuration typos cannot silently widen
    /// guest egress.
    pub fn parse(spec: &str) -> Result<Self, NetworkError> {
        let (cidr, port) = spec.rsplit_once(':').ok_or_else(|| {
            NetworkError::InvalidLanAllow(format!(
                "missing ':PORT' suffix in {spec:?} (expected CIDR:PORT, e.g. 192.168.105.168/32:8000)"
            ))
        })?;

        let (addr, prefix) = cidr.split_once('/').ok_or_else(|| {
            NetworkError::InvalidLanAllow(format!(
                "destination {cidr:?} is not a CIDR (expected e.g. 192.168.105.168/32)"
            ))
        })?;
        let addr: Ipv4Addr = addr.parse().map_err(|_| {
            NetworkError::InvalidLanAllow(format!("invalid IPv4 address in {spec:?}"))
        })?;
        let prefix: u32 = prefix.parse().map_err(|_| {
            NetworkError::InvalidLanAllow(format!("invalid prefix length in {spec:?}"))
        })?;
        if prefix > 32 {
            return Err(NetworkError::InvalidLanAllow(format!(
                "prefix length /{prefix} exceeds /32 in {spec:?}"
            )));
        }

        let port: u16 = port.parse().map_err(|_| {
            NetworkError::InvalidLanAllow(format!("invalid TCP port in {spec:?}"))
        })?;
        if port == 0 {
            return Err(NetworkError::InvalidLanAllow(format!(
                "port 0 is not a valid destination port in {spec:?}"
            )));
        }

        // Normalize the address so the stored CIDR is canonical
        // (host bits cleared), keeping iptables rule matching exact.
        let host_bits = 32 - prefix;
        let mask = if prefix == 0 { 0 } else { u32::MAX << host_bits };
        let network = u32::from(addr) & mask;
        let canonical = Ipv4Addr::from(network);

        Ok(LanAllow {
            cidr: format!("{canonical}/{prefix}"),
            port,
        })
    }

    /// The iptables FORWARD ACCEPT rule for this entry on the given TAP.
    fn accept_rule(&self, tap_name: &str) -> String {
        format!(
            "-i {tap_name} -d {} -p tcp --dport {} -j ACCEPT",
            self.cidr, self.port
        )
    }
}

/// Per-VM iptables NAT rules with LAN isolation and cleanup on removal.
///
/// On creation, adds POSTROUTING masquerade, per-destination ACCEPT rules
/// for the configured [`LanAllow`] allowlist, FORWARD DROP rules for
/// private/LAN ranges, and a FORWARD ACCEPT for internet-bound traffic.
/// `remove()` cleans up all per-VM rules without touching the shared
/// conntrack rule.
pub struct NatRules {
    tap_name: String,
    guest_ip: Ipv4Addr,
    /// Destination-scoped allowlist this rule set granted (mirrored on
    /// removal so exactly the rules that were added get deleted).
    allow: Vec<LanAllow>,
}

impl NatRules {
    /// Add NAT + LAN-isolation rules for a VM's TAP device.
    ///
    /// 1. Enables IP forwarding via `/proc/sys/net/ipv4/ip_forward`
    /// 2. Adds POSTROUTING masquerade rule for the guest subnet
    /// 3. Adds FORWARD ACCEPT for allowlisted LAN destinations (per-TAP)
    /// 4. Adds FORWARD DROP rules for private/LAN ranges (per-TAP)
    /// 5. Adds FORWARD ACCEPT rule for the TAP interface (internet)
    /// 6. Adds shared conntrack ESTABLISHED/RELATED rule (idempotent)
    ///
    /// Rule ordering in FORWARD chain (per VM):
    /// ```text
    ///   ACCEPT -i tapN -d <allow cidr> -p tcp --dport <port>  ← scoped exceptions
    ///   DROP  -i tapN -d 10.0.0.0/8
    ///   DROP  -i tapN -d 172.16.0.0/12
    ///   DROP  -i tapN -d 192.168.0.0/16
    ///   DROP  -i tapN -d 169.254.0.0/16
    ///   ACCEPT -i tapN                    ← only internet survives
    ///   ACCEPT -m conntrack --ctstate RELATED,ESTABLISHED
    /// ```
    ///
    /// The allowlist ACCEPT rules MUST precede the LAN DROP rules so a
    /// scoped exception is visible before the covering RFC1918 drop.
    /// Both rule sets are per-TAP and appended together, so ordering is
    /// preserved as long as the rules for a TAP are added in one pass.
    pub fn add(
        tap_name: &str,
        guest_ip: Ipv4Addr,
        allow: &[LanAllow],
    ) -> Result<Self, NetworkError> {
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

        // ── Scoped exceptions: allowlist BEFORE the LAN DROPs ──────────
        // These rules are per-TAP and destination-scoped (one TCP port on
        // one CIDR). Everything else in the private ranges stays dropped.
        for entry in allow {
            let rule = entry.accept_rule(tap_name);
            if !iptables_rule_exists("filter", "FORWARD", &rule) {
                run_iptables(&[
                    "-A", "FORWARD", "-i", tap_name, "-d", &entry.cidr, "-p", "tcp",
                    "--dport", &entry.port.to_string(), "-j", "ACCEPT",
                ])?;
            }
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

        info!(
            tap = tap_name,
            guest_ip = %guest_ip,
            lan_allow = allow.len(),
            "NAT + LAN isolation rules added"
        );

        Ok(NatRules {
            tap_name: tap_name.to_owned(),
            guest_ip,
            allow: allow.to_vec(),
        })
    }

    /// Remove per-VM NAT + LAN-isolation rules.
    ///
    /// Deletes the masquerade, scoped allowlist ACCEPT, LAN DROP, and
    /// FORWARD ACCEPT rules specific to this VM. Does NOT remove the shared
    /// conntrack ESTABLISHED/RELATED rule, as other VMs may depend on it.
    pub fn remove(&self) -> Result<(), NetworkError> {
        // Delete POSTROUTING masquerade rule (ignore if already gone)
        let _ = run_iptables(&[
            "-t", "nat", "-D", "POSTROUTING", "-s",
            &format!("{}/30", self.guest_ip), "-j", "MASQUERADE",
        ]);

        // Delete scoped allowlist ACCEPT rules (ignore if already gone)
        for entry in &self.allow {
            let _ = run_iptables(&[
                "-D", "FORWARD", "-i", &self.tap_name, "-d", &entry.cidr,
                "-p", "tcp", "--dport", &entry.port.to_string(), "-j", "ACCEPT",
            ]);
        }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_valid_scoped_destination() {
        let entry = LanAllow::parse("192.168.105.168/32:8000").unwrap();
        assert_eq!(entry.cidr, "192.168.105.168/32");
        assert_eq!(entry.port, 8000);
    }

    #[test]
    fn parse_canonicalizes_host_bits() {
        // Host bits must be cleared so the iptables rule matches the
        // network exactly instead of relying on iptables' own masking.
        let entry = LanAllow::parse("192.168.105.199/24:443").unwrap();
        assert_eq!(entry.cidr, "192.168.105.0/24");
        assert_eq!(entry.port, 443);
    }

    #[test]
    fn parse_rejects_missing_port() {
        assert!(LanAllow::parse("192.168.105.168/32").is_err());
        assert!(LanAllow::parse("192.168.105.168/32:").is_err());
    }

    #[test]
    fn parse_rejects_bare_address_without_cidr() {
        assert!(LanAllow::parse("192.168.105.168:8000").is_err());
    }

    #[test]
    fn parse_rejects_invalid_input() {
        // Not an IPv4 address
        assert!(LanAllow::parse("gpu-boi/32:8000").is_err());
        // Prefix out of range
        assert!(LanAllow::parse("192.168.105.168/33:8000").is_err());
        // Port out of range
        assert!(LanAllow::parse("192.168.105.168/32:99999").is_err());
        // Port zero
        assert!(LanAllow::parse("192.168.105.168/32:0").is_err());
        // IPv6-style address
        assert!(LanAllow::parse("::1/128:8000").is_err());
        // Extra junk
        assert!(LanAllow::parse("192.168.105.168/32:8000/udp").is_err());
    }

    #[test]
    fn accept_rule_is_tcp_and_destination_scoped() {
        let entry = LanAllow::parse("192.168.105.168/32:8000").unwrap();
        assert_eq!(
            entry.accept_rule("hyphae-tap0"),
            "-i hyphae-tap0 -d 192.168.105.168/32 -p tcp --dport 8000 -j ACCEPT"
        );
    }
}
