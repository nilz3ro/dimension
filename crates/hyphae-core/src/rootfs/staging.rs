use std::fs;
use std::path::Path;

use hyphae_errors::RootfsError;

/// Directories created inside a rootfs staging area.
const STAGING_DIRS: &[&str] = &[
    "dev",        // mount point for devtmpfs
    "proc",       // mount point for procfs
    "sys",        // mount point for sysfs
    "sbin",       // contains /sbin/init
    "etc/hyphae", // contains entrypoint config
    "app",        // application files
    "lib",        // shared libraries (e.g., libstdc++ for Node.js)
    "tmp",        // writable temp directory
];

/// Create the standard directory structure under `staging` for a rootfs image.
///
/// Creates: dev, proc, sys, sbin, etc/hyphae, app, lib, tmp
///
/// This function is idempotent -- calling it on a staging directory that already
/// has the structure will not error.
pub fn create_directory_structure(staging: &Path) -> Result<(), RootfsError> {
    for dir in STAGING_DIRS {
        fs::create_dir_all(staging.join(dir))?;
    }
    Ok(())
}
