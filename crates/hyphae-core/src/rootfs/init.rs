//! Init binary embedding and validation for rootfs images.
//!
//! The init binary (`hyphae-init`) is compiled separately and embedded in
//! hyphae-core at build time via `include_bytes!`. This module provides:
//!
//! - [`embed_init`]: Writes the init binary and entrypoint config to a staging dir
//! - [`validate_init`]: Validates the init binary and entrypoint are correctly placed
//! - [`embed_dimension_agent`]: Optionally writes the dimension-agent binary to staging
//! - [`validate_dimension_agent`]: Validates the dimension-agent binary is correctly placed

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use hyphae_errors::RootfsError;

/// The pre-compiled init binary, embedded at build time.
///
/// On Linux hosts with the musl target installed, this is the real
/// hyphae-init binary. On other platforms, this is a placeholder.
const INIT_BINARY: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/hyphae-init"));

/// The pre-compiled dimension-agent binary, embedded at build time.
///
/// On Linux hosts with the musl target installed, this is the real
/// dimension-agent binary. On other platforms, this is a placeholder.
const DIMENSION_AGENT_BINARY: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/dimension-agent"));

/// ELF magic bytes: 0x7f followed by 'E', 'L', 'F'.
const ELF_MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];

/// Returns `true` if the embedded init binary is a real ELF file
/// (i.e., not a placeholder written on non-Linux hosts).
pub fn is_init_real() -> bool {
    INIT_BINARY.len() >= 4 && INIT_BINARY[..4] == ELF_MAGIC
}

/// Returns `true` if the embedded dimension-agent binary is a real ELF file.
pub fn is_dimension_agent_real() -> bool {
    DIMENSION_AGENT_BINARY.len() >= 4 && DIMENSION_AGENT_BINARY[..4] == ELF_MAGIC
}

/// Embed the init binary and entrypoint config into a staging directory.
///
/// - Writes the pre-compiled init binary to `staging_dir/sbin/init` with mode 0o755
/// - Writes the entrypoint command to `staging_dir/etc/hyphae/entrypoint`
///   as null-separated parts (e.g., `/app/mybin\0--flag\0value`)
///
/// The `entrypoint_cmd` slice provides the command parts:
/// - `entrypoint_cmd[0]` is the binary path (absolute path inside the image)
/// - `entrypoint_cmd[1..]` are arguments
pub fn embed_init(staging_dir: &Path, entrypoint_cmd: &[String]) -> Result<(), RootfsError> {
    embed_init_from_bytes(staging_dir, INIT_BINARY, entrypoint_cmd)
}

/// Like [`embed_init`], but takes explicit init binary bytes instead of using
/// the compile-time embedded binary. Used when the embedded binary is a
/// placeholder and the real binary was obtained at runtime (e.g., built via Docker).
pub fn embed_init_from_bytes(
    staging_dir: &Path,
    init_binary: &[u8],
    entrypoint_cmd: &[String],
) -> Result<(), RootfsError> {
    // Write init binary to sbin/init
    let init_path = staging_dir.join("sbin/init");
    fs::create_dir_all(init_path.parent().expect("sbin/init has parent"))?;
    // Remove any existing file/symlink first. Docker images often have
    // sbin/init as an absolute symlink (e.g., -> /bin/busybox) which would
    // cause fs::write to follow it to the host filesystem.
    if init_path.exists() || init_path.symlink_metadata().is_ok() {
        fs::remove_file(&init_path)?;
    }
    fs::write(&init_path, init_binary)?;
    fs::set_permissions(&init_path, fs::Permissions::from_mode(0o755))?;

    // Write entrypoint as null-separated parts
    let entrypoint_dir = staging_dir.join("etc/hyphae");
    fs::create_dir_all(&entrypoint_dir)?;
    let entrypoint_content = entrypoint_cmd.join("\0");
    fs::write(entrypoint_dir.join("entrypoint"), entrypoint_content)?;

    Ok(())
}

/// Embed the dimension-agent binary into a staging directory.
///
/// Writes the pre-compiled dimension-agent binary to `staging_dir/sbin/dimension-agent`
/// with mode 0o755. When present in the rootfs, `hyphae-init` will start it
/// alongside the application entrypoint.
pub fn embed_dimension_agent(staging_dir: &Path) -> Result<(), RootfsError> {
    embed_dimension_agent_from_bytes(staging_dir, DIMENSION_AGENT_BINARY)
}

