//! Pre-flight validation for gateway startup.
//!
//! [`validate_prerequisites`] checks that the kernel image and all hyphae
//! bundle rootfs files exist on disk before the gateway binds its listener.
//! If any check fails, the gateway should print the error and exit without
//! accepting traffic.

use std::path::Path;

use hyphae_core::registry::Registry;

/// Validate all prerequisites before the gateway starts accepting traffic.
///
/// Checks:
/// 1. Kernel file exists at the configured path
/// 2. At least one bundle exists in the registry
/// 3. All hyphae bundles have their rootfs files present on disk
///
/// Returns `Ok(bundle_count)` on success for logging, or a descriptive error
/// message on failure.
pub fn validate_prerequisites(
    registry: &Registry,
    kernel_path: &Path,
) -> Result<usize, String> {
    // 1. Verify kernel exists
    if !kernel_path.exists() {
        return Err(format!(
            "kernel not found at {}: ensure the kernel image exists or set --kernel-path",
            kernel_path.display()
        ));
    }

    // 2. Enumerate all bundles
    let images = registry
        .list_images(None, None, None, None)
        .map_err(|e| format!("failed to list hyphae bundles: {e}"))?;

    if images.is_empty() {
        return Err(
            "no hyphae bundles found in registry: run 'hyphae build' to create at least one bundle"
                .to_string(),
        );
    }

    // 3. Verify each bundle's rootfs exists
    let mut errors = Vec::new();
    for image in &images {
        let rootfs_path = Path::new(&image.disk_path);
        if !rootfs_path.exists() {
            errors.push(format!(
                "  - {}:{} rootfs missing at {}",
                image.name, image.tag, image.disk_path
            ));
        }
    }

    if !errors.is_empty() {
        return Err(format!(
            "bundle validation failed -- {} bundle(s) have missing rootfs files:\n{}",
            errors.len(),
            errors.join("\n")
        ));
    }

    Ok(images.len())
}
