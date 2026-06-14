use serde::{Deserialize, Serialize};

/// Block device (drive) configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriveConfig {
    pub drive_id: String,
    pub path_on_host: String,
    pub is_root_device: bool,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_read_only: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_type: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub io_engine: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub partuuid: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_limiter: Option<serde_json::Value>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub socket: Option<String>,
}

impl DriveConfig {
    /// Create a new drive configuration.
    ///
    /// - `drive_id`: unique identifier for this drive
    /// - `path_on_host`: path to the drive image on the host
    /// - `is_read_only`: whether the drive is read-only
    ///
    /// The drive defaults to `is_root_device: false`. The rootfs drive
    /// is only created internally by [`VmConfig::new()`](super::VmConfig::new).
    pub fn new(
        drive_id: impl Into<String>,
        path_on_host: impl Into<String>,
        is_read_only: bool,
    ) -> Self {
        DriveConfig {
            drive_id: drive_id.into(),
            path_on_host: path_on_host.into(),
            is_root_device: false,
            is_read_only: Some(is_read_only),
            cache_type: None,
            io_engine: None,
            partuuid: None,
            rate_limiter: None,
            socket: None,
        }
    }
}
