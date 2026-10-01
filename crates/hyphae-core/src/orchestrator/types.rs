//! Request, result, and plan types for orchestration pipelines.
//!
//! All plan and result types derive `Debug` and `Serialize` (for `--json` output)
//! and implement `Display` (for plain text output in Terraform-plan style).

use std::fmt;
use std::path::PathBuf;

use serde::Serialize;

use crate::net::LanAllow;

/// Progress callback type. The orchestrator calls this with human-readable
/// status messages during pipeline execution. The library never formats
/// output directly -- callers provide the formatting via this callback.
pub type ProgressFn = Box<dyn Fn(&str) + Send>;

// ---------------------------------------------------------------------------
// Build types
// ---------------------------------------------------------------------------

/// Request for the build pipeline.
#[derive(Debug, Clone)]
pub struct BuildRequest {
    /// Path to the project directory to build.
    pub project_dir: PathBuf,
    /// Image name (derived from project dir if not provided).
    pub name: Option<String>,
    /// Image tag (defaults to "latest").
    pub tag: String,
    /// Force rebuild even if a cache hit exists.
    pub force: bool,
    /// Default vCPU count for VMs using this image.
    pub default_vcpus: i64,
    /// Default memory in MiB for VMs using this image.
    pub default_memory_mib: i64,
    /// Embed the dimension-agent binary in the rootfs for gateway communication.
    pub embed_dimension_agent: bool,
    /// Path to a pre-built binary or directory (bypasses project detection).
    pub binary_path: Option<PathBuf>,
    /// Entrypoint command inside the VM (used with binary_path).
    pub entrypoint: Option<String>,
    /// Docker image reference (bypasses project detection).
    pub docker_image: Option<String>,
}

/// Result of a successful build pipeline.
#[derive(Debug, Serialize)]
pub struct BuildResult {
    /// Image name:tag reference.
    pub reference: String,
    /// Content hash of the source directory.
    pub content_hash: String,
    /// Path to the built ext4 image on disk.
    pub disk_path: String,
    /// Size of the ext4 image in bytes.
    pub image_size: u64,
    /// Whether this was a cache hit (no rebuild needed).
    pub cached: bool,
    /// Registry image ID.
    pub image_id: i64,
}

impl fmt::Display for BuildResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Build complete:")?;
        writeln!(f, "  image:    {}", self.reference)?;
        writeln!(f, "  hash:     {}", self.content_hash)?;
        writeln!(f, "  path:     {}", self.disk_path)?;
        writeln!(f, "  size:     {} bytes", self.image_size)?;
        if self.cached {
            writeln!(f, "  cached:   yes (no rebuild needed)")?;
        }
        Ok(())
    }
}

/// Dry-run plan for the build pipeline.
#[derive(Debug, Serialize)]
pub struct BuildPlan {
    /// Ordered list of actions the build would perform.
    pub actions: Vec<PlannedAction>,
    /// Cache status for the source directory.
    pub cache: CacheStatus,
    /// Image reference that would be created.
    pub reference: String,
}

impl fmt::Display for BuildPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Build plan for {}:", self.reference)?;
        writeln!(f)?;
        writeln!(f, "  Cache: {}", self.cache)?;
        writeln!(f)?;
        for action in &self.actions {
            writeln!(f, "  {action}")?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Run types
// ---------------------------------------------------------------------------

/// Vsock configuration for the run pipeline.
///
/// When provided in a RunRequest, the VM will be configured with a
/// vsock device at the specified UDS path with the given guest CID.
#[derive(Debug, Clone)]
pub struct VsockRunConfig {
    /// Guest CID (must be >= 3, validated by VmConfig).
    pub guest_cid: u32,
    /// Path for the vsock Unix domain socket on the host.
    /// Convention: `{runtime_base}/vms/{vm_id}/v.sock`
    pub uds_path: String,
}

/// Request for the run pipeline.
#[derive(Debug, Clone)]
pub struct RunRequest {
    /// Image reference (name:tag or name).
    pub reference: String,
    /// Path to the kernel image file.
    pub kernel_path: PathBuf,
    /// vCPU count override (uses bundle default if None).
    pub vcpus: Option<u8>,
    /// Memory override in MiB (uses bundle default if None).
    pub memory_mib: Option<u64>,
    /// Additional boot arguments.
    pub boot_args: Option<String>,
    /// Whether to set up TAP + NAT networking.
    pub enable_network: bool,
    /// Destination-scoped LAN exceptions for NAT'd runs (`CIDR:PORT`,
    /// TCP only). Empty preserves full LAN isolation.
    pub lan_allow: Vec<LanAllow>,
    /// Skip NAT/masquerade rules even with networking enabled.
    pub no_nat: bool,
    /// Run VM in Firecracker jailer sandbox (opt-in, requires root).
    pub jail: bool,
    /// Vsock configuration. If set, the VM will have a vsock device.
    pub vsock: Option<VsockRunConfig>,
}

/// Result of a successful run pipeline.
#[derive(Debug, Serialize)]
pub struct RunResult {
    /// VM identifier.
    pub vm_id: String,
    /// Image reference used.
    pub reference: String,
    /// Process PID.
    pub pid: u32,
    /// Path to the API socket.
    pub api_socket: String,
    /// vCPU count.
    pub vcpus: u8,
    /// Memory in MiB.
    pub memory_mib: u64,
    /// Vsock UDS path (set when vsock was configured in the request).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vsock_uds_path: Option<String>,
    /// Path to the console log file capturing Firecracker stdout/stderr.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_file: Option<String>,
}

impl fmt::Display for RunResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "VM started:")?;
        writeln!(f, "  vm_id:   {}", self.vm_id)?;
        writeln!(f, "  image:   {}", self.reference)?;
        writeln!(f, "  pid:     {}", self.pid)?;
        writeln!(f, "  socket:  {}", self.api_socket)?;
        writeln!(f, "  vcpus:   {}", self.vcpus)?;
        writeln!(f, "  memory:  {} MiB", self.memory_mib)?;
        if let Some(ref vsock) = self.vsock_uds_path {
            writeln!(f, "  vsock:   {}", vsock)?;
        }
        if let Some(ref log) = self.log_file {
            writeln!(f, "  log:     {}", log)?;
        }
        Ok(())
    }
}

