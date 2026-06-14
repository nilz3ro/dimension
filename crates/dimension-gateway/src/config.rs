//! Configuration types for the gateway server and vsock connector.

use std::path::PathBuf;
use std::time::Duration;

use crate::orchestration::config::OrchestrationConfig;

/// HTTP server configuration parsed from CLI flags with env var fallback.
#[derive(Debug, Clone, clap::Parser)]
#[command(name = "dimension-server", about = "Dimension gateway HTTP server")]
pub struct AppConfig {
    /// Port to listen on.
    #[arg(long, default_value_t = 3000, env = "DIMENSION_PORT")]
    pub port: u16,

    /// Host address to bind to.
    #[arg(long, default_value = "0.0.0.0", env = "DIMENSION_HOST")]
    pub host: String,

    /// Bearer token for request authentication.
    #[arg(long, env = "DIMENSION_TOKEN")]
    pub token: String,

    /// Path to the kernel binary used for VM boot.
    #[arg(long, default_value = "/opt/hyphae/kernel/vmlinux", env = "DIMENSION_KERNEL_PATH")]
    pub kernel_path: PathBuf,

    /// Path to the Firecracker binary.
    #[arg(long, default_value = "firecracker", env = "DIMENSION_FIRECRACKER_BIN")]
    pub firecracker_bin: PathBuf,

    /// Default boot timeout in seconds (spawn + vsock connect phase).
    #[arg(long, default_value_t = 30, env = "DIMENSION_BOOT_TIMEOUT_SECS")]
    pub boot_timeout_secs: u64,

    /// Default processing timeout in seconds (guest work phase).
    #[arg(long, default_value_t = 300, env = "DIMENSION_PROCESSING_TIMEOUT_SECS")]
    pub processing_timeout_secs: u64,

    /// Maximum allowed boot timeout in seconds (server-enforced ceiling).
    #[arg(long, default_value_t = 60, env = "DIMENSION_MAX_BOOT_TIMEOUT_SECS")]
    pub max_boot_timeout_secs: u64,

    /// Maximum allowed processing timeout in seconds (server-enforced ceiling).
    #[arg(long, default_value_t = 600, env = "DIMENSION_MAX_PROCESSING_TIMEOUT_SECS")]
    pub max_processing_timeout_secs: u64,

    /// Path to the hyphae registry data directory.
    #[arg(long, env = "DIMENSION_REGISTRY_PATH")]
    pub registry_path: Option<PathBuf>,

    /// Run in mock mode with MockEchoHandler (no real VMs).
    #[arg(long, default_value_t = false, env = "DIMENSION_MOCK")]
    pub mock: bool,

    /// Maximum concurrent VMs (default: 200).
    /// Per CONTEXT.md: hot-reloadable at runtime.
    #[arg(long, default_value_t = 200, env = "DIMENSION_MAX_CONCURRENT")]
    pub max_concurrent: usize,

    /// SSE heartbeat interval in seconds (default: 15).
    /// Sends SSE comments (`: heartbeat`) to prevent proxy/CDN idle timeouts.
    #[arg(long, default_value_t = 15, env = "DIMENSION_HEARTBEAT_INTERVAL")]
    pub heartbeat_interval_secs: u64,

    /// Maximum vCPUs a request can specify (default: 8).
    /// Requests exceeding this value are rejected with 400.
    #[arg(long, default_value_t = 8, env = "DIMENSION_MAX_VCPUS")]
    pub max_vcpus: u32,

    /// Maximum memory (MiB) a request can specify (default: 8192).
    /// Requests exceeding this value are rejected with 400.
    #[arg(long, default_value_t = 8192, env = "DIMENSION_MAX_MEMORY_MIB")]
    pub max_memory_mib: u32,

    /// Maximum disk size (MiB) a request can specify (default: 65536).
    /// Requests exceeding this value are rejected with 400.
    #[arg(long, default_value_t = 65536, env = "DIMENSION_MAX_DISK_SIZE_MIB")]
    pub max_disk_size_mib: u32,

    /// Enable networking for guest VMs (TAP device + NAT/masquerade).
    /// When enabled, VMs get outbound internet access via iptables masquerading.
    /// Requires root privileges and `ip`/`iptables` commands on the host.
    #[arg(long, default_value_t = false, env = "DIMENSION_ENABLE_NETWORK")]
    pub enable_network: bool,

    /// Drain timeout in seconds for graceful shutdown. Active requests are given
    /// this long to complete before streams are force-closed. (default: 60)
    #[arg(long = "drain-timeout", default_value_t = 60, env = "DIMENSION_DRAIN_TIMEOUT_SECS")]
    pub drain_timeout_secs: u64,

    /// PostgreSQL connection string for the dimension-store database.
    #[arg(long, env = "DATABASE_URL")]
    pub database_url: String,

    /// Vault configuration (AppRole auth, renewal interval, VM token TTL).
    #[command(flatten)]
    pub vault: crate::vault::VaultConfig,

    /// MinIO storage configuration (endpoint, bucket, access credentials).
    #[command(flatten)]
    pub storage: crate::storage::StorageConfig,

    /// Maximum concurrent running tasks per user (default: 5).
    /// Enforced at task creation and in the scheduler tick.
    #[arg(long, default_value_t = 5, env = "DIMENSION_MAX_CONCURRENT_TASKS_PER_USER")]
    pub max_concurrent_tasks_per_user: i64,

