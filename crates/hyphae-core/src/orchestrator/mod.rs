//! Orchestrator: composes subsystem calls into build, run, and stop pipelines.
//!
//! The [`Orchestrator`] struct is the integration layer that all CLI commands
//! call. It owns a [`Registry`] and delegates to the rootfs, config, and
//! process subsystems. Getting the orchestrator right means the CLI layer
//! remains thin -- parse arguments, call orchestrator, format output.

pub mod build;
pub mod run;
pub mod stop;
pub mod types;

pub use types::{
    ActionType, BuildPlan, BuildRequest, BuildResult, CacheStatus, PlannedAction, ProgressFn,
    RunPlan, RunRequest, RunResult, StopPlan, VsockRunConfig,
};

use crate::registry::Registry;

/// The orchestrator composes all subsystem calls into high-level pipelines.
///
/// Owns a [`Registry`] for image and VM tracking. Build, run, and stop
/// methods are implemented in their respective submodules.
pub struct Orchestrator {
    registry: Registry,
}

impl Orchestrator {
    /// Create a new orchestrator with the given registry.
    pub fn new(registry: Registry) -> Self {
        Self { registry }
    }

    /// Borrow the underlying registry.
    pub fn registry(&self) -> &Registry {
        &self.registry
    }
}
