//! Request types for the HTTP API.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::error::{AppError, ValidationProblem};

/// VM resource overrides in the request payload.
///
/// All fields are optional -- omitted fields use hyphae bundle defaults.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct VmResources {
    /// Number of virtual CPUs (must be >= 1 if provided).
    pub vcpus: Option<u32>,
    /// Memory in MiB (must be >= 1 if provided).
    pub memory_mib: Option<u32>,
    /// Root disk size in MiB.
    pub disk_size_mib: Option<u32>,
}

/// Operator-configured maximum values for per-request VM resources.
///
/// Requests exceeding any cap are rejected with 400 Bad Request.
#[derive(Debug, Clone)]
pub struct ResourceCaps {
    /// Maximum allowed vCPUs per request.
    pub max_vcpus: u32,
    /// Maximum allowed memory in MiB per request.
    pub max_memory_mib: u32,
    /// Maximum allowed disk size in MiB per request.
    pub max_disk_size_mib: u32,
}

/// Validate per-request VM resources against operator-configured caps.
///
/// Returns 400 Bad Request (not 422 Validation) for cap violations.
/// Per CONTEXT.md: "requests exceeding caps are rejected with 400 Bad Request
/// identifying which field exceeded which limit."
///
/// Collects ALL violations before returning (not fail-on-first).
pub fn validate_resources(resources: &VmResources, caps: &ResourceCaps) -> Result<(), AppError> {
    let mut problems = Vec::new();

    if let Some(vcpus) = resources.vcpus {
        if vcpus == 0 {
            problems.push("resources.vcpus must be at least 1".to_string());
        } else if vcpus > caps.max_vcpus {
            problems.push(format!(
                "resources.vcpus ({}) exceeds maximum allowed value ({})",
                vcpus, caps.max_vcpus
            ));
        }
    }

    if let Some(memory_mib) = resources.memory_mib {
        if memory_mib == 0 {
            problems.push("resources.memory_mib must be at least 1".to_string());
        } else if memory_mib > caps.max_memory_mib {
            problems.push(format!(
                "resources.memory_mib ({}) exceeds maximum allowed value ({})",
                memory_mib, caps.max_memory_mib
            ));
        }
    }

    if let Some(disk_size_mib) = resources.disk_size_mib
        && disk_size_mib > caps.max_disk_size_mib
    {
        problems.push(format!(
            "resources.disk_size_mib ({}) exceeds maximum allowed value ({})",
            disk_size_mib, caps.max_disk_size_mib
        ));
    }

    if problems.is_empty() {
        Ok(())
    } else {
        Err(AppError::BadRequest(problems.join("; ")))
    }
}

/// A message request submitted to POST /messages.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MessageRequest {
    /// The role of the message sender (e.g. "user").
    pub role: String,

    /// Content blocks that make up the message.
    pub content: Vec<ContentBlock>,

    /// Optional session ID for multi-turn exchanges.
    /// When provided, continues an existing session with history.
    /// When absent, a new session is created.
    #[serde(default)]
    pub session_id: Option<String>,

    /// Hyphae bundle identifier specifying which VM image to use.
    pub bundle_id: String,

    /// Optional boot timeout override in seconds (clamped to server max).
    #[serde(default)]
    pub boot_timeout_secs: Option<u64>,

    /// Optional processing timeout override in seconds (clamped to server max).
    #[serde(default)]
    pub processing_timeout_secs: Option<u64>,

    /// Optional VM resource overrides. If omitted, bundle defaults apply.
    #[serde(default)]
    pub resources: Option<VmResources>,

    /// Authenticated user identity -- set by messages_handler, never serialized to agents.
    #[serde(default)]
    pub user_id: Option<Uuid>,

    /// Pre-assigned session ID — set by messages_handler from the request_id.
    /// When Some, SessionAwareHandler uses this UUID instead of generating a
    /// new one so the 202 response's session_id matches the real session.
    #[serde(skip_serializing, skip_deserializing, default)]
    pub assigned_session_id: Option<Uuid>,

    /// Call depth from X-Dimension-Call-Depth header. Set by messages_handler.
    /// When >= 1, the nested VM's JWT has caps.agents = false (one-hop limit).
    #[serde(skip_serializing, skip_deserializing, default)]
    pub call_depth: u32,

    /// Extra fields for enriched context (session, history, truncation).
    /// Populated by SessionAwareHandler; flattened to top-level JSON keys on serialization.
    /// Empty by default when deserializing client requests.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// A typed content block within a message.
///
/// Serialized with a `type` tag to distinguish variants.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    /// Plain text content.
    Text {
        /// The text content.
        text: String,
    },
    /// An image referenced by URL.
    Image {
        /// URL pointing to the image resource.
        url: String,
        /// Optional media type (e.g. "image/png").
        #[serde(default)]
        media_type: Option<String>,
    },
    /// A file referenced by URL.
    File {
        /// URL pointing to the file resource.
        url: String,
        /// Original filename.
        #[serde(default)]
        filename: Option<String>,
    },
}

