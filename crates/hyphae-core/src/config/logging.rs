use serde::{Deserialize, Serialize};

/// Logger configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggerConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_path: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_level: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_log_origin: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
}

/// Metrics configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsConfig {
    pub metrics_path: String,
}