    // ── Multi-host configuration ───────────────────────────────────────

    /// Enable multi-host mode. When true, VM execution is delegated to
    /// registered worker nodes via gRPC. When false (default), VMs run
    /// locally on the gateway host (backward-compatible single-host mode).
    #[arg(long, default_value_t = false, env = "DIMENSION_MULTI_HOST")]
    pub multi_host: bool,

    /// Health poll interval in seconds for checking worker availability.
    /// Workers that fail 3 consecutive polls are removed from the registry.
    /// (default: 15)
    #[arg(long, default_value_t = 15, env = "DIMENSION_WORKER_HEALTH_INTERVAL_SECS")]
    pub worker_health_interval_secs: u64,

    // ── Invocation observability (Clickhouse + log MinIO) ──────────────

    /// Clickhouse HTTP URL for invocation telemetry queries.
    #[arg(long, default_value = "http://localhost:8123", env = "CLICKHOUSE_URL")]
    pub clickhouse_url: String,

    /// Clickhouse database name for invocation telemetry.
    #[arg(long, default_value = "dimension", env = "CLICKHOUSE_DATABASE")]
    pub clickhouse_database: String,

    /// MinIO endpoint for invocation log storage (separate from bundle storage).
    #[arg(long, default_value = "http://127.0.0.1:9000", env = "LOG_MINIO_ENDPOINT")]
    pub log_minio_endpoint: String,

    /// MinIO bucket for invocation logs.
    #[arg(long, default_value = "dimension-logs", env = "LOG_MINIO_BUCKET")]
    pub log_minio_bucket: String,

    /// MinIO access key for invocation log storage.
    #[arg(long, env = "LOG_MINIO_ACCESS_KEY")]
    pub log_minio_access_key: Option<String>,

    /// MinIO secret key for invocation log storage.
    #[arg(long, env = "LOG_MINIO_SECRET_KEY")]
    pub log_minio_secret_key: Option<String>,

    // ── Pulsar (run-event consumer) ────────────────────────────────────

    /// Pulsar broker URL. When unset the gateway falls back to the worker's
    /// gRPC SubscribeRunEvents RPC for live tailing.
    #[arg(long, env = "PULSAR_URL")]
    pub pulsar_url: Option<String>,

    /// Pulsar topic for run events (must match the worker).
    #[arg(
        long,
        env = "PULSAR_TOPIC",
        default_value = "persistent://dimension/events/runs"
    )]
    pub pulsar_topic: String,
}

impl AppConfig {
    /// Build an [`OrchestrationConfig`] from the CLI/env configuration.
    pub fn orchestration_config(&self) -> OrchestrationConfig {
        OrchestrationConfig {
            boot_timeout: Duration::from_secs(self.boot_timeout_secs),
            processing_timeout: Duration::from_secs(self.processing_timeout_secs),
            max_boot_timeout: Duration::from_secs(self.max_boot_timeout_secs),
            max_processing_timeout: Duration::from_secs(self.max_processing_timeout_secs),
            kernel_path: self.kernel_path.clone(),
            firecracker_bin: self.firecracker_bin.clone(),
            mock: self.mock,
            enable_network: self.enable_network,
            // Default to suppressing guest stderr (existing behavior).
            // Future: expose as CLI flag if needed.
            suppress_guest_stderr: true,
            jailer_bin: None,
            chroot_base_dir: PathBuf::from("/srv/jailer"),
        }
    }

    /// Resolve the registry data directory path.
    ///
    /// Uses the explicitly configured path if provided, otherwise falls
    /// back to hyphae's default data directory.
    pub fn resolved_registry_path(&self) -> PathBuf {
        if let Some(ref path) = self.registry_path {
            path.clone()
        } else {
            hyphae_core::registry::default_data_dir()
                .unwrap_or_else(|_| PathBuf::from("/var/lib/hyphae"))
        }
    }

    /// Build [`ResourceCaps`](crate::models::request::ResourceCaps) from
    /// the operator configuration for use with
    /// [`validate_resources`](crate::models::request::validate_resources).
    pub fn resource_caps(&self) -> crate::models::request::ResourceCaps {
        crate::models::request::ResourceCaps {
            max_vcpus: self.max_vcpus,
            max_memory_mib: self.max_memory_mib,
            max_disk_size_mib: self.max_disk_size_mib,
        }
    }
}

/// Retry configuration for vsock connection attempts.
///
/// Handles the VM boot race: after Orchestrator::run() returns,
/// the guest agent needs time to start listening on its vsock port.
/// The connector retries with exponential backoff until connected
/// or the timeout is exceeded.
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Initial delay between connection attempts.
    pub initial_delay: Duration,
    /// Maximum delay cap (backoff stops growing beyond this).
    pub max_delay: Duration,
    /// Overall timeout for all retry attempts.
    pub timeout: Duration,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            initial_delay: Duration::from_millis(10),
            max_delay: Duration::from_millis(500),
            timeout: Duration::from_secs(30),
        }
    }
}

/// Configuration for establishing a vsock connection to a VM.
#[derive(Debug, Clone)]
pub struct VsockConnectConfig {
    /// Path to the vsock Unix domain socket created by Firecracker.
    pub uds_path: String,
    /// Vsock port number the guest agent is listening on.
    pub port: u32,
    /// Retry/backoff configuration.
    pub retry: RetryConfig,
}
