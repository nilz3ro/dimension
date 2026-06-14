use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::Serialize;

use hyphae_core::process::runtime::{runtime_base_dir, vms_dir};
use hyphae_core::registry::storage::default_data_dir;
use hyphae_core::registry::Registry;

use crate::cli::InspectArgs;
use crate::output::OutputConfig;

// ---------------------------------------------------------------------------
// Inspect output types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct InspectOutput {
    vm_id: String,
    // Bundle info
    bundle_name: String,
    bundle_tag: String,
    bundle_hash: String,
    source_path: String,
    // Resources
    vcpus: i64,
    memory_mib: i64,
    rootfs_path: String,
    // Process
    pid: Option<u32>,
    uptime_seconds: i64,
    state: String,
    // Firecracker
    api_socket: String,
    // Init config
    init_config: Option<String>,
}

impl fmt::Display for InspectOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "VM {}", self.vm_id)?;
        writeln!(f)?;

        writeln!(f, "Bundle")?;
        writeln!(f, "  name:    {}:{}", self.bundle_name, self.bundle_tag)?;
        writeln!(f, "  hash:    {}", truncate_hash(&self.bundle_hash))?;
        writeln!(f, "  source:  {}", self.source_path)?;
        writeln!(f)?;

        writeln!(f, "Resources")?;
        writeln!(f, "  vcpus:   {}", self.vcpus)?;
        writeln!(f, "  memory:  {} MiB", self.memory_mib)?;
        writeln!(f, "  rootfs:  {}", self.rootfs_path)?;
        writeln!(f)?;

        writeln!(f, "Process")?;
        match self.pid {
            Some(pid) => writeln!(f, "  pid:     {pid}")?,
            None => writeln!(f, "  pid:     unknown")?,
        }
        writeln!(f, "  uptime:  {}", format_uptime(self.uptime_seconds))?;
        writeln!(f, "  state:   {}", self.state)?;
        writeln!(f)?;

        writeln!(f, "Firecracker")?;
        writeln!(f, "  socket:  {}", self.api_socket)?;
        writeln!(f)?;

        writeln!(f, "Init Config")?;
        match &self.init_config {
            Some(config) => write!(f, "  config:  {config}")?,
            None => write!(f, "  config:  none")?,
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn truncate_hash(hash: &str) -> &str {
    if hash.len() > 12 { &hash[..12] } else { hash }
}

/// Format seconds into `Xh Ym Zs`.
fn format_uptime(total_secs: i64) -> String {
    if total_secs < 0 {
        return "unknown".to_string();
    }
    let secs = total_secs as u64;
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;

    if h > 0 {
        format!("{h}h {m}m {s}s")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

// ---------------------------------------------------------------------------
// Command handler
// ---------------------------------------------------------------------------

pub async fn execute(args: InspectArgs, output: &OutputConfig) -> Result<()> {
    let data_dir = default_data_dir().context("Failed to resolve data directory")?;
    let registry = Registry::open(&data_dir).context("Failed to open registry")?;

    // Find the VM by vm_id.
    let all_vms = registry
        .list_all_vms()
        .context("Failed to list running VMs")?;

    let vm_record = all_vms
        .iter()
        .find(|v| v.vm_id == args.vm_id)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "VM {} not found. List running VMs with: hyphae list vms",
                args.vm_id
            )
        })?;

    // Look up the associated bundle.
    let (bundle_name, bundle_tag, bundle_hash, source_path, vcpus, memory_mib, rootfs_path, init_config) =
        match registry.find_by_id(vm_record.image_id) {
            Ok(Some(img)) => (
                img.name,
                img.tag,
                img.content_hash,
                img.source_path,
                img.default_vcpus,
                img.default_memory_mib,
                img.disk_path,
                img.init_config,
            ),
            _ => (
                "<deleted>".to_string(),
                "<deleted>".to_string(),
                "<deleted>".to_string(),
                "<deleted>".to_string(),
                0,
                0,
                "<deleted>".to_string(),
                None,
            ),
        };

    // Read process info from runtime directory.
    let base = runtime_base_dir();
    let vm_dir = vms_dir(&base).join(&args.vm_id);

    let pid = read_pid(&vm_dir);
    let api_socket = vm_dir
        .join("firecracker.sock")
        .to_string_lossy()
        .to_string();

    // Compute uptime.
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let uptime_seconds = now - vm_record.started_at;

    // Determine state from PID liveness.
    let state = match pid {
        Some(p) => {
            if is_pid_alive(p) {
                "running".to_string()
            } else {
                "unknown".to_string()
            }
        }
        None => "unknown".to_string(),
    };

    let inspect_output = InspectOutput {
        vm_id: args.vm_id,
        bundle_name,
        bundle_tag,
        bundle_hash,
        source_path,
        vcpus,
        memory_mib,
        rootfs_path,
        pid,
        uptime_seconds,
        state,
        api_socket,
        init_config,
    };

    output.print_result(&inspect_output);
    Ok(())
}

/// Read the PID from the runtime directory's PID file.
fn read_pid(runtime_dir: &std::path::Path) -> Option<u32> {
    let pid_file = runtime_dir.join("firecracker.pid");
    let content = std::fs::read_to_string(pid_file).ok()?;
    content.trim().parse().ok()
}

/// Check if a process with the given PID is alive.
fn is_pid_alive(pid: u32) -> bool {
    // Use kill(pid, 0) to probe without sending a signal.
    #[cfg(unix)]
    {
        use nix::sys::signal;
        use nix::unistd::Pid;
        signal::kill(Pid::from_raw(pid as i32), None).is_ok()
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}
