//! Process lifecycle management for Firecracker VMs.
//!
//! This module provides types, runtime directory management, spawning,
//! monitoring, and process handle for Firecracker processes.

pub mod handle;
pub mod monitor;
pub mod orphan;
pub mod runtime;
pub mod shutdown;
pub mod spawn;
pub mod types;

pub use handle::VmProcess;
pub use orphan::{recover_orphans, RecoveryReport};
pub use runtime::{cleanup_runtime_dir_sync, create_vm_runtime_dir, runtime_base_dir, vms_dir};
pub use spawn::spawn;
pub use types::{
    ExitInfo, ProcessState, ShutdownConfig, ShutdownReport, ShutdownStage, SpawnConfig,
};