impl MessageRequest {
    /// Validate the request, collecting all problems at once.
    ///
    /// Returns `Ok(())` if valid, or `Err` with a list of every
    /// validation problem found (not fail-on-first).
    pub fn validate(&self) -> Result<(), Vec<ValidationProblem>> {
        let mut problems = Vec::new();

        if self.role.is_empty() {
            problems.push(ValidationProblem {
                field: "role".into(),
                message: "role must not be empty".into(),
            });
        }

        if self.bundle_id.trim().is_empty() {
            problems.push(ValidationProblem {
                field: "bundle_id".into(),
                message: "bundle_id must not be empty".into(),
            });
        }

        if self.content.is_empty() {
            problems.push(ValidationProblem {
                field: "content".into(),
                message: "content must contain at least one block".into(),
            });
        }

        for (i, block) in self.content.iter().enumerate() {
            match block {
                ContentBlock::Text { text } if text.is_empty() => {
                    problems.push(ValidationProblem {
                        field: format!("content[{i}].text"),
                        message: "text block must not be empty".into(),
                    });
                }
                ContentBlock::Image { url, .. } if url.is_empty() => {
                    problems.push(ValidationProblem {
                        field: format!("content[{i}].url"),
                        message: "image block must have a url".into(),
                    });
                }
                ContentBlock::File { url, .. } if url.is_empty() => {
                    problems.push(ValidationProblem {
                        field: format!("content[{i}].url"),
                        message: "file block must have a url".into(),
                    });
                }
                _ => {}
            }
        }

        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_text_request() {
        let json = r#"{"role":"user","content":[{"type":"text","text":"hello"}],"bundle_id":"test-bundle"}"#;
        let req: MessageRequest = serde_json::from_str(json).unwrap();
        assert!(req.validate().is_ok());
    }

    #[test]
    fn valid_request_with_session_id() {
        let json =
            r#"{"role":"user","content":[{"type":"text","text":"hi"}],"bundle_id":"test-bundle","session_id":"abc"}"#;
        let req: MessageRequest = serde_json::from_str(json).unwrap();
        assert!(req.validate().is_ok());
        assert_eq!(req.session_id.as_deref(), Some("abc"));
    }

    #[test]
    fn valid_image_block() {
        let json = r#"{"role":"user","content":[{"type":"image","url":"https://example.com/img.png","media_type":"image/png"}],"bundle_id":"img-bundle"}"#;
        let req: MessageRequest = serde_json::from_str(json).unwrap();
        assert!(req.validate().is_ok());
    }

    #[test]
    fn valid_file_block() {
        let json = r#"{"role":"user","content":[{"type":"file","url":"https://example.com/doc.pdf","filename":"doc.pdf"}],"bundle_id":"file-bundle"}"#;
        let req: MessageRequest = serde_json::from_str(json).unwrap();
        assert!(req.validate().is_ok());
    }

    #[test]
    fn collects_all_validation_problems() {
        let json = r#"{"role":"","content":[{"type":"text","text":""},{"type":"image","url":""}],"bundle_id":""}"#;
        let req: MessageRequest = serde_json::from_str(json).unwrap();
        let problems = req.validate().unwrap_err();
        // Should collect: empty role, empty bundle_id, empty text, empty image url = 4 problems
        assert_eq!(problems.len(), 4);
        assert!(problems.iter().any(|p| p.field == "role"));
        assert!(problems.iter().any(|p| p.field == "bundle_id"));
        assert!(problems.iter().any(|p| p.field == "content[0].text"));
        assert!(problems.iter().any(|p| p.field == "content[1].url"));
    }

    #[test]
    fn empty_content_array_is_invalid() {
        let json = r#"{"role":"user","content":[],"bundle_id":"test"}"#;
        let req: MessageRequest = serde_json::from_str(json).unwrap();
        let problems = req.validate().unwrap_err();
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].field, "content");
    }

    #[test]
    fn session_id_defaults_to_none() {
        let json = r#"{"role":"user","content":[{"type":"text","text":"hi"}],"bundle_id":"test"}"#;
        let req: MessageRequest = serde_json::from_str(json).unwrap();
        assert!(req.session_id.is_none());
    }

    #[test]
    fn empty_bundle_id_is_invalid() {
        let json = r#"{"role":"user","content":[{"type":"text","text":"hi"}],"bundle_id":""}"#;
        let req: MessageRequest = serde_json::from_str(json).unwrap();
        let problems = req.validate().unwrap_err();
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].field, "bundle_id");
    }

    #[test]
    fn whitespace_only_bundle_id_is_invalid() {
        let json = r#"{"role":"user","content":[{"type":"text","text":"hi"}],"bundle_id":"   "}"#;
        let req: MessageRequest = serde_json::from_str(json).unwrap();
        let problems = req.validate().unwrap_err();
        assert!(problems.iter().any(|p| p.field == "bundle_id"));
    }

    #[test]
    fn timeout_overrides_are_optional() {
        let json = r#"{"role":"user","content":[{"type":"text","text":"hi"}],"bundle_id":"test","boot_timeout_secs":10,"processing_timeout_secs":120}"#;
        let req: MessageRequest = serde_json::from_str(json).unwrap();
        assert!(req.validate().is_ok());
        assert_eq!(req.boot_timeout_secs, Some(10));
        assert_eq!(req.processing_timeout_secs, Some(120));
    }

    #[test]
    fn missing_bundle_id_fails_deserialization() {
        let json = r#"{"role":"user","content":[{"type":"text","text":"hi"}]}"#;
        let result: Result<MessageRequest, _> = serde_json::from_str(json);
        assert!(result.is_err(), "bundle_id is required for deserialization");
    }

    // --- VmResources deserialization tests ---

    #[test]
    fn test_vm_resources_deserialize_all_fields() {
        let json = r#"{"vcpus": 4, "memory_mib": 2048, "disk_size_mib": 10240}"#;
        let res: VmResources = serde_json::from_str(json).unwrap();
        assert_eq!(res.vcpus, Some(4));
        assert_eq!(res.memory_mib, Some(2048));
        assert_eq!(res.disk_size_mib, Some(10240));
    }

    #[test]
    fn test_vm_resources_deserialize_partial() {
        let json = r#"{"vcpus": 2}"#;
        let res: VmResources = serde_json::from_str(json).unwrap();
        assert_eq!(res.vcpus, Some(2));
        assert!(res.memory_mib.is_none());
        assert!(res.disk_size_mib.is_none());
    }

    #[test]
    fn test_vm_resources_deserialize_empty_object() {
        let json = r#"{}"#;
        let res: VmResources = serde_json::from_str(json).unwrap();
        assert!(res.vcpus.is_none());
        assert!(res.memory_mib.is_none());
        assert!(res.disk_size_mib.is_none());
    }

    #[test]
    fn test_message_request_without_resources() {
        let json = r#"{"role":"user","content":[{"type":"text","text":"hello"}],"bundle_id":"test-bundle"}"#;
        let req: MessageRequest = serde_json::from_str(json).unwrap();
        assert!(req.resources.is_none());
    }

    #[test]
    fn test_message_request_with_resources() {
        let json = r#"{"role":"user","content":[{"type":"text","text":"hello"}],"bundle_id":"test-bundle","resources":{"vcpus":4,"memory_mib":2048}}"#;
        let req: MessageRequest = serde_json::from_str(json).unwrap();
        let res = req.resources.unwrap();
        assert_eq!(res.vcpus, Some(4));
        assert_eq!(res.memory_mib, Some(2048));
        assert!(res.disk_size_mib.is_none());
    }

    // --- validate_resources tests ---

    fn default_caps() -> ResourceCaps {
        ResourceCaps {
            max_vcpus: 8,
            max_memory_mib: 8192,
            max_disk_size_mib: 65536,
        }
    }

    #[test]
    fn test_validate_resources_within_caps() {
        let res = VmResources {
            vcpus: Some(4),
            memory_mib: Some(4096),
            disk_size_mib: Some(10240),
        };
        assert!(validate_resources(&res, &default_caps()).is_ok());
    }

    #[test]
    fn test_validate_resources_exceeds_vcpus() {
        let res = VmResources {
            vcpus: Some(16),
            memory_mib: None,
            disk_size_mib: None,
        };
        let err = validate_resources(&res, &default_caps()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("bad request"), "expected BadRequest, got: {msg}");
    }

    #[test]
    fn test_validate_resources_exceeds_memory() {
        let res = VmResources {
            vcpus: None,
            memory_mib: Some(16384),
            disk_size_mib: None,
        };
        let err = validate_resources(&res, &default_caps()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("bad request"), "expected BadRequest, got: {msg}");
    }

    #[test]
    fn test_validate_resources_zero_vcpus() {
        let res = VmResources {
            vcpus: Some(0),
            memory_mib: None,
            disk_size_mib: None,
        };
        let err = validate_resources(&res, &default_caps()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("bad request"), "expected BadRequest, got: {msg}");
    }

    #[test]
    fn test_validate_resources_zero_memory() {
        let res = VmResources {
            vcpus: None,
            memory_mib: Some(0),
            disk_size_mib: None,
        };
        let err = validate_resources(&res, &default_caps()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("bad request"), "expected BadRequest, got: {msg}");
    }

    #[test]
    fn test_validate_resources_multiple_violations() {
        let res = VmResources {
            vcpus: Some(0),
            memory_mib: Some(0),
            disk_size_mib: Some(100_000),
        };
        let err = validate_resources(&res, &default_caps()).unwrap_err();
        let msg = format!("{err}");
        // Should contain all three violations joined by "; "
        assert!(msg.contains("vcpus must be at least 1"), "missing vcpus violation: {msg}");
        assert!(msg.contains("memory_mib must be at least 1"), "missing memory violation: {msg}");
        assert!(msg.contains("disk_size_mib"), "missing disk violation: {msg}");
    }

    #[test]
    fn test_validate_resources_all_none_passes() {
        let res = VmResources {
            vcpus: None,
            memory_mib: None,
            disk_size_mib: None,
        };
        assert!(validate_resources(&res, &default_caps()).is_ok());
    }

}
