//! iptables NAT rule management for VM networking.
//!
//! Manages per-VM masquerade and forwarding rules via `iptables` commands.
//! A shared conntrack rule for ESTABLISHED/RELATED connections is added once
//! and never removed by individual VM cleanup.

use std::net::Ipv4Addr;
use std::process::Command;

use hyphae_errors::NetworkError;
use tracing::{info, trace, warn};

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

        let port: u16 = port
            .parse()
            .map_err(|_| NetworkError::InvalidLanAllow(format!("invalid TCP port in {spec:?}")))?;
        if port == 0 {
            return Err(NetworkError::InvalidLanAllow(format!(
                "port 0 is not a valid destination port in {spec:?}"
            )));
        }

        // A CIDR with host bits set (e.g. 192.168.105.199/24) is rejected
        // rather than silently widened to the network (192.168.105.0/24):
        // a typo in the allowlist must fail closed instead of quietly
        // broadening guest egress to the whole /24.
        let host_bits = 32 - prefix;
        let mask = if prefix == 0 {
            0
        } else {
            u32::MAX << host_bits
        };
        if u32::from(addr) & mask != u32::from(addr) {
            let network = Ipv4Addr::from(u32::from(addr) & mask);
            return Err(NetworkError::InvalidLanAllow(format!(
                "CIDR {cidr:?} in {spec:?} has host bits set; refusing to widen \
                 egress — write the exact network (e.g. {network}/{prefix})"
            )));
        }

        Ok(LanAllow {
            cidr: format!("{addr}/{prefix}"),
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
#[derive(Debug)]
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
    /// 1. Ensures IP forwarding is enabled — reads
    ///    `/proc/sys/net/ipv4/ip_forward` first and only writes when it is
    ///    currently disabled (hardened worker units mount `/proc/sys`
    ///    read-only; hosts with forwarding already on must keep working)
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
    ///
    /// If any step fails, every rule successfully added by *this* call is
    /// rolled back in reverse order, so a partial setup cannot leave a
    /// stale ACCEPT (or any other rule) behind to affect a future TAP
    /// that reuses the same name. Pre-existing rules are never touched.
    pub fn add(
        tap_name: &str,
        guest_ip: Ipv4Addr,
        allow: &[LanAllow],
    ) -> Result<Self, NetworkError> {
        Self::add_with(&SystemBackend, tap_name, guest_ip, allow)
    }

    /// Backend-injectable core of [`NatRules::add`], so the setup and
    /// rollback logic is unit-testable without root or iptables.
    fn add_with(
        backend: &dyn IptablesBackend,
        tap_name: &str,
        guest_ip: Ipv4Addr,
        allow: &[LanAllow],
    ) -> Result<Self, NetworkError> {
        // Ensure IP forwarding — read first, write only when disabled.
        // Hosts running the worker under a hardened systemd unit
        // (ProtectKernelTunables=yes) mount /proc/sys read-only, so an
        // unconditional write fails even though forwarding is already
        // enabled. Deployment persists `net.ipv4.ip_forward=1` outside
        // the worker unit; the worker only needs the write as a fallback.
        let forwarding = backend.read_ip_forward().map_err(|e| {
            NetworkError::IpForwardFailed(format!(
                "failed to read /proc/sys/net/ipv4/ip_forward: {e}"
            ))
        })?;
        if forwarding.trim() != "1" {
            backend.write_ip_forward("1").map_err(|e| {
                NetworkError::IpForwardFailed(format!(
                    "ip_forward is disabled and could not be enabled: {e}"
                ))
            })?;
        }

        // Rules actually installed by this call (excluding pre-existing
        // ones), rolled back in reverse order if any later step fails.
        // Stored as owned strings because argument slices borrow from
        // per-rule temporaries (e.g. the formatted guest subnet).
        let mut added: Vec<Vec<String>> = Vec::new();

        // Install one rule if it does not already exist; record it for
        // rollback when newly added.
        fn install(
            backend: &dyn IptablesBackend,
            added: &mut Vec<Vec<String>>,
            table: &str,
            chain: &str,
            spec: &str,
            args: Vec<&str>,
        ) -> Result<(), NetworkError> {
            if !backend.rule_exists(table, chain, spec) {
                backend.run(&args)?;
                added.push(args.iter().map(|s| s.to_string()).collect());
            }
            Ok(())
        }

        let mut setup = || -> Result<(), NetworkError> {
            // POSTROUTING masquerade rule (idempotent)
            install(
                backend,
                &mut added,
                "nat",
                "POSTROUTING",
                &format!("-s {guest_ip}/30 -j MASQUERADE"),
                vec![
                    "-t",
                    "nat",
                    "-A",
                    "POSTROUTING",
                    "-s",
                    &format!("{guest_ip}/30"),
                    "-j",
                    "MASQUERADE",
                ],
            )?;

            // ── Scoped exceptions: allowlist BEFORE the LAN DROPs ──────
            // These rules are per-TAP and destination-scoped (one TCP
            // port on one CIDR). Everything else in the private ranges
            // stays dropped.
            for entry in allow {
                install(
                    backend,
                    &mut added,
                    "filter",
                    "FORWARD",
                    &entry.accept_rule(tap_name),
                    vec![
                        "-A",
                        "FORWARD",
                        "-i",
                        tap_name,
                        "-d",
                        &entry.cidr,
                        "-p",
                        "tcp",
                        "--dport",
                        &entry.port.to_string(),
                        "-j",
                        "ACCEPT",
                    ],
                )?;
            }

            // ── LAN isolation: DROP private ranges before ACCEPT ───────
            // These rules are per-TAP, so each VM gets its own set.
            // Order matters: DROP rules must precede the ACCEPT for the
            // same TAP.
            for cidr in LAN_CIDRS {
                install(
                    backend,
                    &mut added,
                    "filter",
                    "FORWARD",
                    &format!("-i {tap_name} -d {cidr} -j DROP"),
                    vec!["-A", "FORWARD", "-i", tap_name, "-d", cidr, "-j", "DROP"],
                )?;
            }

            // FORWARD accept rule for TAP interface — only internet-bound
            // traffic reaches this rule (private ranges already dropped
            // above).
            install(
                backend,
                &mut added,
                "filter",
                "FORWARD",
                &format!("-i {tap_name} -j ACCEPT"),
                vec!["-A", "FORWARD", "-i", tap_name, "-j", "ACCEPT"],
            )?;

            // Shared conntrack rule for ESTABLISHED/RELATED (idempotent)
            install(
                backend,
                &mut added,
                "filter",
                "FORWARD",
                "-m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT",
                vec![
                    "-A",
                    "FORWARD",
                    "-m",
                    "conntrack",
                    "--ctstate",
                    "RELATED,ESTABLISHED",
                    "-j",
                    "ACCEPT",
                ],
            )?;

            Ok(())
        };

        if let Err(err) = setup() {
            // Roll back every rule this call added, in reverse order.
            // Rollback failures are logged but do not mask the original
            // error; each delete maps the add's "-A" to "-D" and leaves
            // pre-existing rules (including ones another setup installed)
            // untouched.
            for args in added.iter().rev() {
                let delete: Vec<&str> = args
                    .iter()
                    .map(|a| if a == "-A" { "-D" } else { a.as_str() })
                    .collect();
                if let Err(rollback_err) = backend.run(&delete) {
                    warn!(
                        rule = delete.join(" "),
                        error = %rollback_err,
                        "failed to roll back rule after NAT setup failure"
                    );
                }
            }
            return Err(err);
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
            "-t",
            "nat",
            "-D",
            "POSTROUTING",
            "-s",
            &format!("{}/30", self.guest_ip),
            "-j",
            "MASQUERADE",
        ]);

        // Delete scoped allowlist ACCEPT rules (ignore if already gone)
        for entry in &self.allow {
            let _ = run_iptables(&[
                "-D",
                "FORWARD",
                "-i",
                &self.tap_name,
                "-d",
                &entry.cidr,
                "-p",
                "tcp",
                "--dport",
                &entry.port.to_string(),
                "-j",
                "ACCEPT",
            ]);
        }

        // Delete LAN isolation DROP rules (ignore if already gone)
        for cidr in LAN_CIDRS {
            let _ = run_iptables(&[
                "-D",
                "FORWARD",
                "-i",
                &self.tap_name,
                "-d",
                cidr,
                "-j",
                "DROP",
            ]);
        }

        // Delete FORWARD accept rule (ignore if already gone)
        let _ = run_iptables(&["-D", "FORWARD", "-i", &self.tap_name, "-j", "ACCEPT"]);

        trace!(tap = %self.tap_name, "NAT + LAN isolation rules removed");

        Ok(())
    }
}

