//! Process lifecycle type definitions.
//!
//! All shared types for process state tracking, spawn configuration,
//! shutdown escalation, and reporting.

use std::time::{Duration, Instant};

/// State of a Firecracker VM process.
#[derive(Debug, Clone, PartialEq)]
pub enum ProcessState {
    Starting,
    Running,
    Exited(ExitInfo),
}

/// Information about a process exit.
#[derive(Debug, Clone, PartialEq)]
pub struct ExitInfo {
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub timestamp: Instant,
}

/// Configuration for spawning a Firecracker process.
#[derive(Debug, Clone)]
pub struct SpawnConfig {
    /// Path to the firecracker binary.
    pub firecracker_bin: std::path::PathBuf,
    /// Path to a Firecracker JSON config file (--config-file).
    pub config_file: Option<std::path::PathBuf>,
    /// Override for runtime base directory. If None, uses default XDG/env resolution.
    pub runtime_base_dir: Option<std::path::PathBuf>,
    /// Override path for the console log file. If None, defaults to
    /// `{runtime_dir}/console.log` so Firecracker stdout/stderr is always captured.
    pub log_file: Option<std::path::PathBuf>,
}

/// Configuration for graceful shutdown escalation.
#[derive(Debug, Clone)]
pub struct ShutdownConfig {
    /// Timeout after SendCtrlAltDel before escalating to SIGTERM.
    pub ctrl_alt_del_timeout: Duration,
    /// Timeout after SIGTERM before escalating to SIGKILL.
    pub sigterm_timeout: Duration,
    /// Timeout after SIGKILL before giving up.
    pub sigkill_timeout: Duration,
}

impl Default for ShutdownConfig {
    fn default() -> Self {
        Self {
            ctrl_alt_del_timeout: Duration::from_secs(5),
            sigterm_timeout: Duration::from_secs(10),
            sigkill_timeout: Duration::from_secs(5),
        }
    }
}

/// Report from a shutdown operation.
#[derive(Debug, Clone)]
pub struct ShutdownReport {
    /// Which escalation stage ultimately stopped the process.
    pub final_stage: ShutdownStage,
    /// Total wall-clock time for the shutdown.
    pub duration: Duration,
}

/// Escalation stage that terminated the process.
#[derive(Debug, Clone, PartialEq)]
pub enum ShutdownStage {
    CtrlAltDel,
    Sigterm,
    Sigkill,
    AlreadyExited,
}
