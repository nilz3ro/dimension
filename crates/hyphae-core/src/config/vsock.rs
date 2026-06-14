use serde::{Deserialize, Serialize};

/// Vsock device configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VsockConfig {
    pub guest_cid: u32,
    pub uds_path: String,
}
