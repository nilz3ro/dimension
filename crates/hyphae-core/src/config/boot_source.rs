use serde::{Deserialize, Serialize};

/// Boot source configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootSource {
    pub kernel_image_path: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub boot_args: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub initrd_path: Option<String>,
}
