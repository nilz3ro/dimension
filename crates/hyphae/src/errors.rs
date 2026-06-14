use hyphae_errors::*;

/// Translate a library error into an actionable user message.
///
/// Attempts to downcast the anyhow error to [`HyphaeError`] for structured
/// matching. Each match arm provides specific guidance for how the user can
/// fix the problem. Unmatched errors fall through to the error's Display
/// impl, which always includes the error code (E1xx, E2xx, etc.).
pub fn translate_error(err: &anyhow::Error) -> String {
    if let Some(he) = err.downcast_ref::<HyphaeError>() {
        return translate_hyphae_error(he);
    }
    // Fallback: show the anyhow error chain
    format!("{err:#}")
}

fn translate_hyphae_error(err: &HyphaeError) -> String {
    match err {
        // --- Prerequisite errors (E1xx) ---
        HyphaeError::Prereq(PrereqError::KvmNotPresent) => {
            "KVM is not available on this system.\n\
             \n\
             To fix:\n\
             1. Load the KVM kernel module:\n\
             \n\
             sudo modprobe kvm\n\
             sudo modprobe kvm_intel  # or kvm_amd for AMD CPUs\n\
             \n\
             2. If running in a VM, ensure nested virtualization is enabled."
                .to_string()
        }
        HyphaeError::Prereq(PrereqError::KvmPermissionDenied) => {
            "Permission denied accessing /dev/kvm.\n\
             \n\
             To fix:\n\
             1. Add your user to the kvm group:\n\
             \n\
             sudo usermod -aG kvm $USER\n\
             \n\
             2. Log out and back in for the change to take effect."
                .to_string()
        }
        HyphaeError::Prereq(PrereqError::FirecrackerNotFound) => {
            "firecracker binary not found in PATH.\n\
             \n\
             To fix:\n\
             1. Download Firecracker from:\n\
                https://github.com/firecracker-microvm/firecracker/releases\n\
             \n\
             2. Place the binary in a directory on your PATH (e.g., /usr/local/bin/)"
                .to_string()
        }
        HyphaeError::Prereq(PrereqError::MkfsNotFound) => {
            "mkfs.ext4 binary not found in PATH.\n\
             \n\
             To fix:\n\
             Install e2fsprogs:\n\
             - Debian/Ubuntu: sudo apt install e2fsprogs\n\
             - Fedora/RHEL:   sudo dnf install e2fsprogs\n\
             - Arch:          sudo pacman -S e2fsprogs"
                .to_string()
        }
        HyphaeError::Prereq(PrereqError::KvmNotAccessible { reason }) => {
            format!(
                "/dev/kvm is not accessible: {reason}\n\
                 \n\
                 Check that KVM is installed and your user has appropriate permissions.\n\
                 Run 'hyphae check' for a full prerequisites report."
            )
        }

        // --- Config errors (E2xx) ---
        HyphaeError::Config(ConfigError::ValidationFailed(errors)) => {
            let details: Vec<String> = errors.iter().map(|e| format!("  - {e}")).collect();
            format!(
                "VM configuration is invalid:\n{}\n\
                 \n\
                 Fix the configuration values and try again.",
                details.join("\n")
            )
        }
        HyphaeError::Config(ConfigError::Serialization(msg)) => {
            format!(
                "Failed to generate Firecracker config: {msg}\n\
                 \n\
                 This is likely a bug. Please report it."
            )
        }

        // --- Process errors (E3xx) ---
        HyphaeError::Process(e) => {
            format!(
                "VM process error: {e}\n\
                 \n\
                 Check that:\n\
                 1. firecracker is installed and in your PATH\n\
                 2. /dev/kvm is accessible (run 'hyphae check')\n\
                 3. No other process is using the same API socket"
            )
        }

        // --- Rootfs errors (E4xx) ---
        HyphaeError::Rootfs(RootfsError::AmbiguousProject { path }) => {
            format!(
                "Ambiguous project type at {}: both package.json and Cargo.toml found.\n\
                 \n\
                 Remove one of the marker files to resolve:\n\
                 - Keep package.json for a JavaScript project\n\
                 - Keep Cargo.toml for a Rust project",
                path.display()
            )
        }
        HyphaeError::Rootfs(RootfsError::UnrecognizedProject { path }) => {
            format!(
                "Unrecognized project at {}: no package.json or Cargo.toml found.\n\
                 \n\
                 Supported project types:\n\
                 - JavaScript: add a package.json\n\
                 - Rust:       add a Cargo.toml",
                path.display()
            )
        }
        HyphaeError::Rootfs(RootfsError::NoEntrypoint { project_dir }) => {
            format!(
                "No entrypoint found in {}.\n\
                 \n\
                 For JavaScript projects, add one of:\n\
                 - scripts.start in package.json\n\
                 - main field in package.json\n\
                 - index.js in the project root",
                project_dir.display()
            )
        }
        HyphaeError::Rootfs(e) => {
            format!("Build error: {e}")
        }

        // --- Registry errors (E5xx) ---
        HyphaeError::Registry(RegistryError::ImageNotFound { id }) => {
            format!(
                "Bundle not found (id={id}).\n\
                 \n\
                 List available bundles:\n\
                 hyphae list bundles"
            )
        }
        HyphaeError::Registry(RegistryError::ImageInUse {
            image_id,
            vm_count,
        }) => {
            format!(
                "Cannot delete bundle (id={image_id}): {vm_count} VM(s) still running.\n\
                 \n\
                 Stop the running VMs first:\n\
                 hyphae list vms\n\
                 hyphae stop <vm-id>"
            )
        }
        HyphaeError::Registry(RegistryError::NoDataDirectory) => {
            "Cannot determine data directory.\n\
             \n\
             To fix, set the HYPHAE_DATA_DIR environment variable:\n\
             export HYPHAE_DATA_DIR=/path/to/hyphae/data"
                .to_string()
        }
        HyphaeError::Registry(e) => {
            format!("Registry error: {e}")
        }

        // --- Orchestrator errors (E6xx) ---
        HyphaeError::Orchestrator(OrchestratorError::BundleNotFound { reference }) => {
            format!(
                "Bundle '{reference}' not found.\n\
                 \n\
                 List available bundles:\n\
                 hyphae list bundles\n\
                 \n\
                 Build a bundle from a project:\n\
                 hyphae build /path/to/project"
            )
        }
        HyphaeError::Orchestrator(OrchestratorError::VmNotFound { vm_id }) => {
            format!(
                "VM '{vm_id}' not found.\n\
                 \n\
                 List running VMs:\n\
                 hyphae list vms"
            )
        }
        HyphaeError::Orchestrator(OrchestratorError::KernelNotFound { path }) => {
            format!(
                "Kernel file not found at {}.\n\
                 \n\
                 Provide a valid vmlinux kernel image:\n\
                 hyphae run <bundle> --kernel /path/to/vmlinux\n\
                 \n\
                 Download a Firecracker-compatible kernel from:\n\
                 https://github.com/firecracker-microvm/firecracker/tree/main/resources",
                path.display()
            )
        }
        HyphaeError::Orchestrator(OrchestratorError::ProjectDirNotFound { path }) => {
            format!(
                "Project directory not found at {}.\n\
                 \n\
                 Provide a valid project directory:\n\
                 hyphae build /path/to/project",
                path.display()
            )
        }
        HyphaeError::Orchestrator(e) => {
            format!("{e}")
        }

        // --- Network errors (E8xx) ---
        HyphaeError::Network(NetworkError::InsufficientPrivileges {
            operation,
            hint,
        }) => {
            format!(
                "Insufficient privileges for {operation}.\n\
                 \n\
                 {hint}\n\
                 \n\
                 Alternatively, set the CAP_NET_ADMIN capability:\n\
                 sudo setcap cap_net_admin+ep $(which hyphae)"
            )
        }
        HyphaeError::Network(NetworkError::SubnetExhausted) => {
            "All 16,384 network subnets are allocated.\n\
             \n\
             Stop some running VMs to free up network resources:\n\
             hyphae list vms\n\
             hyphae stop <vm-id>"
                .to_string()
        }
        HyphaeError::Network(e) => {
            format!("Network error: {e}")
        }

        // --- Jail errors (E8xx) ---
        HyphaeError::Jail(JailError::UserNotFound { hint }) => {
            format!(
                "The hyphae system user does not exist.\n\
                 \n\
                 Create it with:\n\
                 {hint}\n\
                 \n\
                 Or run without jailing:\n\
                 hyphae run <bundle> --kernel <path>"
            )
        }
        HyphaeError::Jail(JailError::JailerNotFound { path }) => {
            format!(
                "Jailer binary not found at {}.\n\
                 \n\
                 Install Firecracker (includes jailer):\n\
                 https://github.com/firecracker-microvm/firecracker/releases\n\
                 \n\
                 Or run without jailing (default mode, no --jail flag needed).",
                path.display()
            )
        }
        HyphaeError::Jail(e) => {
            format!("Jail error: {e}")
        }

        // --- Kernel errors (E7xx) ---
        HyphaeError::Kernel(e) => {
            format!("Kernel error: {e}")
        }

        // --- MMDS errors (E7xx) ---
        HyphaeError::Mmds(e) => {
            format!("MMDS error: {e}")
        }

        // --- Fallback: use Display impl (includes error code) ---
        other => format!("{other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prereq_kvm_not_present() {
        let err = HyphaeError::Prereq(PrereqError::KvmNotPresent);
        let msg = translate_hyphae_error(&err);
        assert!(msg.contains("KVM is not available"));
        assert!(msg.contains("sudo modprobe kvm"));
    }

    #[test]
    fn test_prereq_kvm_permission_denied() {
        let err = HyphaeError::Prereq(PrereqError::KvmPermissionDenied);
        let msg = translate_hyphae_error(&err);
        assert!(msg.contains("Permission denied"));
        assert!(msg.contains("sudo usermod -aG kvm"));
    }

    #[test]
    fn test_prereq_firecracker_not_found() {
        let err = HyphaeError::Prereq(PrereqError::FirecrackerNotFound);
        let msg = translate_hyphae_error(&err);
        assert!(msg.contains("firecracker binary not found"));
        assert!(msg.contains("https://github.com/firecracker-microvm"));
    }

    #[test]
    fn test_config_validation_failed() {
        let err = HyphaeError::Config(ConfigError::ValidationFailed(vec![
            ConfigValidationError::InvalidVcpuCount { value: 7 },
            ConfigValidationError::InvalidMemSize { value: 2 },
        ]));
        let msg = translate_hyphae_error(&err);
        assert!(msg.contains("VM configuration is invalid"));
        assert!(msg.contains("E201"));
        assert!(msg.contains("E202"));
        assert!(msg.contains("Fix the configuration values"));
    }

    #[test]
    fn test_process_error_guidance() {
        let err = HyphaeError::Process(ProcessError::SpawnFailed("test".to_string()));
        let msg = translate_hyphae_error(&err);
        assert!(msg.contains("VM process error"));
        assert!(msg.contains("firecracker is installed"));
        assert!(msg.contains("/dev/kvm is accessible"));
    }

    #[test]
    fn test_rootfs_ambiguous_project() {
        let err = HyphaeError::Rootfs(RootfsError::AmbiguousProject {
            path: std::path::PathBuf::from("/tmp/project"),
        });
        let msg = translate_hyphae_error(&err);
        assert!(msg.contains("/tmp/project"));
        assert!(msg.contains("package.json"));
        assert!(msg.contains("Cargo.toml"));
    }

    #[test]
    fn test_bundle_not_found_guidance() {
        let err = HyphaeError::Orchestrator(OrchestratorError::BundleNotFound {
            reference: "my-app:latest".to_string(),
        });
        let msg = translate_hyphae_error(&err);
        assert!(msg.contains("my-app:latest"));
        assert!(msg.contains("hyphae list bundles"));
        assert!(msg.contains("hyphae build"));
    }

    #[test]
    fn test_kernel_not_found_guidance() {
        let err = HyphaeError::Orchestrator(OrchestratorError::KernelNotFound {
            path: std::path::PathBuf::from("/tmp/vmlinux"),
        });
        let msg = translate_hyphae_error(&err);
        assert!(msg.contains("/tmp/vmlinux"));
        assert!(msg.contains("--kernel"));
        assert!(msg.contains("firecracker"));
    }

    #[test]
    fn test_image_in_use_guidance() {
        let err = HyphaeError::Registry(RegistryError::ImageInUse {
            image_id: 1,
            vm_count: 2,
        });
        let msg = translate_hyphae_error(&err);
        assert!(msg.contains("2 VM(s) still running"));
        assert!(msg.contains("hyphae stop"));
    }

    #[test]
    fn test_image_not_found_guidance() {
        let err = HyphaeError::Registry(RegistryError::ImageNotFound { id: 42 });
        let msg = translate_hyphae_error(&err);
        assert!(msg.contains("Bundle not found"));
        assert!(msg.contains("hyphae list bundles"));
    }

    #[test]
    fn test_unknown_error_uses_display() {
        let err = HyphaeError::Other("something unexpected".to_string());
        let msg = translate_hyphae_error(&err);
        assert!(msg.contains("something unexpected"));
    }

    #[test]
    fn test_translate_error_with_anyhow() {
        let he = HyphaeError::Prereq(PrereqError::FirecrackerNotFound);
        let anyhow_err: anyhow::Error = he.into();
        let msg = translate_error(&anyhow_err);
        assert!(msg.contains("firecracker binary not found"));
        assert!(msg.contains("https://github.com/firecracker-microvm"));
    }

    #[test]
    fn test_translate_error_non_hyphae_fallback() {
        let anyhow_err = anyhow::anyhow!("some other error");
        let msg = translate_error(&anyhow_err);
        assert!(msg.contains("some other error"));
    }

    #[test]
    fn test_network_insufficient_privileges() {
        let err = HyphaeError::Network(NetworkError::InsufficientPrivileges {
            operation: "TAP device creation".to_string(),
            hint: "Run with sudo or as root".to_string(),
        });
        let msg = translate_hyphae_error(&err);
        assert!(msg.contains("Insufficient privileges"));
        assert!(msg.contains("TAP device creation"));
        assert!(msg.contains("CAP_NET_ADMIN"));
    }

    #[test]
    fn test_network_subnet_exhausted() {
        let err = HyphaeError::Network(NetworkError::SubnetExhausted);
        let msg = translate_hyphae_error(&err);
        assert!(msg.contains("16,384"));
        assert!(msg.contains("hyphae stop"));
    }

    #[test]
    fn test_network_generic_error() {
        let err = HyphaeError::Network(NetworkError::IptablesFailed(
            "iptables not found".to_string(),
        ));
        let msg = translate_hyphae_error(&err);
        assert!(msg.contains("Network error"));
        assert!(msg.contains("iptables not found"));
    }

    #[test]
    fn test_jail_user_not_found() {
        let err = HyphaeError::Jail(JailError::UserNotFound {
            hint: "sudo useradd --system --no-create-home hyphae".to_string(),
        });
        let msg = translate_hyphae_error(&err);
        assert!(msg.contains("hyphae system user does not exist"));
        assert!(msg.contains("useradd"));
    }

    #[test]
    fn test_jail_jailer_not_found() {
        let err = HyphaeError::Jail(JailError::JailerNotFound {
            path: std::path::PathBuf::from("/usr/local/bin/jailer"),
        });
        let msg = translate_hyphae_error(&err);
        assert!(msg.contains("/usr/local/bin/jailer"));
        assert!(msg.contains("firecracker"));
    }

    #[test]
    fn test_jail_generic_error() {
        let err = HyphaeError::Jail(JailError::JailerSpawnFailed(
            "permission denied".to_string(),
        ));
        let msg = translate_hyphae_error(&err);
        assert!(msg.contains("Jail error"));
        assert!(msg.contains("permission denied"));
    }

    #[test]
    fn test_kernel_error_translation() {
        let err = HyphaeError::Kernel(KernelError::KernelNotFound { id: 5 });
        let msg = translate_hyphae_error(&err);
        assert!(msg.contains("Kernel error"));
        assert!(msg.contains("id=5"));
    }

    #[test]
    fn test_mmds_error_translation() {
        let err = HyphaeError::Mmds(MmdsError::MetadataTooLarge {
            actual_bytes: 60000,
        });
        let msg = translate_hyphae_error(&err);
        assert!(msg.contains("MMDS error"));
        assert!(msg.contains("60000"));
    }
}
