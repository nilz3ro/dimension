//! MMDS (MicroVM Metadata Service) client module.
//!
//! Provides functions for configuring and pushing metadata to Firecracker
//! VMs via the MMDS API. Metadata is set once before boot and is immutable
//! after the VM starts.
//!
//! # Usage
//!
//! 1. Call [`configure_mmds`] to set the MMDS version, network interface,
//!    and IPv4 address on the Firecracker API socket.
//! 2. Call [`set_metadata`] to push arbitrary JSON metadata before boot.
//! 3. Optionally call [`get_metadata`] to verify metadata from the host side.
//!
//! All communication uses raw HTTP over Unix domain sockets (see [`client`]).

pub mod client;

use std::path::Path;

use hyphae_errors::MmdsError;
use serde::{Deserialize, Serialize};

/// Maximum MMDS data store size in bytes (Firecracker default).
const MMDS_MAX_SIZE_BYTES: usize = 51_200;

/// MMDS protocol version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MmdsVersion {
    V1,
    V2,
}

impl MmdsVersion {
    fn as_str(&self) -> &'static str {
        match self {
            MmdsVersion::V1 => "V1",
            MmdsVersion::V2 => "V2",
        }
    }
}

/// Configuration for MMDS on a Firecracker VM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MmdsConfig {
    /// Network interface ID that MMDS should be accessible through.
    pub network_interface: String,
    /// MMDS protocol version. V2 (session tokens) is recommended.
    pub version: MmdsVersion,
    /// IPv4 address for the MMDS endpoint. Defaults to 169.254.169.254.
    pub ipv4_address: Option<String>,
}

impl Default for MmdsConfig {
    fn default() -> Self {
        Self {
            network_interface: "eth0".to_string(),
            version: MmdsVersion::V2,
            ipv4_address: None,
        }
    }
}

/// Configure MMDS on a Firecracker VM via the API socket.
///
/// Must be called before `InstanceStart` (before the VM boots) and before
/// [`set_metadata`]. Sends a PUT to `/mmds/config` with the MMDS version,
/// network interface, and IPv4 address.
///
/// `socket_path` is the path to the Firecracker API Unix socket.
/// `config` specifies the MMDS version, network interface, and optional
/// IPv4 address.
pub async fn configure_mmds(
    socket_path: &Path,
    config: &MmdsConfig,
) -> Result<(), MmdsError> {
    let ipv4 = config
        .ipv4_address
        .as_deref()
        .unwrap_or("169.254.169.254");

    let body = serde_json::json!({
        "network_interfaces": [&config.network_interface],
        "version": config.version.as_str(),
        "ipv4_address": ipv4,
    });

    let body_str = serde_json::to_string(&body)
        .map_err(|e| MmdsError::SerializationError(e.to_string()))?;

    client::send_mmds_request(socket_path, "PUT", "/mmds/config", &body_str).await
}

/// Push metadata to a Firecracker VM via the MMDS API.
///
/// Replaces all existing metadata with the provided JSON value. Must be
/// called after [`configure_mmds`] and before `InstanceStart` (before the
/// VM boots). Metadata is set once and is immutable after boot.
///
/// The metadata can be any nested JSON structure. Example:
/// ```json
/// {
///   "app": { "env": "production", "version": "1.2.3" },
///   "vm": { "id": "abc-123", "hostname": "worker-1" }
/// }
/// ```
///
/// Returns an error if the serialized metadata exceeds Firecracker's
/// 51,200-byte MMDS size limit. Size validation happens **before** sending
/// the request to the API socket.
pub async fn set_metadata(
    socket_path: &Path,
    metadata: &serde_json::Value,
) -> Result<(), MmdsError> {
    let body = serde_json::to_string(metadata)
        .map_err(|e| MmdsError::SerializationError(e.to_string()))?;

    // Validate size against Firecracker's MMDS limit BEFORE sending
    if body.len() > MMDS_MAX_SIZE_BYTES {
        return Err(MmdsError::MetadataTooLarge {
            actual_bytes: body.len(),
        });
    }

    client::send_mmds_request(socket_path, "PUT", "/mmds", &body).await
}

/// Retrieve current MMDS metadata from a Firecracker VM via the API socket.
///
/// This is a **host-side** operation that reads metadata from the Firecracker
/// API socket (not from inside the guest). Guest-side access uses HTTP GET
/// to the MMDS IPv4 address (169.254.169.254) from within the VM.
///
/// Returns the metadata as a [`serde_json::Value`].
pub async fn get_metadata(socket_path: &Path) -> Result<serde_json::Value, MmdsError> {
    let response_body =
        client::send_mmds_request_with_body(socket_path, "GET", "/mmds", "").await?;

    serde_json::from_str(&response_body)
        .map_err(|e| MmdsError::DeserializationError(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mmds_config_default_uses_v2() {
        let config = MmdsConfig::default();
        assert_eq!(config.version, MmdsVersion::V2);
        assert_eq!(config.network_interface, "eth0");
        assert!(config.ipv4_address.is_none());
    }

    #[test]
    fn mmds_version_as_str() {
        assert_eq!(MmdsVersion::V1.as_str(), "V1");
        assert_eq!(MmdsVersion::V2.as_str(), "V2");
    }

    #[test]
    fn mmds_max_size_constant() {
        assert_eq!(MMDS_MAX_SIZE_BYTES, 51_200);
    }
}