/// Execution backend for iptables commands and the ip_forward sysctl.
///
/// Abstracted so [`NatRules::add`] setup and rollback logic can be unit
/// tested with a mock instead of requiring root + iptables.
trait IptablesBackend {
    /// Run an `iptables` command (append/delete) — must fail on nonzero
    /// exit status.
    fn run(&self, args: &[&str]) -> Result<(), NetworkError>;
    /// Check if a rule exists via `iptables -t {table} -C {chain} {rule}`.
    fn rule_exists(&self, table: &str, chain: &str, rule: &str) -> bool;
    /// Read `/proc/sys/net/ipv4/ip_forward`.
    fn read_ip_forward(&self) -> std::io::Result<String>;
    /// Write `/proc/sys/net/ipv4/ip_forward`.
    fn write_ip_forward(&self, value: &str) -> std::io::Result<()>;
}

/// Real [`IptablesBackend`] backed by `Command` and `/proc/sys`.
struct SystemBackend;

impl IptablesBackend for SystemBackend {
    fn run(&self, args: &[&str]) -> Result<(), NetworkError> {
        run_iptables(args)
    }

    fn rule_exists(&self, table: &str, chain: &str, rule: &str) -> bool {
        iptables_rule_exists(table, chain, rule)
    }

    fn read_ip_forward(&self) -> std::io::Result<String> {
        std::fs::read_to_string("/proc/sys/net/ipv4/ip_forward")
    }

