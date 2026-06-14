//! Shared error types for the hyphae project.
//!
//! # Error Code Numbering Scheme
//!
//! Error codes follow the pattern `E{subsystem}{number}`:
//!
//! | Range  | Subsystem   | Phase |
//! |--------|-------------|-------|
//! | E0xx   | General     | 1+    |
//! | E1xx   | Prereq      | 1     |
//! | E2xx   | Config      | 2     |
//! | E3xx   | Process     | 3     |
//! | E4xx   | Rootfs      | 4     |
//! | E5xx   | Registry    | 5     |
//! | E6xx   | Orchestrator| 6     |
//! | E7xx   | Kernel/MMDS | 7     |
//! | E8xx   | Network/Jail| 8     |
//!
//! Each subsystem owns its range. New variants are appended with the next
//! available number within that range. Codes are stable once assigned --
//! never reuse a retired code.

use std::path::PathBuf;
use thiserror::Error;

/// Errors from prerequisite checks (E1xx range).
#[derive(Error, Debug)]
pub enum PrereqError {
    #[error("E101: /dev/kvm is not accessible: {reason}")]
    KvmNotAccessible { reason: String },

    #[error("E102: firecracker binary not found in PATH")]
    FirecrackerNotFound,

    #[error("E103: mkfs.ext4 binary not found in PATH")]
    MkfsNotFound,

    #[error("E104: /dev/kvm does not exist (KVM kernel module not loaded)")]
    KvmNotPresent,

    #[error("E105: /dev/kvm permission denied (user not in kvm group?)")]
    KvmPermissionDenied,
}

/// A single config validation failure with full context.
#[derive(Error, Debug, Clone)]
pub enum ConfigValidationError {
    #[error("E201: vcpu_count must be 1-32 and either 1 or even, got {value}")]
    InvalidVcpuCount { value: u8 },

    #[error("E202: mem_size_mib must be >= 8, got {value}")]
    InvalidMemSize { value: u64 },

    #[error("E203: vsock guest_cid must be >= 3, got {value}")]
    InvalidGuestCid { value: u32 },

    #[error("E204: drive '{drive_id}' has no path_on_host")]
    MissingDrivePath { drive_id: String },

    #[error("E205: duplicate drive_id '{drive_id}'")]
    DuplicateDriveId { drive_id: String },

    #[error("E206: no root device found in drives")]
    NoRootDevice,
}

/// Config-level errors.
#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("E200: config validation failed:\n{}", format_validation_errors(.0))]
    ValidationFailed(Vec<ConfigValidationError>),

    #[error("E210: JSON serialization failed: {0}")]
    Serialization(String),
}

fn format_validation_errors(errors: &[ConfigValidationError]) -> String {
    errors.iter().map(|e| format!("  - {e}")).collect::<Vec<_>>().join("\n")
}

/// Errors from process lifecycle management (E3xx range).
#[derive(Error, Debug)]
pub enum ProcessError {
    #[error("[E300] failed to spawn firecracker process: {0}")]
    SpawnFailed(String),

    #[error("[E301] PID file error: {0}")]
    PidFileError(String),

    #[error("[E302] timed out waiting for API socket to become ready")]
    WaitReadyTimeout,

    #[error("[E303] shutdown failed: {0}")]
    ShutdownFailed(String),

    #[error("[E304] API socket error: {0}")]
    SocketError(String),

    #[error("[E305] orphan recovery error: {0}")]
    OrphanRecoveryError(String),

    #[error("[E306] runtime directory error: {0}")]
    RuntimeDirError(String),
}

/// Errors from rootfs building (E4xx range).
#[derive(Error, Debug)]
pub enum RootfsError {
    #[error("[E401] ambiguous project type at {path}: both package.json and Cargo.toml found -- remove one to resolve")]
    AmbiguousProject { path: PathBuf },

    #[error("[E402] unrecognized project at {path}: no package.json or Cargo.toml found -- supported types: JavaScript (package.json), Rust (Cargo.toml)")]
    UnrecognizedProject { path: PathBuf },

