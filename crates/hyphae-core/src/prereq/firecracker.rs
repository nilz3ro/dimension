//! Firecracker binary availability check.

use hyphae_errors::PrereqError;

use super::PrereqCheck;

/// Checks that the `firecracker` binary is available in `PATH`.
pub struct FirecrackerCheck;

impl PrereqCheck for FirecrackerCheck {
    fn name(&self) -> &str {
        "firecracker"
    }

    fn check(&self) -> Result<(), PrereqError> {
        which::which("firecracker")
            .map(|_| ())
            .map_err(|_| PrereqError::FirecrackerNotFound)
    }
}
