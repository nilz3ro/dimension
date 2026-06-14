//! Worker configuration parsed from CLI flags and environment variables.

/// Configuration for a dimension-worker node.
#[derive(Debug, Clone, clap::Parser)]
#[command(name = "dimension-worker", about = "Dimension worker gRPC server")]
pub struct WorkerConfig {
    /// Gateway URL for registration (e.g., http://gateway:8080).
    #[arg(long, env = "DIMENSION_GATEWAY_URL")]
    pub gateway_url: String,

    /// gRPC listen address for this worker (host:port).
    #[arg(long, env = "DIMENSION_WORKER_GRPC_ADDR", default_value = "0.0.0.0:50051")]
    pub grpc_addr: String,

    /// Advertised gRPC address (what the gateway uses to reach this worker).
    /// Defaults to grpc_addr when not set.
    #[arg(long, env = "DIMENSION_WORKER_ADVERTISE_ADDR")]
    pub advertise_addr: Option<String>,

    /// Total memory available for VMs in megabytes.
    #[arg(long, env = "DIMENSION_WORKER_MEMORY_MB", default_value_t = 4096)]
    pub memory_mb: u64,

    /// Total vCPUs available for VMs.
    #[arg(long, env = "DIMENSION_WORKER_VCPUS", default_value_t = 4)]
    pub vcpus: u32,

    /// Heartbeat interval in seconds. After initial registration, the worker
    /// re-POSTs to the gateway registration endpoint at this interval to keep
    /// its record fresh. Must be well under 60 seconds.
    #[arg(long, default_value_t = 15, env = "DIMENSION_WORKER_HEARTBEAT_INTERVAL_SECS")]
    pub heartbeat_interval_secs: u64,

    // ── Forwarded gateway/orchestration config ─────────────────────────────────

    /// Path to the kernel binary used for VM boot.
    #[arg(long, default_value = "/opt/hyphae/kernel/vmlinux", env = "DIMENSION_KERNEL_PATH")]
    pub kernel_path: std::path::PathBuf,

    /// Path to the Firecracker binary.
    #[arg(long, default_value = "firecracker", env = "DIMENSION_FIRECRACKER_BIN")]
    pub firecracker_bin: std::path::PathBuf,

    /// Default boot timeout in seconds.
    #[arg(long, default_value_t = 30, env = "DIMENSION_BOOT_TIMEOUT_SECS")]
    pub boot_timeout_secs: u64,

    /// Default processing timeout in seconds.
    #[arg(long, default_value_t = 300, env = "DIMENSION_PROCESSING_TIMEOUT_SECS")]
    pub processing_timeout_secs: u64,

    /// Maximum allowed boot timeout in seconds (server-enforced ceiling).
    #[arg(long, default_value_t = 60, env = "DIMENSION_MAX_BOOT_TIMEOUT_SECS")]
    pub max_boot_timeout_secs: u64,

    /// Maximum allowed processing timeout in seconds.
    #[arg(long, default_value_t = 600, env = "DIMENSION_MAX_PROCESSING_TIMEOUT_SECS")]
    pub max_processing_timeout_secs: u64,

    /// Path to the hyphae registry data directory.
    #[arg(long, env = "DIMENSION_REGISTRY_PATH")]
    pub registry_path: Option<std::path::PathBuf>,

    /// Enable networking for guest VMs.
    #[arg(long, default_value_t = false, env = "DIMENSION_ENABLE_NETWORK")]
    pub enable_network: bool,

    /// PostgreSQL connection URL for platform dispatch stores.
    /// When set, the worker connects to Postgres on startup and constructs
    /// a DispatchContext so that guest ServiceRequests (webhooks, session
    /// events, etc.) are handled directly instead of failing with
    /// "dispatch not configured".
    #[arg(long, env = "DATABASE_URL")]
    pub database_url: Option<String>,

    // ── Clickhouse observability ───────────────────────────────────────

    /// Clickhouse HTTP URL for invocation records.
    #[arg(long, env = "CLICKHOUSE_URL", default_value = "http://localhost:8123")]
    pub clickhouse_url: String,

    /// Clickhouse database name.
    #[arg(long, env = "CLICKHOUSE_DATABASE", default_value = "dimension")]
    pub clickhouse_database: String,

    // ── MinIO log upload ───────────────────────────────────────────────

    /// MinIO endpoint for invocation log uploads.
    #[arg(long, env = "LOG_MINIO_ENDPOINT")]
    pub log_minio_endpoint: Option<String>,

    /// MinIO bucket for invocation logs.
    #[arg(long, env = "LOG_MINIO_BUCKET", default_value = "dimension-logs")]
    pub log_minio_bucket: String,

    /// MinIO access key for invocation log uploads.
    #[arg(long, env = "LOG_MINIO_ACCESS_KEY")]
    pub log_minio_access_key: Option<String>,

    /// MinIO secret key for invocation log uploads.
    #[arg(long, env = "LOG_MINIO_SECRET_KEY")]
    pub log_minio_secret_key: Option<String>,

    // ── Pulsar (outbound message center) ───────────────────────────────

    /// Pulsar broker URL (e.g. `pulsar://broker:6650`). When unset, run
    /// events are still persisted to Clickhouse and exposed via the
    /// worker's in-process broadcast, but no Pulsar publish happens.
    #[arg(long, env = "PULSAR_URL")]
    pub pulsar_url: Option<String>,

    /// Pulsar topic for run events. Messages are keyed by run_id so a
    /// single multi-partition topic keeps per-run ordering.
    #[arg(
        long,
        env = "PULSAR_TOPIC",
        default_value = "persistent://dimension/events/runs"
    )]
    pub pulsar_topic: String,
}

impl WorkerConfig {
    /// Returns the effective advertised gRPC address (falls back to grpc_addr).
    pub fn effective_advertise_addr(&self) -> &str {
        self.advertise_addr
            .as_deref()
            .unwrap_or(&self.grpc_addr)
    }

    /// Resolve the registry data directory path.
    pub fn resolved_registry_path(&self) -> std::path::PathBuf {
        if let Some(ref path) = self.registry_path {
            path.clone()
        } else {
            hyphae_core::registry::default_data_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from("/var/lib/hyphae"))
        }
    }
}
