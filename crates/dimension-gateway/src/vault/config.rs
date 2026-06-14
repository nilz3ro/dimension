//! Vault configuration parsed from CLI flags with env var fallback.

/// Configuration for the HashiCorp Vault client.
#[derive(Debug, Clone, clap::Args)]
pub struct VaultConfig {
    /// Vault server URL.
    #[arg(long, default_value = "http://127.0.0.1:8200", env = "VAULT_ADDR")]
    pub vault_url: String,

    /// AppRole role ID for authentication.
    #[arg(long, env = "VAULT_ROLE_ID")]
    pub vault_role_id: Option<String>,

    /// AppRole secret ID for authentication.
    #[arg(long, env = "VAULT_SECRET_ID")]
    pub vault_secret_id: Option<String>,

    /// Token renewal interval in seconds (default: 900 = 15 minutes).
    #[arg(long, default_value_t = 900, env = "VAULT_RENEWAL_INTERVAL_SECS")]
    pub vault_renewal_interval_secs: u64,

    /// TTL in seconds for per-VM scoped tokens (default: 3600 = 1 hour).
    #[arg(long, default_value_t = 3600, env = "VAULT_VM_TOKEN_TTL_SECS")]
    pub vault_vm_token_ttl_secs: u64,
}
