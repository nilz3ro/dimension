//! Core library for the hyphae Firecracker microVM manager.
#![allow(
    clippy::collapsible_if,
    clippy::manual_is_multiple_of,
    clippy::new_without_default,
    clippy::manual_div_ceil
)]

pub mod config;
pub mod jail;
pub mod kernel;
pub mod launch;
pub mod manifest;
pub mod mmds;
pub mod net;
pub mod orchestrator;
pub mod prereq;
pub mod process;
pub mod registry;
pub mod rootfs;
pub mod volume;
