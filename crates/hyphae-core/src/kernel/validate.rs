//! ELF validation for Firecracker-compatible kernel images.
//!
//! Checks that a file is a valid 64-bit ELF binary targeting x86_64 or
//! aarch64, which are the only architectures Firecracker supports.

use elf::abi;
use elf::endian::AnyEndian;
use elf::file::Class;
use elf::ElfBytes;
use std::path::Path;

/// Result of validating a kernel file.
pub struct KernelValidation {
    pub is_valid: bool,
    pub class: Option<String>,
    pub machine: Option<String>,
    pub error: Option<String>,
}

/// Validate that a file is a valid Firecracker-compatible kernel.
///
/// Checks: file exists, is a regular file, has valid ELF header, is 64-bit,
/// and targets x86_64 or aarch64. Reading the full file into memory is
/// acceptable for this use case (kernels are typically around 40 MB and
/// validation happens once per registration).
pub fn validate_kernel(path: &Path) -> KernelValidation {
    // Check file exists and is a regular file.
    let metadata = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) => {
            return KernelValidation {
                is_valid: false,
                class: None,
                machine: None,
                error: Some(format!("Cannot access kernel file: {}", e)),
            };
        }
    };

    if !metadata.is_file() {
        return KernelValidation {
            is_valid: false,
            class: None,
            machine: None,
            error: Some("Path is not a regular file".to_string()),
        };
    }

    // Read file and parse ELF header.
    let file_data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            return KernelValidation {
                is_valid: false,
                class: None,
                machine: None,
                error: Some(format!("Cannot read kernel file: {}", e)),
            };
        }
    };

    let elf = match ElfBytes::<AnyEndian>::minimal_parse(&file_data) {
        Ok(e) => e,
        Err(e) => {
            return KernelValidation {
                is_valid: false,
                class: None,
                machine: None,
                error: Some(format!("Not a valid ELF binary: {}", e)),
            };
        }
    };

    // Check 64-bit class.
    let class = match elf.ehdr.class {
        Class::ELF64 => "64-bit",
        Class::ELF32 => {
            return KernelValidation {
                is_valid: false,
                class: Some("32-bit".to_string()),
                machine: None,
                error: Some("Firecracker requires a 64-bit kernel".to_string()),
            };
        }
    };

    // Check machine type.
    let machine = match elf.ehdr.e_machine {
        abi::EM_X86_64 => "x86_64",
        abi::EM_AARCH64 => "aarch64",
        other => {
            return KernelValidation {
                is_valid: false,
                class: Some(class.to_string()),
                machine: Some(format!("unknown ({})", other)),
                error: Some(format!(
                    "Unsupported architecture (e_machine={}). Firecracker requires x86_64 or aarch64.",
                    other
                )),
            };
        }
    };

    KernelValidation {
        is_valid: true,
        class: Some(class.to_string()),
        machine: Some(machine.to_string()),
        error: None,
    }
}
