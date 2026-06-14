//! KVM availability check.

use std::fs::File;
use std::io;

use hyphae_errors::PrereqError;

use super::PrereqCheck;

/// Checks that `/dev/kvm` exists and is accessible.
pub struct KvmCheck;

impl PrereqCheck for KvmCheck {
    fn name(&self) -> &str {
        "/dev/kvm"
    }

    fn check(&self) -> Result<(), PrereqError> {
        match File::open("/dev/kvm") {
            Ok(_) => Ok(()),
            Err(e) => match e.kind() {
                io::ErrorKind::NotFound => Err(PrereqError::KvmNotPresent),
                io::ErrorKind::PermissionDenied => Err(PrereqError::KvmPermissionDenied),
                _ => Err(PrereqError::KvmNotAccessible {
                    reason: e.to_string(),
                }),
            },
        }
    }
}
