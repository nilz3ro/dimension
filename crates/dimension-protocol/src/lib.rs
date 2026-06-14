//! Dimension wire protocol types and codec.
//!
//! This crate defines the protobuf message types and async codec
//! for host-guest communication over vsock.

/// Well-known vsock port for host-guest communication.
///
/// Both the guest agent (listener) and host connector use this port.
/// Port 1024 is the first non-privileged vsock port (ports 0-1023
/// require CAP_NET_BIND_SERVICE on Linux).
pub const VSOCK_PORT: u32 = 1024;

/// Generated protobuf types for the dimension wire protocol.
pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/dimension.protocol.rs"));
}

pub mod codec;
pub mod error;

// Re-export key types at crate root for ergonomic imports
pub use codec::ProtocolCodec;
pub use error::CodecError;
pub use proto::{envelope, DataChunk, Done, Envelope, Error, ErrorCode, OutboundMessage, Request, ShutdownAck, ShutdownRequest};

// Re-export platform service protocol types
pub use proto::{
    // Wrappers
    ServiceRequest, ServiceResponse, ServiceError, Capabilities,
    // Secrets
    SecretGetRequest, SecretGetResponse,
    SecretCreateRequest, SecretCreateResponse,
    SecretResolveRequest, SecretResolveResponse,
    // Storage
    StoragePutRequest, StoragePutResponse,
    StorageGetRequest, StorageGetResponse,
    StorageDeleteRequest, StorageDeleteResponse,
    StorageListRequest, StorageListResponse, StorageObject,
    // Agents
    AgentSendRequest, AgentSendResponse,
    AgentListRequest, AgentListResponse, AgentInfo,
    // Session
    SessionAppendEventRequest, SessionAppendEventResponse,
    // Context
    ContextGetRequest, ContextGetResponse, ContextEvent,
    ContextUpdateRequest, ContextUpdateResponse,
    ContextSetMetadata, ContextSetMetadataAll,
    ContextDeleteMessage, ContextTrim, ContextSetMessages,
    // Capabilities
    CapabilitiesGetRequest, CapabilitiesGetResponse,
    // Artifacts
    ArtifactPublishRequest, ArtifactPublishResponse,
    ArtifactListRequest, ArtifactListResponse, ArtifactInfo,
    // Oneof discriminants
    service_request, service_response, context_update_request,
};
