//! Multi-host worker management for the dimension gateway.
//!
//! This module provides the infrastructure for distributing VM execution
//! across multiple worker nodes:
//!
//! - [`registry`]: In-memory worker state tracking
//! - [`scheduler`]: Resource-aware worker selection

// Include generated gRPC client code for the worker service.
pub mod worker_proto {
    tonic::include_proto!("dimension.worker");
}

pub mod registry;
pub mod scheduler;

pub use registry::{WorkerRegistry, WorkerState};
pub use scheduler::pick_worker;