    fn write_ip_forward(&self, value: &str) -> std::io::Result<()> {
        std::fs::write("/proc/sys/net/ipv4/ip_forward", value)
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

    if stderr.contains("Operation not permitted") || stderr.contains("Permission denied") {
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
    use std::cell::RefCell;
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn parse_accepts_valid_scoped_destination() {
        let entry = LanAllow::parse("192.168.105.168/32:8000").unwrap();
        assert_eq!(entry.cidr, "192.168.105.168/32");
        assert_eq!(entry.port, 8000);
    }

    #[test]
    fn parse_rejects_host_bits_instead_of_widening() {
        // A CIDR whose address has host bits set must be rejected, not
        // silently widened to the covering network — that would broaden
        // guest egress on a typo.
        let err = LanAllow::parse("192.168.105.199/24:443").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("host bits"), "unexpected error: {msg}");
        assert!(
            msg.contains("192.168.105.0/24"),
            "should name the network: {msg}"
        );
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

    /// Mock iptables backend: records every command, maintains a rule
    /// set, and can be told to fail the first command containing a given
    /// substring.
    struct MockBackend {
        commands: RefCell<Vec<Vec<String>>>,
        rules: RefCell<HashSet<String>>,
        fail_on: RefCell<Option<String>>,
        ip_forward: RefCell<String>,
        ip_forward_writable: bool,
        ip_forward_writes: RefCell<Vec<String>>,
    }

    impl MockBackend {
        fn new(ip_forward: &str) -> Self {
            Self {
                commands: RefCell::new(Vec::new()),
                rules: RefCell::new(HashSet::new()),
                fail_on: RefCell::new(None),
                ip_forward: RefCell::new(ip_forward.to_owned()),
                ip_forward_writable: true,
                ip_forward_writes: RefCell::new(Vec::new()),
            }
        }

        fn fail_on(mut self, needle: &str) -> Self {
            self.fail_on = RefCell::new(Some(needle.to_owned()));
            self
        }

        fn read_only_sysctl(mut self) -> Self {
            self.ip_forward_writable = false;
            self
        }

        /// Key identifying a rule: `table:chain:spec`. The action
        /// (`-A`/`-D`) is normalized away so a delete removes the key its
        /// append inserted.
        fn rule_key(args: &[&str]) -> String {
            let (table, rest): (&str, &[&str]) = if args.first() == Some(&"-t") {
                (args[1], &args[2..])
            } else {
                ("filter", args)
            };
            // Action is -A/-D/-C; chain follows it, spec after that.
            let action_idx = rest
                .iter()
                .position(|a| *a == "-A" || *a == "-D" || *a == "-C")
                .expect("rule args must contain an action");
            let chain = rest[action_idx + 1];
            let spec = rest[action_idx + 2..].join(" ");
            format!("{table}:{chain}:{spec}")
        }

        /// All commands issued as joined strings.
        fn commands(&self) -> Vec<String> {
            self.commands.borrow().iter().map(|c| c.join(" ")).collect()
        }
    }

    impl IptablesBackend for MockBackend {
        fn run(&self, args: &[&str]) -> Result<(), NetworkError> {
            let joined = args.join(" ");
            self.commands
                .borrow_mut()
                .push(args.iter().map(|s| s.to_string()).collect());

            let needle = self.fail_on.borrow().clone();
            if let Some(needle) = needle {
                if joined.contains(needle.as_str()) {
                    // Fire once, so the rollback delete of the same rule
                    // (and any later retry) is not poisoned.
                    self.fail_on.borrow_mut().take();
                    return Err(NetworkError::IptablesFailed(format!(
                        "mock failure on {joined}"
                    )));
                }
            }

            let key = Self::rule_key(args);
            let is_delete = args.contains(&"-D");
            if is_delete {
                self.rules.borrow_mut().remove(&key);
            } else {
                self.rules.borrow_mut().insert(key);
            }
            Ok(())
        }

        fn rule_exists(&self, table: &str, chain: &str, rule: &str) -> bool {
            self.rules
                .borrow()
                .contains(&format!("{table}:{chain}:{rule}"))
        }

        fn read_ip_forward(&self) -> std::io::Result<String> {
            Ok(self.ip_forward.borrow().clone())
        }

        fn write_ip_forward(&self, value: &str) -> std::io::Result<()> {
            if !self.ip_forward_writable {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::ReadOnlyFilesystem,
                    "Read-only file system",
                ));
            }
            self.ip_forward_writes.borrow_mut().push(value.to_owned());
            *self.ip_forward.borrow_mut() = value.to_owned();
            Ok(())
        }
    }

