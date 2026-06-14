//! Prerequisite check system for validating runtime dependencies.
//!
//! Provides a trait-based check system where each prerequisite implements
//! [`PrereqCheck`]. Use [`default_checks`] to get the standard set and
//! [`run_all_checks`] to execute them without short-circuiting.

pub mod firecracker;
pub mod kvm;
pub mod mkfs;

use hyphae_errors::PrereqError;

/// Result of a single prerequisite check.
pub struct CheckResult {
    /// Human-readable name of the check.
    pub name: String,
    /// Whether the check passed.
    pub passed: bool,
    /// Display message: "OK: {name}" or "FAIL: {error}".
    pub message: String,
}

/// Trait for prerequisite checks. Implement this to add new checks.
pub trait PrereqCheck {
    /// Human-readable name of what is being checked.
    fn name(&self) -> &str;

    /// Run the check. Returns `Ok(())` if passed, `Err` with details if failed.
    fn check(&self) -> Result<(), PrereqError>;
}

/// Run all prerequisite checks, collecting all results (no short-circuit).
pub fn run_all_checks(checks: &[Box<dyn PrereqCheck>]) -> Vec<CheckResult> {
    checks
        .iter()
        .map(|c| match c.check() {
            Ok(()) => CheckResult {
                name: c.name().to_string(),
                passed: true,
                message: format!("OK: {}", c.name()),
            },
            Err(e) => CheckResult {
                name: c.name().to_string(),
                passed: false,
                message: format!("FAIL: {e}"),
            },
        })
        .collect()
}

/// Returns the default set of prerequisite checks.
pub fn default_checks() -> Vec<Box<dyn PrereqCheck>> {
    vec![
        Box::new(kvm::KvmCheck),
        Box::new(firecracker::FirecrackerCheck),
        Box::new(mkfs::MkfsCheck),
    ]
}