    #[error("[E403] failed to read package.json: {0}")]
    PackageJsonRead(#[source] std::io::Error),

    #[error("[E404] failed to parse package.json: {0}")]
    PackageJsonParse(String),

    #[error("[E405] no entrypoint detected in {project_dir} -- add scripts.start to package.json, set a main field, or create index.js")]
    NoEntrypoint { project_dir: PathBuf },

    #[error("[E406] cargo metadata failed: {0}")]
    CargoMetadata(String),

    #[error("[E407] no binary target found in {path}")]
    NoBinaryTarget { path: PathBuf },

    #[error("[E408] multiple binary targets in {path}: {names:?} -- set default-run in Cargo.toml to disambiguate")]
    MultipleBinaries { path: PathBuf, names: Vec<String> },

    #[error("[E409] cargo build failed:\n{stderr}")]
    CargoBuildFailed { stderr: String },

    #[error("[E410] expected binary not found at {expected}")]
    BinaryNotFound { expected: PathBuf },

    #[error("[E411] failed to execute mkfs.ext4: {0}")]
    MkfsExec(#[source] std::io::Error),

    #[error("[E412] mkfs.ext4 failed:\n{stderr}")]
    MkfsFailed { stderr: String },

    #[error("[E413] staging directory walk error: {0}")]
    StagingWalk(String),

    #[error("[E414] no root package found at {path}")]
    NoRootPackage { path: PathBuf },

    #[error("[E415] rootfs IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("[E416] cargo build execution failed: {0}")]
    CargoBuild(#[source] std::io::Error),

    #[error("[E417] Docker API error: {0}")]
    DockerApi(String),

    #[error("[E418] Docker export failed: {0}")]
    DockerExportFailed(String),

    #[error("[E419] Docker image has no entrypoint or cmd")]
    DockerNoEntrypoint,

    #[error("[E420] insufficient disk space: need {required} bytes but only {available} bytes available")]
    InsufficientDisk { required: u64, available: u64 },

    #[error("[E421] disk space check failed: {0}")]
    DiskSpaceCheck(String),
}

/// Errors from image registry operations (E5xx range).
#[derive(Error, Debug)]
pub enum RegistryError {
    #[error("[E501] cannot open registry database at {path}: {message}")]
    DatabaseOpen { path: PathBuf, message: String },

    #[error("[E502] WAL mode unavailable (got '{actual}' instead of 'wal')")]
    WalModeUnavailable { actual: String },

    #[error("[E503] database migration failed: {0}")]
    MigrationFailed(String),

    #[error("[E504] database integrity check failed: {details}")]
    IntegrityCheckFailed { details: String },

    #[error("[E505] image not found (id={id})")]
    ImageNotFound { id: i64 },

    #[error("[E506] cannot delete image (id={image_id}): {vm_count} VM(s) still running")]
    ImageInUse { image_id: i64, vm_count: u32 },

    #[error("[E507] failed to hash source files at {path}: {message}")]
    HashingFailed { path: PathBuf, message: String },

    #[error("[E508] cannot access storage directory {path}: {message}")]
    StorageAccess { path: PathBuf, message: String },

    #[error("[E509] failed to clean up image file {path}: {message}")]
    DiskCleanupFailed { path: PathBuf, message: String },

    #[error("[E510] cannot determine data directory (set HYPHAE_DATA_DIR or ensure HOME is set)")]
    NoDataDirectory,

    #[error("[E511] invalid source path: {path}")]
    InvalidSourcePath { path: PathBuf },

    #[error("[E512] image insert failed unexpectedly")]
    InsertFailed,

    #[error("[E513] database error: {0}")]
    Database(String),

    #[error("[E514] access denied: {message}")]
    AccessDenied { message: String },
}

/// Errors from orchestration pipelines (E6xx range).
#[derive(Error, Debug)]
pub enum OrchestratorError {
    #[error("[E601] bundle not found: {reference}")]
    BundleNotFound { reference: String },

    #[error("[E602] VM not found: {vm_id}")]
    VmNotFound { vm_id: String },

    #[error("[E603] kernel file not found at {path}")]
    KernelNotFound { path: PathBuf },

    #[error("[E604] project directory not found at {path}")]
    ProjectDirNotFound { path: PathBuf },

    #[error("[E605] build failed: {0}")]
    BuildFailed(String),

    #[error("[E606] VM spawn failed: {0}")]
    SpawnFailed(String),

    #[error("[E607] VM shutdown failed: {0}")]
    ShutdownFailed(String),

    #[error("[E608] config generation failed: {0}")]
    ConfigFailed(String),

    #[error("[E609] registry operation failed: {0}")]
    RegistryFailed(String),

    #[error("[E610] dry-run aborted: {0}")]
    DryRunFailed(String),
}

/// Errors from MMDS (MicroVM Metadata Service) operations (E711-E717).
#[derive(Error, Debug)]
pub enum MmdsError {
    #[error("[E711] failed to connect to Firecracker API socket at {path}: {message}")]
    SocketConnect { path: PathBuf, message: String },

    #[error("[E712] failed to write to Firecracker API socket: {0}")]
    SocketWrite(String),

    #[error("[E713] failed to read from Firecracker API socket: {0}")]
    SocketRead(String),

    #[error("[E714] MMDS API error on {endpoint}: {response}")]
    ApiError { endpoint: String, response: String },

    #[error("[E715] MMDS metadata exceeds Firecracker's 51,200-byte limit (actual: {actual_bytes} bytes)")]
    MetadataTooLarge { actual_bytes: usize },

    #[error("[E716] failed to serialize MMDS metadata: {0}")]
    SerializationError(String),

    #[error("[E717] failed to deserialize MMDS response: {0}")]
    DeserializationError(String),
}

/// Errors from kernel management (E7xx range).
#[derive(Error, Debug)]
pub enum KernelError {
    #[error("[E701] network error during kernel download: {message}")]
    NetworkError { message: String },

    #[error("[E702] kernel download failed: {url} returned HTTP {status}")]
    DownloadFailed { url: String, status: u16 },

    #[error("[E703] no kernels available for Firecracker {fc_version} on {arch}")]
    NoKernelsAvailable { fc_version: String, arch: String },

    #[error("[E704] kernel version {version} not found (available: {available:?})")]
    VersionNotFound {
        version: String,
        available: Vec<String>,
    },

    #[error("[E705] invalid kernel at {path}: {reason}")]
    InvalidKernel { path: PathBuf, reason: String },

    #[error("[E706] kernel not found: id={id}")]
    KernelNotFound { id: i64 },

    #[error("[E707] kernel file missing at {path} -- check your bundle config or use a managed kernel")]
    KernelFileMissing { path: PathBuf },

    #[error("[E708] kernel I/O error at {path}: {source}")]
    IoError {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("[E709] malformed S3 listing response")]
    MalformedS3Response,

    #[error("[E710] kernel registry error: {0}")]
    RegistryError(String),
}

/// Errors from networking subsystem (E8xx range).
#[derive(Error, Debug)]
pub enum NetworkError {
    #[error("[E801] ip command failed: ip {args}: {message}")]
    IpCommandFailed { args: String, message: String },

    #[error("[E802] insufficient privileges for {operation}: {hint}")]
    InsufficientPrivileges { operation: String, hint: String },

    #[error("[E803] subnet range exhausted -- all 16384 /30 subnets in 172.16.0.0/16 are allocated")]
    SubnetExhausted,

    #[error("[E804] iptables operation failed: {0}")]
    IptablesFailed(String),

    #[error("[E805] failed to enable IP forwarding: {0}")]
    IpForwardFailed(String),

    #[error("[E806] TAP device {name} already exists")]
    TapAlreadyExists { name: String },

    #[error("[E807] network setup failed for VM: {reason}")]
    NetworkSetupFailed { reason: String },

    #[error("[E808] failed to generate MAC address for TAP index {index}: index exceeds 24-bit range")]
    MacAddressOverflow { index: u32 },
}

/// Errors from jailer subsystem (E811-E820).
#[derive(Error, Debug)]
pub enum JailError {
    #[error("[E811] jailer binary not found at {path}")]
    JailerNotFound { path: PathBuf },

    #[error("[E812] failed to spawn jailer: {0}")]
    JailerSpawnFailed(String),

    #[error("[E813] hyphae system user not found -- {hint}")]
    UserNotFound { hint: String },

    #[error("[E814] failed to look up system user: {0}")]
    UserLookupFailed(String),

    #[error("[E815] jail directory creation failed at {path}: {reason}")]
    DirectoryCreation { path: PathBuf, reason: String },

    #[error("[E816] hard-link failed: {src} -> {dst}: {reason}")]
    HardLinkFailed {
        src: PathBuf,
        dst: PathBuf,
        reason: String,
    },

    #[error("[E817] cgroup setup failed: {0}")]
    CgroupSetupFailed(String),

    #[error("[E818] jail cleanup failed at {path}: {source}")]
    CleanupFailed {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("[E819] firecracker PID file not found at {path} within timeout")]
    PidFileTimeout { path: PathBuf },

    #[error("[E820] invalid PID in {path}: {content}")]
    InvalidPidFile { path: PathBuf, content: String },
}

/// Top-level error type wrapping all subsystem errors.
///
/// Each subsystem error enum gets a `#[from]` variant here so that
/// `?` propagation works transparently. New subsystem errors will be
/// added as additional variants in their respective phases.
#[derive(Error, Debug)]
pub enum HyphaeError {
    #[error(transparent)]
    Prereq(#[from] PrereqError),

    #[error(transparent)]
    Config(#[from] ConfigError),

    #[error(transparent)]
    Process(#[from] ProcessError),

    #[error(transparent)]
    Rootfs(#[from] RootfsError),

    #[error(transparent)]
    Registry(#[from] RegistryError),

    #[error(transparent)]
    Orchestrator(#[from] OrchestratorError),

    #[error(transparent)]
    Kernel(#[from] KernelError),

    #[error(transparent)]
    Mmds(#[from] MmdsError),

    #[error(transparent)]
    Network(#[from] NetworkError),

    #[error(transparent)]
    Jail(#[from] JailError),

    #[error("E001: {0}")]
    Other(String),
}
