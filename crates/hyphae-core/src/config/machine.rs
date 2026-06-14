use serde::{Deserialize, Serialize};

/// Machine resource configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineConfig {
    pub vcpu_count: u8,
    pub mem_size_mib: u64,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub smt: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub track_dirty_pages: Option<bool>,
}
