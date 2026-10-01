use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "hyphae", version, about = "Firecracker microVM manager")]
#[command(propagate_version = true)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalOpts,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Args)]
pub struct GlobalOpts {
    /// Output in JSON format
    #[arg(long, global = true)]
    pub json: bool,

    /// Suppress non-essential output (only errors and final result)
    #[arg(long, global = true)]
    pub quiet: bool,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Build a bundle from a project directory
    Build(BuildArgs),
    /// Run a VM from a bundle
    Run(RunArgs),
    /// Stop a running VM
    Stop(StopArgs),
    /// List bundles or running VMs
    List(ListArgs),
    /// Show detailed VM information
    Inspect(InspectArgs),
    /// Validate runtime prerequisites
    Check,
}

#[derive(Args)]
pub struct BuildArgs {
    /// Path to the project directory
    #[arg(conflicts_with_all = ["binary", "docker"])]
    pub project: Option<PathBuf>,

    /// Path to a pre-built binary or directory to package (bypasses project detection)
    #[arg(long, conflicts_with_all = ["project", "docker"])]
    pub binary: Option<PathBuf>,

    /// Docker image reference to convert to a Firecracker rootfs
    #[arg(long, conflicts_with_all = ["binary", "project"])]
    pub docker: Option<String>,

    /// Entrypoint command inside the VM (defaults to /app/{binary_name})
    #[arg(long)]
    pub entrypoint: Option<String>,

    /// Bundle name (defaults to directory name)
    #[arg(long)]
    pub name: Option<String>,

    /// Bundle tag (defaults to "latest")
    #[arg(long, default_value = "latest")]
    pub tag: String,

    /// Force rebuild, ignoring cache
    #[arg(long)]
    pub force: bool,

    /// Embed the dimension-agent in the rootfs for gateway communication
    #[arg(long)]
    pub with_dimension: bool,

    /// Show what would be built without executing
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Args)]
pub struct RunArgs {
    /// Bundle reference (name:tag or name for latest)
    pub bundle: String,

    /// Override vCPU count from bundle defaults
    #[arg(long)]
    pub vcpus: Option<u8>,

    /// Override memory size (MiB) from bundle defaults
    #[arg(long)]
    pub memory: Option<u64>,

    /// Path to kernel image (vmlinux)
    #[arg(long)]
    pub kernel: PathBuf,

    /// Additional kernel boot arguments
    #[arg(long)]
    pub boot_args: Option<String>,

    /// Enable TAP-based networking for the VM
    #[arg(long)]
    pub network: bool,

    /// Skip NAT/masquerade rules (requires --network)
    #[arg(long)]
    pub no_nat: bool,

    /// Allow guest egress to one LAN destination TCP port
    /// (repeatable, requires --network). Format: CIDR:PORT, e.g.
    /// `--lan-allow 192.168.105.168/32:8000`. These exceptions are
    /// inserted before the RFC1918 drops; all other LAN destinations
    /// remain blocked.
    #[arg(long = "lan-allow", value_name = "CIDR:PORT")]
    pub lan_allow: Vec<String>,

    /// Run VM in Firecracker jailer sandbox (requires root, hyphae system user)
    #[arg(long)]
    pub jail: bool,

    /// Show what would be launched without executing
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Args)]
pub struct StopArgs {
    /// VM ID (UUID) to stop
    pub vm_id: String,

    /// Show what would be stopped without executing
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Args)]
pub struct ListArgs {
    #[command(subcommand)]
    pub resource: ListResource,
}

#[derive(Subcommand)]
pub enum ListResource {
    /// List registered bundles
    Bundles,
    /// List running VMs
    Vms,
}

#[derive(Args)]
pub struct InspectArgs {
    /// VM ID (UUID) to inspect
    pub vm_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn verify_cli() {
        Cli::command().debug_assert();
    }
}
