//! Types for the `dimension.toml` bundle manifest.
//!
//! The manifest is an optional declarative file at the root of a bundle archive.
//! All sections are optional; missing sections use safe defaults (empty vecs, all
//! capabilities off, no env vars, no resources override).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Top-level manifest structure for `dimension.toml`.
///
/// `deny_unknown_fields` causes unknown top-level sections to produce a parse
/// error with line/column information. This prevents silent misconfiguration
/// from typos in section names.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct DimensionManifest {
    /// VM resource requirements declared by the bundle author.
    #[serde(default)]
    pub resources: ResourcesSection,

    /// Runtime environment variables injected at launch time (not baked into
    /// the image). These are applied after Docker ENV via kernel boot args.
    #[serde(default)]
    pub env: EnvSection,

    /// Named secrets the bundle requires. Values are never stored here --
    /// only the names are declared so the platform can verify availability.
    #[serde(default)]
    pub secrets: SecretsSection,

    /// Capability gates: all default to `false` (default-deny policy).
    /// Capabilities must be explicitly enabled in the manifest.
    #[serde(default)]
    pub capabilities: CapabilitiesSection,

    /// Agent-to-Agent (A2A) metadata for agent discovery and routing.
    #[serde(default)]
    pub a2a: A2aSection,

    /// Persistent volume configuration.
    ///
    /// When present, the platform attaches a persistent ext4 volume at
    /// `/workspace` before the agent starts. The volume persists across
    /// sessions and is owned exclusively by this bundle's session.
    #[serde(default)]
    pub volumes: VolumesSection,
}

/// Resource requirements for the VM.
///
/// All fields are `Option` -- `None` means "use server default".
/// Resolution to concrete values happens at launch time.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ResourcesSection {
    /// Memory in megabytes. Server default is 256 MiB if unset.
    pub memory_mb: Option<u32>,

    /// Number of vCPUs. Server default is 2 if unset.
    pub vcpus: Option<u8>,

    /// Maximum execution time in seconds before the VM is terminated.
    /// Server default applies if unset.
    pub timeout_secs: Option<u64>,
}

/// Runtime environment variables.
///
/// Uses `#[serde(flatten)]` to allow arbitrary `KEY = "VALUE"` pairs at the
/// TOML section level. NOTE: `deny_unknown_fields` must NOT be applied to this
/// struct -- it conflicts with `flatten`. Validation of key names happens at
/// injection time.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EnvSection {
    #[serde(flatten)]
    pub vars: HashMap<String, String>,
}

impl EnvSection {
    /// Expand `${VAR_NAME}` references in env values from the host environment.
    ///
    /// A value that is exactly `${FOO}` is replaced with the value of the `FOO`
    /// environment variable on the host. If the variable is unset, the value
    /// becomes an empty string. Values that don't match the `${...}` pattern
    /// are left unchanged.
    pub fn resolve_host_env(&mut self) {
        for value in self.vars.values_mut() {
            let trimmed = value.trim();
            if trimmed.starts_with("${") && trimmed.ends_with('}') && trimmed.len() > 3 {
                let var_name = &trimmed[2..trimmed.len() - 1];
                *value = std::env::var(var_name).unwrap_or_default();
            }
        }
    }
}

/// Named secrets the bundle declares it needs.
///
/// Only names are stored -- values are injected at runtime via the secrets
/// service (Phase 8). Declaring a secret name here allows the platform to
/// fail fast if a required secret is unavailable.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct SecretsSection {
    /// List of secret names required by this bundle.
    #[serde(default)]
    pub names: Vec<String>,
}

/// Capability gates for the bundle.
///
/// All capabilities default to `false` (default-deny). The bundle author must
/// explicitly opt in to each capability they need.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct CapabilitiesSection {
    /// Access to named secrets via the secrets service (Phase 8).
    #[serde(default)]
    pub secrets: bool,

    /// Access to persistent object storage (Phase 9).
    #[serde(default)]
    pub storage: bool,

    /// Ability to call other agents (A2A, Phase 11).
    #[serde(default)]
    pub agent_calls: bool,

    /// Access to the tokenization/detokenization service (Phase 8).
    /// Enables POST /tokenize and POST /detokenize endpoints.
    #[serde(default)]
    pub tokenize: bool,
}

/// Agent-to-Agent metadata for agent discovery.
///
/// Used by downstream phases (Phase 11) to build the A2A agent card.
/// All fields are optional; a bundle without an `[a2a]` section will not
/// be visible in the agent registry.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct A2aSection {
    /// Human-readable name for this agent in the A2A registry.
    pub name: Option<String>,

    /// Human-readable description of what this agent does.
    pub description: Option<String>,

    /// Skill identifiers this agent advertises.
    #[serde(default)]
    pub skills: Vec<String>,
}

/// Persistent volume configuration for the bundle.
///
/// Bundles with a `[volumes]` section get a persistent ext4 volume
/// attached at `/workspace` before the agent starts. The volume persists
/// across sessions and carries data between turns.
///
/// If `[volumes]` is absent (or `size` is not set), no volume is attached
/// and the agent runs ephemerally.
///
/// `shared_mount` and `size` are mutually exclusive at runtime: if both are
/// set, `shared_mount` takes precedence and a warning is logged.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct VolumesSection {
    /// Volume size as a human-readable string (e.g. `"10GB"`, `"512MB"`).
    ///
    /// Parsed and used by the volume creation pipeline (Phase 17) when
    /// provisioning the volume image for the first time.
    /// If `None`, the platform applies a server-default size.
    pub size: Option<String>,

    /// Name of a user-owned named volume to mount at /workspace instead of
    /// a session-scoped volume. When set, the platform acquires an advisory
    /// lock on the named volume before VM launch (409 if already in use).
    pub shared_mount: Option<String>,
}

impl VolumesSection {
    /// Returns `true` if a volume has been explicitly declared with a size.
    ///
    /// A `[volumes]` section with no `size` field is treated the same as
    /// an absent section (both produce `VolumesSection { size: None }`
    /// via `serde(default)`). Phase 17 will use the `manifest_volumes_json`
    /// DB column to distinguish the two cases.
    pub fn is_declared(&self) -> bool {
        self.size.is_some()
    }
}
