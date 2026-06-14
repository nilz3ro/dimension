//! mkfs.ext4 binary availability check.

use hyphae_errors::PrereqError;

use super::PrereqCheck;

/// Checks that the `mkfs.ext4` binary is available in `PATH`.
pub struct MkfsCheck;

impl PrereqCheck for MkfsCheck {
    fn name(&self) -> &str {
        "mkfs.ext4"
    }

    fn check(&self) -> Result<(), PrereqError> {
        which::which("mkfs.ext4")
            .map(|_| ())
            .map_err(|_| PrereqError::MkfsNotFound)
    }
}
