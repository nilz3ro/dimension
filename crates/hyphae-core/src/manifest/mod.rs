//! Bundle manifest parsing for `dimension.toml`.
//!
//! The manifest is an optional declarative file placed at the root of a bundle
//! archive. It is parsed during the upload pipeline (after tar extraction,
//! before the build step) and stored as JSON columns in the registry.
//!
//! # Usage
//!
//! ```rust
//! use hyphae_core::manifest::parse_manifest;
//!
//! let toml = r#"
//! [resources]
//! memory_mb = 512
//! vcpus = 2
//! "#;
//!
//! let manifest = parse_manifest(toml).unwrap();
//! assert_eq!(manifest.resources.memory_mb, Some(512));
//! ```

pub mod types;
pub use types::*;

/// Parse a `dimension.toml` file content into a [`DimensionManifest`].
///
/// Returns a `DimensionManifest` with safe defaults if `content` is empty.
/// Returns a `toml::de::Error` with line/column information on parse failure,
/// including unknown section names.
pub fn parse_manifest(content: &str) -> Result<DimensionManifest, toml::de::Error> {
    // Empty string short-circuit: return defaults without invoking the parser.
    // This avoids any serde-related issues with truly empty TOML.
    if content.trim().is_empty() {
        return Ok(DimensionManifest::default());
    }
    toml::from_str(content)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test: valid dimension.toml with all sections parses to DimensionManifest
    /// with correct values.
    #[test]
    fn test_parse_full_manifest() {
        let toml = r#"
[resources]
memory_mb = 512
vcpus = 4
timeout_secs = 30

[env]
DATABASE_URL = "postgres://localhost/mydb"
LOG_LEVEL = "debug"

[secrets]
names = ["DB_PASSWORD", "API_KEY"]

[capabilities]
secrets = true
storage = false
agent_calls = true

[a2a]
name = "my-agent"
description = "A test agent"
skills = ["summarize", "translate"]
"#;

        let manifest = parse_manifest(toml).unwrap();

        assert_eq!(manifest.resources.memory_mb, Some(512));
        assert_eq!(manifest.resources.vcpus, Some(4));
        assert_eq!(manifest.resources.timeout_secs, Some(30));

        assert_eq!(
            manifest.env.vars.get("DATABASE_URL").map(String::as_str),
            Some("postgres://localhost/mydb")
        );
        assert_eq!(
            manifest.env.vars.get("LOG_LEVEL").map(String::as_str),
            Some("debug")
        );

        assert_eq!(manifest.secrets.names, vec!["DB_PASSWORD", "API_KEY"]);

        assert!(manifest.capabilities.secrets);
        assert!(!manifest.capabilities.storage);
        assert!(manifest.capabilities.agent_calls);

        assert_eq!(manifest.a2a.name.as_deref(), Some("my-agent"));
        assert_eq!(manifest.a2a.description.as_deref(), Some("A test agent"));
        assert_eq!(manifest.a2a.skills, vec!["summarize", "translate"]);
    }

    /// Test: dimension.toml with only [resources] parses successfully
    /// (other sections default to safe values).
    #[test]
    fn test_parse_resources_only() {
        let toml = r#"
[resources]
memory_mb = 1024
"#;

        let manifest = parse_manifest(toml).unwrap();

        assert_eq!(manifest.resources.memory_mb, Some(1024));
        assert_eq!(manifest.resources.vcpus, None);
        assert_eq!(manifest.resources.timeout_secs, None);

        // Other sections default
        assert!(manifest.env.vars.is_empty());
        assert!(manifest.secrets.names.is_empty());
        assert!(!manifest.capabilities.secrets);
        assert!(!manifest.capabilities.storage);
        assert!(!manifest.capabilities.agent_calls);
        assert!(manifest.a2a.name.is_none());
    }

    /// Test: empty string parses to DimensionManifest::default()
    /// (all sections default; this is the "no manifest" case).
    #[test]
    fn test_parse_empty_string() {
        let manifest = parse_manifest("").unwrap();

        assert_eq!(manifest.resources.memory_mb, None);
        assert_eq!(manifest.resources.vcpus, None);
        assert_eq!(manifest.resources.timeout_secs, None);
        assert!(manifest.env.vars.is_empty());
        assert!(manifest.secrets.names.is_empty());
        assert!(!manifest.capabilities.secrets);
        assert!(!manifest.capabilities.storage);
        assert!(!manifest.capabilities.agent_calls);
        assert!(manifest.a2a.name.is_none());
        assert!(manifest.a2a.skills.is_empty());
    }

    /// Test: unknown top-level field (e.g. [unknown_section]) is rejected
    /// with an error (deny_unknown_fields on DimensionManifest).
    #[test]
    fn test_unknown_section_rejected() {
        let toml = r#"
[resources]
memory_mb = 256

[unknown_section]
foo = "bar"
"#;

        let result = parse_manifest(toml);
        assert!(
            result.is_err(),
            "expected error for unknown section, got: {:?}",
            result
        );
        let err = result.unwrap_err();
        let err_str = err.to_string();
        // Error should mention the unknown field
        assert!(
            err_str.contains("unknown_section") || err_str.contains("unknown field"),
            "error should mention the unknown field: {err_str}"
        );
    }

    /// Test: invalid TOML syntax (missing closing bracket) is rejected with
    /// line/column info.
    #[test]
    fn test_invalid_toml_syntax_rejected() {
        let toml = r#"
[resources
memory_mb = 256
"#;

        let result = parse_manifest(toml);
        assert!(
            result.is_err(),
            "expected error for invalid TOML, got: {:?}",
            result
        );
        let err = result.unwrap_err();
        let err_str = err.to_string();
        // toml::de::Error includes line/column info
        assert!(
            err_str.contains("line") || err_str.contains("column") || err_str.contains("at line"),
            "error should include line/column info: {err_str}"
        );
    }

    /// Test: [env] section with KEY = "VALUE" pairs deserializes to HashMap
    /// correctly.
    #[test]
    fn test_env_section_deserialization() {
        let toml = r#"
[env]
REDIS_URL = "redis://localhost:6379"
NODE_ENV = "production"
PORT = "8080"
"#;

        let manifest = parse_manifest(toml).unwrap();
        let env = &manifest.env.vars;

        assert_eq!(env.len(), 3);
        assert_eq!(env.get("REDIS_URL").map(String::as_str), Some("redis://localhost:6379"));
        assert_eq!(env.get("NODE_ENV").map(String::as_str), Some("production"));
        assert_eq!(env.get("PORT").map(String::as_str), Some("8080"));
    }

    /// Test: [secrets] with names = ["A", "B"] deserializes correctly.
    #[test]
    fn test_secrets_section_deserialization() {
        let toml = r#"
[secrets]
names = ["SECRET_A", "SECRET_B", "SECRET_C"]
"#;

        let manifest = parse_manifest(toml).unwrap();
        assert_eq!(
            manifest.secrets.names,
            vec!["SECRET_A", "SECRET_B", "SECRET_C"]
        );
    }

    /// Test: [capabilities] defaults all to false (default deny).
    #[test]
    fn test_capabilities_default_deny() {
        // Empty capabilities section: all fields should be false
        let toml = r#"
[capabilities]
"#;
        let manifest = parse_manifest(toml).unwrap();
        assert!(!manifest.capabilities.secrets, "secrets should default to false");
        assert!(!manifest.capabilities.storage, "storage should default to false");
        assert!(!manifest.capabilities.agent_calls, "agent_calls should default to false");

        // Missing capabilities section entirely: same defaults
        let manifest2 = parse_manifest("").unwrap();
        assert!(!manifest2.capabilities.secrets);
        assert!(!manifest2.capabilities.storage);
        assert!(!manifest2.capabilities.agent_calls);
    }

    /// Test: [a2a] with name, description, skills deserializes correctly.
    #[test]
    fn test_a2a_section_deserialization() {
        let toml = r#"
[a2a]
name = "code-reviewer"
description = "Reviews pull requests and suggests improvements"
skills = ["review", "suggest", "explain"]
"#;

        let manifest = parse_manifest(toml).unwrap();
        assert_eq!(manifest.a2a.name.as_deref(), Some("code-reviewer"));
        assert_eq!(
            manifest.a2a.description.as_deref(),
            Some("Reviews pull requests and suggests improvements")
        );
        assert_eq!(manifest.a2a.skills, vec!["review", "suggest", "explain"]);
    }

    /// Test: [resources] with partial fields (only memory_mb) keeps others as None.
    #[test]
    fn test_resources_partial_fields() {
        let toml = r#"
[resources]
memory_mb = 768
"#;

        let manifest = parse_manifest(toml).unwrap();
        assert_eq!(manifest.resources.memory_mb, Some(768));
        assert_eq!(manifest.resources.vcpus, None, "vcpus should be None when not specified");
        assert_eq!(
            manifest.resources.timeout_secs,
            None,
            "timeout_secs should be None when not specified"
        );
    }
}