/// Like [`embed_dimension_agent`], but takes explicit binary bytes instead of
/// using the compile-time embedded binary. Used when the embedded binary is a
/// placeholder and the real binary was obtained at runtime (e.g., built via Docker).
pub fn embed_dimension_agent_from_bytes(
    staging_dir: &Path,
    agent_binary: &[u8],
) -> Result<(), RootfsError> {
    let agent_path = staging_dir.join("sbin/dimension-agent");
    fs::create_dir_all(agent_path.parent().expect("sbin/dimension-agent has parent"))?;
    fs::write(&agent_path, agent_binary)?;
    fs::set_permissions(&agent_path, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

/// Validate that the dimension-agent binary is correctly embedded.
///
/// Checks:
/// 1. `sbin/dimension-agent` exists and starts with ELF magic bytes
/// 2. `sbin/dimension-agent` has the executable bit set
pub fn validate_dimension_agent(staging_dir: &Path) -> Result<(), RootfsError> {
    let agent_path = staging_dir.join("sbin/dimension-agent");

    let agent_bytes = fs::read(&agent_path).map_err(|e| {
        RootfsError::Io(std::io::Error::new(
            e.kind(),
            format!(
                "failed to read dimension-agent binary at {}: {e}",
                agent_path.display()
            ),
        ))
    })?;

    if agent_bytes.len() < 4 || agent_bytes[..4] != ELF_MAGIC {
        return Err(RootfsError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "dimension-agent binary at {} is not a valid ELF file (missing magic bytes)",
                agent_path.display()
            ),
        )));
    }

    let metadata = fs::metadata(&agent_path)?;
    let mode = metadata.permissions().mode();
    if mode & 0o111 == 0 {
        return Err(RootfsError::Io(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "dimension-agent binary at {} is not executable (mode: {mode:o})",
                agent_path.display()
            ),
        )));
    }

    Ok(())
}

/// Write Docker image environment configuration to the staging directory.
///
/// - Writes ENV vars to `etc/hyphae/env` (newline-separated KEY=VALUE)
/// - Writes WorkingDir to `etc/hyphae/workdir` (single line)
///
/// Both files are optional — only written if non-empty data is provided.
pub fn write_env_config(
    staging_dir: &Path,
    env_vars: &[String],
    working_dir: Option<&str>,
) -> Result<(), RootfsError> {
    if !env_vars.is_empty() {
        let env_path = staging_dir.join("etc/hyphae/env");
        fs::write(&env_path, env_vars.join("\n"))?;
    }

    if let Some(wd) = working_dir {
        let wd = wd.trim();
        if !wd.is_empty() {
            let workdir_path = staging_dir.join("etc/hyphae/workdir");
            fs::write(&workdir_path, wd)?;
        }
    }

    Ok(())
}

