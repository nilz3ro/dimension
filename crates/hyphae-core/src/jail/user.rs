//! System user validation for jailer privilege dropping.
//!
//! The Firecracker jailer drops privileges to a dedicated system user.
//! This module validates that the `hyphae` user exists and returns its
//! uid/gid for use in jailer configuration.

use hyphae_errors::JailError;

/// Resolved system user identity for jailer privilege dropping.
pub struct JailUser {
    pub uid: u32,
    pub gid: u32,
    pub name: String,
}

/// Look up the `hyphae` system user and return its uid/gid.
///
/// Returns an error with actionable guidance if the user does not exist.
pub fn validate_jail_user() -> Result<JailUser, JailError> {
    let user = nix::unistd::User::from_name("hyphae")
        .map_err(|e| JailError::UserLookupFailed(e.to_string()))?
        .ok_or_else(|| JailError::UserNotFound {
            hint: "Create with: sudo useradd --system --no-create-home \
                   --shell /usr/sbin/nologin hyphae"
                .to_string(),
        })?;

    Ok(JailUser {
        uid: user.uid.as_raw(),
        gid: user.gid.as_raw(),
        name: user.name,
    })
}
