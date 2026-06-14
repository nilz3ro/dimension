//! VM lifecycle orchestration.
//!
//! This module contains the infrastructure for managing the full lifecycle
//! of a Firecracker microVM per request: spawn, connect, forward, stream,
//! and teardown.
//!
//! - [`config`]: Timeout configuration and CID allocation
//! - [`deployment`]: Deployment orchestration

pub mod config;
pub mod deployment;

pub use config::{CidAllocator, OrchestrationConfig};
