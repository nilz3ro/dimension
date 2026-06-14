//! Vault integration: AppRole auth, KV v2, Transit, VM token minting.

pub mod auth;
pub mod client;
pub mod config;

pub use client::{VaultClient, VaultError, VaultHealthStatus};
pub use config::VaultConfig;