    /// A LAN CIDR DROP rule, one past the first, so the failure exercises
    /// rollback of multiple already-added rules.
    const TAP: &str = "hyphae-tap0";
    const GUEST: Ipv4Addr = Ipv4Addr::new(172, 20, 0, 2);

    fn allowlist() -> Vec<LanAllow> {
        vec![LanAllow {
            cidr: "192.168.105.168/32".to_owned(),
            port: 8000,
        }]
    }

    #[test]
    fn add_rolls_back_installed_rules_in_reverse_order_on_failure() {
        // Fail on the second LAN DROP rule; masquerade, the scoped
        // ACCEPT, and the first DROP were already installed and must be
        // deleted in reverse order with no rule left behind.
        let backend = MockBackend::new("1").fail_on("172.16.0.0/12");
        let err = NatRules::add_with(&backend, TAP, GUEST, &allowlist()).unwrap_err();
        assert!(err.to_string().contains("mock failure"), "got: {err}");

        let cmds = backend.commands();
        // The failing append:
        assert!(
            cmds.iter()
                .any(|c| c.contains(&format!("-A FORWARD -i {TAP} -d 172.16.0.0/12 -j DROP")))
        );

        // Rollback deletes, in reverse order of the successful adds:
        //   1. DROP 10.0.0.0/8
        //   2. scoped ACCEPT (the stale-ACCEPT hazard)
        //   3. POSTROUTING masquerade
        let rollback: Vec<&String> = cmds.iter().filter(|c| c.contains("-D ")).collect();
        assert_eq!(
            rollback.iter().map(|c| c.as_str()).collect::<Vec<&str>>(),
            vec![
                format!("-D FORWARD -i {TAP} -d 10.0.0.0/8 -j DROP"),
                format!("-D FORWARD -i {TAP} -d 192.168.105.168/32 -p tcp --dport 8000 -j ACCEPT"),
                format!("-t nat -D POSTROUTING -s {GUEST}/30 -j MASQUERADE"),
            ],
            "rollback must delete exactly the rules this call added, in reverse order"
        );
        // Masquerade delete keeps the -t nat prefix.
        assert!(cmds.contains(&format!(
            "-t nat -D POSTROUTING -s {GUEST}/30 -j MASQUERADE"
        )));

        // Nothing installed by this call survives the rollback.
        let survivors: Vec<String> = backend
            .rules
            .borrow()
            .iter()
            .filter(|k| k.contains(&format!("-i {TAP}")) || k.contains("POSTROUTING"))
            .cloned()
            .collect();
        assert!(
            survivors.is_empty(),
            "no rules for this TAP (or masquerade) may survive a failed setup: {survivors:?}"
        );
    }

