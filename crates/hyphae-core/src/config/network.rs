use serde::{Deserialize, Serialize};

/// Network interface configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkInterface {
    pub iface_id: String,
    pub host_dev_name: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub guest_mac: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub rx_rate_limiter: Option<serde_json::Value>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub tx_rate_limiter: Option<serde_json::Value>,
}