/// Dry-run plan for the run pipeline.
#[derive(Debug, Serialize)]
pub struct RunPlan {
    /// Ordered list of actions the run would perform.
    pub actions: Vec<PlannedAction>,
    /// Image reference that would be used.
    pub reference: String,
    /// vCPU count that would be configured.
    pub vcpus: u8,
    /// Memory in MiB that would be configured.
    pub memory_mib: u64,
    /// Launch mode: "direct" or "jailed".
    pub launch_mode: String,
    /// Whether networking will be set up.
    pub network_enabled: bool,
    /// Whether vsock will be configured.
    pub vsock_enabled: bool,
}

impl fmt::Display for RunPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Run plan for {}:", self.reference)?;
        writeln!(f)?;
        writeln!(f, "  vcpus:   {}", self.vcpus)?;
        writeln!(f, "  memory:  {} MiB", self.memory_mib)?;
        writeln!(f, "  launch:  {}", self.launch_mode)?;
        writeln!(
            f,
            "  network: {}",
            if self.network_enabled {
                "enabled"
            } else {
                "disabled"
            }
        )?;
        writeln!(
            f,
            "  vsock:   {}",
            if self.vsock_enabled {
                "enabled"
            } else {
                "disabled"
            }
        )?;
        writeln!(f)?;
        for action in &self.actions {
            writeln!(f, "  {action}")?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Stop types
// ---------------------------------------------------------------------------

/// Dry-run plan for the stop pipeline.
#[derive(Debug, Serialize)]
pub struct StopPlan {
    /// Ordered list of actions the stop would perform.
    pub actions: Vec<PlannedAction>,
    /// VM identifier that would be stopped.
    pub vm_id: String,
}

impl fmt::Display for StopPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Stop plan for VM {}:", self.vm_id)?;
        writeln!(f)?;
        for action in &self.actions {
            writeln!(f, "  {action}")?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Shared types
// ---------------------------------------------------------------------------

/// A single planned action in a dry-run plan.
#[derive(Debug, Serialize)]
pub struct PlannedAction {
    /// What kind of action this is.
    pub action_type: ActionType,
    /// Human-readable description.
    pub description: String,
}

impl fmt::Display for PlannedAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let prefix = match self.action_type {
            ActionType::Create => "+",
            ActionType::Update => "~",
            ActionType::Destroy => "-",
            ActionType::Skip => " ",
        };
        write!(f, "{prefix} {}", self.description)
    }
}

/// Type of planned action.
#[derive(Debug, Clone, Serialize)]
pub enum ActionType {
    /// Resource will be created.
    Create,
    /// Resource will be updated.
    Update,
    /// Resource will be destroyed.
    Destroy,
    /// Resource will be skipped (already in desired state).
    Skip,
}

/// Cache status for a source directory during build.
#[derive(Debug, Serialize)]
pub enum CacheStatus {
    /// Source hash matches a registered image -- build can be skipped.
    Hit {
        content_hash: String,
        image_id: i64,
    },
    /// No matching image found -- build required.
    Miss { content_hash: String },
    /// Cache check bypassed (force rebuild requested).
    Bypassed,
}

impl fmt::Display for CacheStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CacheStatus::Hit {
                content_hash,
                image_id,
            } => write!(f, "HIT (hash={content_hash}, image_id={image_id})"),
            CacheStatus::Miss { content_hash } => {
                write!(f, "MISS (hash={content_hash})")
            }
            CacheStatus::Bypassed => write!(f, "BYPASSED (force rebuild)"),
        }
    }
}