    #[test]
    fn add_failure_leaves_pre_existing_rules_untouched() {
        // A rule that already exists is skipped (not recorded for
        // rollback), so a later failure must not delete it.
        let backend = MockBackend::new("1").fail_on("169.254.0.0/16");
        // Pre-install the masquerade and the scoped ACCEPT, as a previous
        // successful setup would have.
        backend
            .rules
            .borrow_mut()
            .insert("nat:POSTROUTING:-s 172.20.0.2/30 -j MASQUERADE".to_owned());
        let scoped =
            format!("filter:FORWARD:-i {TAP} -d 192.168.105.168/32 -p tcp --dport 8000 -j ACCEPT");
        backend.rules.borrow_mut().insert(scoped);

        let err = NatRules::add_with(&backend, TAP, GUEST, &allowlist()).unwrap_err();
        assert!(err.to_string().contains("mock failure"), "got: {err}");

        // Both pre-existing rules still present after rollback.
        assert!(
            backend
                .rules
                .borrow()
                .contains("nat:POSTROUTING:-s 172.20.0.2/30 -j MASQUERADE")
        );
        assert!(backend.rules.borrow().contains(&format!(
            "filter:FORWARD:-i {TAP} -d 192.168.105.168/32 -p tcp --dport 8000 -j ACCEPT"
        )));
    }

    #[test]
    fn add_succeeds_and_installs_rules_in_order() {
        let backend = MockBackend::new("1");
        let rules = NatRules::add_with(&backend, TAP, GUEST, &allowlist()).unwrap();

        let cmds = backend.commands();
        let scoped_pos = cmds
            .iter()
            .position(|c| {
                c.contains(&format!(
                    "-A FORWARD -i {TAP} -d 192.168.105.168/32 -p tcp --dport 8000 -j ACCEPT"
                ))
            })
            .expect("scoped ACCEPT installed");
        let first_drop_pos = cmds
            .iter()
            .position(|c| c.contains(&format!("-A FORWARD -i {TAP} -d 10.0.0.0/8 -j DROP")))
            .expect("LAN DROP installed");
        assert!(
            scoped_pos < first_drop_pos,
            "scoped ACCEPT must precede LAN DROPs"
        );
        assert_eq!(rules.tap_name, TAP);
        assert_eq!(rules.allow, allowlist());

        // Forwarding already enabled: no sysctl write happened.
        assert!(backend.ip_forward_writes.borrow().is_empty());
    }

    #[test]
    fn add_skips_ip_forward_write_when_already_enabled() {
        // ProtectKernelTunables=yes mounts /proc/sys read-only; when the
        // host already forwards, add() must succeed without writing.
        let backend = MockBackend::new("1").read_only_sysctl();
        NatRules::add_with(&backend, TAP, GUEST, &allowlist()).unwrap();
        assert!(backend.ip_forward_writes.borrow().is_empty());
    }

    #[test]
    fn add_writes_ip_forward_only_when_disabled() {
        let backend = MockBackend::new("0\n");
        NatRules::add_with(&backend, TAP, GUEST, &allowlist()).unwrap();
        let writes = backend.ip_forward_writes.borrow().clone();
        assert_eq!(writes, vec!["1".to_owned()]);
    }

    #[test]
    fn add_fails_when_forwarding_disabled_and_sysctl_read_only() {
        // If forwarding is off and the sysctl cannot be written, fail
        // closed — no NAT rules get installed.
        let backend = MockBackend::new("0").read_only_sysctl();
        let err = NatRules::add_with(&backend, TAP, GUEST, &allowlist()).unwrap_err();
        assert!(err.to_string().contains("ip_forward"), "got: {err}");
        // No iptables commands attempted after the sysctl failure.
        assert!(backend.commands().is_empty());
    }
}