/// Validate that the init binary and entrypoint are correctly embedded.
///
/// Checks:
/// 1. `sbin/init` exists and starts with ELF magic bytes (0x7f, 'E', 'L', 'F')
/// 2. `sbin/init` has the executable bit set
/// 3. `etc/hyphae/entrypoint` exists
/// 4. The binary referenced in the entrypoint exists in the staging directory
///
/// This implements ROOT-07 (reinterpreted for compiled binaries: ELF magic
/// check is the architectural equivalent of a shebang check for scripts).
pub fn validate_init(staging_dir: &Path) -> Result<(), RootfsError> {
    let init_path = staging_dir.join("sbin/init");

    // Check 1: init binary exists and has ELF magic
    let init_bytes = fs::read(&init_path).map_err(|e| {
        RootfsError::Io(std::io::Error::new(
            e.kind(),
            format!("failed to read init binary at {}: {e}", init_path.display()),
        ))
    })?;

    if init_bytes.len() < 4 || init_bytes[..4] != ELF_MAGIC {
        return Err(RootfsError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "init binary at {} is not a valid ELF file (missing magic bytes)",
                init_path.display()
            ),
        )));
    }

    // Check 2: executable bit is set
    let metadata = fs::metadata(&init_path)?;
    let mode = metadata.permissions().mode();
    if mode & 0o111 == 0 {
        return Err(RootfsError::Io(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "init binary at {} is not executable (mode: {mode:o})",
                init_path.display()
            ),
        )));
    }

    // Check 3: entrypoint file exists
    let entrypoint_path = staging_dir.join("etc/hyphae/entrypoint");
    if !entrypoint_path.exists() {
        return Err(RootfsError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "etc/hyphae/entrypoint not found in staging directory",
        )));
    }

    // Check 4: the binary referenced in entrypoint exists in staging dir
    let entrypoint_data = fs::read(&entrypoint_path)?;
    let parts: Vec<&[u8]> = entrypoint_data.split(|&b| b == 0).collect();
    if let Some(cmd_bytes) = parts.first() {
        let cmd = String::from_utf8_lossy(cmd_bytes);
        if cmd.is_empty() {
            return Err(RootfsError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "entrypoint file is empty",
            )));
        }
        // The command is an absolute path inside the image (e.g., /app/mybin).
        // Strip the leading '/' and check against the staging directory.
        let relative_cmd = cmd.strip_prefix('/').unwrap_or(&cmd);
        let cmd_path = staging_dir.join(relative_cmd);
        if !cmd_path.exists() {
            return Err(RootfsError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "entrypoint binary '{}' not found in staging directory (expected at {})",
                    cmd,
                    cmd_path.display()
                ),
            )));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup_staging() -> TempDir {
        let dir = TempDir::new().unwrap();
        // Create the standard staging structure
        for subdir in &["sbin", "etc/hyphae", "app", "dev", "proc", "sys"] {
            fs::create_dir_all(dir.path().join(subdir)).unwrap();
        }
        dir
    }

    #[test]
    fn embed_init_creates_init_binary_and_entrypoint() {
        let staging = setup_staging();
        let cmd = vec!["/app/mybin".to_string(), "--port".to_string(), "8080".to_string()];

        embed_init(staging.path(), &cmd).unwrap();

        // Verify init binary was written
        let init_path = staging.path().join("sbin/init");
        assert!(init_path.exists());

        // Verify permissions
        let mode = fs::metadata(&init_path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);

        // Verify content matches INIT_BINARY
        let content = fs::read(&init_path).unwrap();
        assert_eq!(content, INIT_BINARY);

        // Verify entrypoint was written with null-separated parts
        let entrypoint = fs::read_to_string(staging.path().join("etc/hyphae/entrypoint")).unwrap();
        assert_eq!(entrypoint, "/app/mybin\0--port\08080");
    }

    #[test]
    fn embed_init_single_command_no_args() {
        let staging = setup_staging();
        let cmd = vec!["/app/server".to_string()];

        embed_init(staging.path(), &cmd).unwrap();

        let entrypoint = fs::read_to_string(staging.path().join("etc/hyphae/entrypoint")).unwrap();
        assert_eq!(entrypoint, "/app/server");
    }

    #[test]
    fn validate_init_passes_with_valid_elf() {
        let staging = setup_staging();

        // Write a fake ELF binary (just the magic header + padding)
        let mut elf_data = vec![0x7f, b'E', b'L', b'F'];
        elf_data.extend_from_slice(&[0u8; 100]);
        let init_path = staging.path().join("sbin/init");
        fs::write(&init_path, &elf_data).unwrap();
        fs::set_permissions(&init_path, fs::Permissions::from_mode(0o755)).unwrap();

        // Write entrypoint referencing a binary that exists
        let app_bin = staging.path().join("app/mybin");
        fs::write(&app_bin, b"fake binary").unwrap();
        fs::write(
            staging.path().join("etc/hyphae/entrypoint"),
            "/app/mybin",
        )
        .unwrap();

        validate_init(staging.path()).unwrap();
    }

    #[test]
    fn validate_init_rejects_non_elf() {
        let staging = setup_staging();

        // Write a non-ELF file
        let init_path = staging.path().join("sbin/init");
        fs::write(&init_path, b"#!/bin/sh\necho hello").unwrap();
        fs::set_permissions(&init_path, fs::Permissions::from_mode(0o755)).unwrap();

        fs::write(
            staging.path().join("etc/hyphae/entrypoint"),
            "/app/mybin",
        )
        .unwrap();

        let err = validate_init(staging.path()).unwrap_err();
        assert!(err.to_string().contains("not a valid ELF"));
    }

    #[test]
    fn validate_init_rejects_non_executable() {
        let staging = setup_staging();

        // Write valid ELF but without executable bit
        let mut elf_data = vec![0x7f, b'E', b'L', b'F'];
        elf_data.extend_from_slice(&[0u8; 100]);
        let init_path = staging.path().join("sbin/init");
        fs::write(&init_path, &elf_data).unwrap();
        fs::set_permissions(&init_path, fs::Permissions::from_mode(0o644)).unwrap();

        fs::write(
            staging.path().join("etc/hyphae/entrypoint"),
            "/app/mybin",
        )
        .unwrap();

        let err = validate_init(staging.path()).unwrap_err();
        assert!(err.to_string().contains("not executable"));
    }

    #[test]
    fn validate_init_rejects_missing_entrypoint() {
        let staging = setup_staging();

        // Write valid ELF init
        let mut elf_data = vec![0x7f, b'E', b'L', b'F'];
        elf_data.extend_from_slice(&[0u8; 100]);
        let init_path = staging.path().join("sbin/init");
        fs::write(&init_path, &elf_data).unwrap();
        fs::set_permissions(&init_path, fs::Permissions::from_mode(0o755)).unwrap();

        // Remove entrypoint file
        let entrypoint_path = staging.path().join("etc/hyphae/entrypoint");
        if entrypoint_path.exists() {
            fs::remove_file(&entrypoint_path).unwrap();
        }

        let err = validate_init(staging.path()).unwrap_err();
        assert!(err.to_string().contains("entrypoint not found"));
    }

    #[test]
    fn validate_init_rejects_missing_entrypoint_binary() {
        let staging = setup_staging();

        // Write valid ELF init
        let mut elf_data = vec![0x7f, b'E', b'L', b'F'];
        elf_data.extend_from_slice(&[0u8; 100]);
        let init_path = staging.path().join("sbin/init");
        fs::write(&init_path, &elf_data).unwrap();
        fs::set_permissions(&init_path, fs::Permissions::from_mode(0o755)).unwrap();

        // Entrypoint references a binary that does NOT exist
        fs::write(
            staging.path().join("etc/hyphae/entrypoint"),
            "/app/nonexistent",
        )
        .unwrap();

        let err = validate_init(staging.path()).unwrap_err();
        assert!(err.to_string().contains("not found in staging"));
    }

    // -------------------------------------------------------------------
    // dimension-agent embedding tests
    // -------------------------------------------------------------------

    #[test]
    fn embed_dimension_agent_creates_binary() {
        let staging = setup_staging();

        embed_dimension_agent(staging.path()).unwrap();

        let agent_path = staging.path().join("sbin/dimension-agent");
        assert!(agent_path.exists());

        let mode = fs::metadata(&agent_path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);

        let content = fs::read(&agent_path).unwrap();
        assert_eq!(content, DIMENSION_AGENT_BINARY);
    }

    #[test]
    fn validate_dimension_agent_passes_with_valid_elf() {
        let staging = setup_staging();

        let mut elf_data = vec![0x7f, b'E', b'L', b'F'];
        elf_data.extend_from_slice(&[0u8; 100]);
        let agent_path = staging.path().join("sbin/dimension-agent");
        fs::write(&agent_path, &elf_data).unwrap();
        fs::set_permissions(&agent_path, fs::Permissions::from_mode(0o755)).unwrap();

        validate_dimension_agent(staging.path()).unwrap();
    }

    #[test]
    fn validate_dimension_agent_rejects_non_elf() {
        let staging = setup_staging();

        let agent_path = staging.path().join("sbin/dimension-agent");
        fs::write(&agent_path, b"not an elf").unwrap();
        fs::set_permissions(&agent_path, fs::Permissions::from_mode(0o755)).unwrap();

        let err = validate_dimension_agent(staging.path()).unwrap_err();
        assert!(err.to_string().contains("not a valid ELF"));
    }

    #[test]
    fn validate_dimension_agent_rejects_non_executable() {
        let staging = setup_staging();

        let mut elf_data = vec![0x7f, b'E', b'L', b'F'];
        elf_data.extend_from_slice(&[0u8; 100]);
        let agent_path = staging.path().join("sbin/dimension-agent");
        fs::write(&agent_path, &elf_data).unwrap();
        fs::set_permissions(&agent_path, fs::Permissions::from_mode(0o644)).unwrap();

        let err = validate_dimension_agent(staging.path()).unwrap_err();
        assert!(err.to_string().contains("not executable"));
    }

    #[test]
    fn validate_dimension_agent_rejects_missing() {
        let staging = setup_staging();

        let err = validate_dimension_agent(staging.path()).unwrap_err();
        assert!(err.to_string().contains("dimension-agent"));
    }
}
