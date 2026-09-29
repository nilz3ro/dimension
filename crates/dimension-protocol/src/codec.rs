//! Varint length-delimited async codec for dimension wire protocol messages.
//!
//! [`ProtocolCodec`] implements [`tokio_util::codec::Decoder`] and
//! [`tokio_util::codec::Encoder`] for [`Envelope`] messages, using prost's
//! varint length-delimited framing. It is designed for use with
//! [`tokio_util::codec::Framed`] over any async byte stream (vsock, TCP, etc.).

use bytes::BytesMut;
use prost::Message;
use tokio_util::codec::{Decoder, Encoder};

use crate::error::CodecError;
use crate::proto::Envelope;

/// Default maximum message size: 4 MiB.
pub const DEFAULT_MAX_MESSAGE_SIZE: usize = 4 * 1024 * 1024;

/// A varint length-delimited codec for [`Envelope`] messages.
///
/// Each frame on the wire consists of a prost varint encoding the message
/// length, followed by that many bytes of protobuf-encoded [`Envelope`].
///
/// The codec enforces a configurable maximum message size on both encode
/// and decode paths to prevent unbounded memory allocation.
#[derive(Debug, Clone)]
pub struct ProtocolCodec {
    max_message_size: usize,
}

impl ProtocolCodec {
    /// Create a new codec with the given maximum message size in bytes.
    pub fn new(max_message_size: usize) -> Self {
        Self { max_message_size }
    }
}

impl Default for ProtocolCodec {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_MESSAGE_SIZE)
    }
}

impl Decoder for ProtocolCodec {
    type Item = Envelope;
    type Error = CodecError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if src.is_empty() {
            return Ok(None);
        }

        // Peek at the varint length from a slice reference to avoid consuming
        // bytes prematurely. prost::decode_length_delimiter advances the cursor
        // on its input, so we must pass a slice, not the BytesMut itself.
        let msg_len = match prost::decode_length_delimiter(&src[..]) {
            Ok(len) => len,
            Err(err) => {
                // Varint could be incomplete. A varint is at most 10 bytes,
                // so if we have 10+ bytes and still fail, it's a real error.
                if src.len() >= 10 {
                    return Err(CodecError::Decode(err));
                }
                // Incomplete varint -- wait for more data.
                return Ok(None);
            }
        };

        // Check message size limit before allocating.
        if msg_len > self.max_message_size {
            return Err(CodecError::MessageTooLarge {
                size: msg_len,
                max: self.max_message_size,
            });
        }

        // Calculate the total frame size: varint prefix + message body.
        let varint_len = prost::length_delimiter_len(msg_len);
        let total = varint_len + msg_len;

        if src.len() < total {
            // Not enough data yet. Reserve capacity so the next read can
            // fill the buffer without reallocation.
            src.reserve(total - src.len());
            return Ok(None);
        }

        // We have a complete frame. Split it off and decode.
        let frame = src.split_to(total);
        let envelope = Envelope::decode_length_delimited(frame)?;
        Ok(Some(envelope))
    }
}

impl Encoder<Envelope> for ProtocolCodec {
    type Error = CodecError;

    fn encode(&mut self, item: Envelope, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let len = item.encoded_len();

        if len > self.max_message_size {
            return Err(CodecError::MessageTooLarge {
                size: len,
                max: self.max_message_size,
            });
        }

        dst.reserve(prost::length_delimiter_len(len) + len);
        item.encode_length_delimited(dst)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::{Bytes, BytesMut};
    use std::collections::HashMap;

    use crate::proto::{envelope, DataChunk, Done, Envelope, Error, ErrorCode, OutboundMessage, Request};

    /// Helper: create an Envelope with a Request payload.
    fn request_envelope(request_id: &str, payload: &[u8]) -> Envelope {
        Envelope {
            request_id: request_id.to_string(),
            payload: Some(envelope::Payload::Request(Request {
                payload: Bytes::copy_from_slice(payload),
            })),
        }
    }

    /// Helper: create an Envelope with a DataChunk payload.
    fn data_chunk_envelope(
        request_id: &str,
        content: &[u8],
        sequence: u32,
        content_type: Option<&str>,
    ) -> Envelope {
        Envelope {
            request_id: request_id.to_string(),
            payload: Some(envelope::Payload::DataChunk(DataChunk {
                content: Bytes::copy_from_slice(content),
                sequence,
                content_type: content_type.map(String::from),
            })),
        }
    }

    /// Helper: create an Envelope with an Error payload.
    fn error_envelope(
        request_id: &str,
        code: ErrorCode,
        message: &str,
        detail: &str,
        terminal: bool,
    ) -> Envelope {
        Envelope {
            request_id: request_id.to_string(),
            payload: Some(envelope::Payload::Error(Error {
                code: code as i32,
                message: message.to_string(),
                detail: detail.to_string(),
                terminal,
            })),
        }
    }

    /// Helper: create an Envelope with a Done payload.
    fn done_envelope(
        request_id: &str,
        metadata: HashMap<String, String>,
        exit_code: i32,
        success: bool,
    ) -> Envelope {
        Envelope {
            request_id: request_id.to_string(),
            payload: Some(envelope::Payload::Done(Done {
                metadata,
                exit_code,
                success,
            })),
        }
    }

    /// Helper: encode an envelope into a BytesMut buffer.
    fn encode_to_buf(codec: &mut ProtocolCodec, envelope: &Envelope) -> BytesMut {
        let mut buf = BytesMut::new();
        codec.encode(envelope.clone(), &mut buf).unwrap();
        buf
    }

    // -----------------------------------------------------------------------
    // Roundtrip tests
    // -----------------------------------------------------------------------

    #[test]
    fn roundtrip_request() {
        let mut codec = ProtocolCodec::default();
        let original = request_envelope("req-001", b"hello world");
        let mut buf = encode_to_buf(&mut codec, &original);
        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(original, decoded);
        assert!(buf.is_empty(), "buffer should be fully consumed");
    }

    #[test]
    fn roundtrip_data_chunk() {
        let mut codec = ProtocolCodec::default();
        let original = data_chunk_envelope("req-002", b"chunk data here", 42, Some("text"));
        let mut buf = encode_to_buf(&mut codec, &original);
        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(original, decoded);

        // Verify individual fields to be extra thorough.
        if let Some(envelope::Payload::DataChunk(chunk)) = decoded.payload {
            assert_eq!(chunk.content, Bytes::from_static(b"chunk data here"));
            assert_eq!(chunk.sequence, 42);
            assert_eq!(chunk.content_type, Some("text".to_string()));
        } else {
            panic!("expected DataChunk payload");
        }
    }

    #[test]
    fn roundtrip_data_chunk_no_content_type() {
        let mut codec = ProtocolCodec::default();
        let original = data_chunk_envelope("req-003", b"no type", 1, None);
        let mut buf = encode_to_buf(&mut codec, &original);
        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(original, decoded);

        if let Some(envelope::Payload::DataChunk(chunk)) = decoded.payload {
            assert_eq!(chunk.content_type, None);
        } else {
            panic!("expected DataChunk payload");
        }
    }

    #[test]
    fn roundtrip_error() {
        let mut codec = ProtocolCodec::default();
        let original = error_envelope(
            "req-004",
            ErrorCode::ApplicationError,
            "something failed",
            "stack trace here",
            true,
        );
        let mut buf = encode_to_buf(&mut codec, &original);
        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(original, decoded);

        if let Some(envelope::Payload::Error(err)) = decoded.payload {
            assert_eq!(err.code, ErrorCode::ApplicationError as i32);
            assert_eq!(err.message, "something failed");
            assert_eq!(err.detail, "stack trace here");
            assert!(err.terminal);
        } else {
            panic!("expected Error payload");
        }
    }

    #[test]
    fn roundtrip_error_nonterminal() {
        let mut codec = ProtocolCodec::default();
        let original = error_envelope(
            "req-005",
            ErrorCode::Timeout,
            "timed out",
            "",
            false,
        );
        let mut buf = encode_to_buf(&mut codec, &original);
        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(original, decoded);

        if let Some(envelope::Payload::Error(err)) = decoded.payload {
            assert!(!err.terminal);
        } else {
            panic!("expected Error payload");
        }
    }

    #[test]
    fn roundtrip_done() {
        let mut codec = ProtocolCodec::default();
        let mut metadata = HashMap::new();
        metadata.insert("tokens".to_string(), "1234".to_string());
        metadata.insert("model".to_string(), "gpt-4".to_string());
        let original = done_envelope("req-006", metadata.clone(), 3, false);
        let mut buf = encode_to_buf(&mut codec, &original);
        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(original, decoded);

        if let Some(envelope::Payload::Done(done)) = decoded.payload {
            assert_eq!(done.metadata, metadata);
            assert_eq!(done.exit_code, 3);
            assert!(!done.success);
        } else {
            panic!("expected Done payload");
        }
    }

    #[test]
    fn roundtrip_done_empty_metadata() {
        let mut codec = ProtocolCodec::default();
        let original = done_envelope("req-007", HashMap::new(), 0, true);
        let mut buf = encode_to_buf(&mut codec, &original);
        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(original, decoded);

        if let Some(envelope::Payload::Done(done)) = decoded.payload {
            assert!(done.metadata.is_empty());
        } else {
            panic!("expected Done payload");
        }
    }

    #[test]
    fn roundtrip_outbound_message() {
        let mut codec = ProtocolCodec::default();
        let mut attrs = HashMap::new();
        attrs.insert("tool".to_string(), "grep".to_string());
        let original = Envelope {
            request_id: "req-outbound".to_string(),
            payload: Some(envelope::Payload::Outbound(OutboundMessage {
                sequence: 7,
                timestamp_ms: 1_700_000_000_000,
                kind: "bundle".to_string(),
                content_type: "application/json".to_string(),
                body: Bytes::from_static(br#"{"hello":"world"}"#),
                attributes: attrs.clone(),
            })),
        };
        let mut buf = encode_to_buf(&mut codec, &original);
        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(original, decoded);

        if let Some(envelope::Payload::Outbound(msg)) = decoded.payload {
            assert_eq!(msg.sequence, 7);
            assert_eq!(msg.kind, "bundle");
            assert_eq!(msg.content_type, "application/json");
            assert_eq!(msg.body, Bytes::from_static(br#"{"hello":"world"}"#));
            assert_eq!(msg.attributes, attrs);
        } else {
            panic!("expected Outbound payload");
        }
    }

    #[test]
    fn roundtrip_empty_payload() {
        let mut codec = ProtocolCodec::default();
        let original = Envelope {
            request_id: "req-008".to_string(),
            payload: None,
        };
        let mut buf = encode_to_buf(&mut codec, &original);
        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(original, decoded);
        assert!(decoded.payload.is_none());
    }

    // -----------------------------------------------------------------------
    // Partial read tests
    // -----------------------------------------------------------------------

    #[test]
    fn partial_read_incomplete_varint() {
        let mut codec = ProtocolCodec::default();
        let original = request_envelope("req-009", b"partial varint test");
        let full_buf = encode_to_buf(&mut codec, &original);

        // Feed only the first byte (which is the start of the varint).
        let mut buf = BytesMut::from(&full_buf[..1]);
        let result = codec.decode(&mut buf).unwrap();
        assert!(result.is_none(), "should return None on incomplete varint");

        // Now feed the rest.
        buf.extend_from_slice(&full_buf[1..]);
        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn partial_read_incomplete_body() {
        let mut codec = ProtocolCodec::default();
        let original = request_envelope("req-010", b"partial body test with more data");
        let full_buf = encode_to_buf(&mut codec, &original);

        // Feed varint + half the body.
        let varint_len = prost::length_delimiter_len(original.encoded_len());
        let partial_end = varint_len + original.encoded_len() / 2;
        let mut buf = BytesMut::from(&full_buf[..partial_end]);
        let result = codec.decode(&mut buf).unwrap();
        assert!(result.is_none(), "should return None on incomplete body");
        // Buffer should have reserved capacity for the remaining bytes.
        assert!(
            buf.capacity() >= full_buf.len() - buf.len(),
            "should have reserved capacity"
        );

        // Feed the rest.
        buf.extend_from_slice(&full_buf[partial_end..]);
        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn partial_read_empty_buffer() {
        let mut codec = ProtocolCodec::default();
        let mut buf = BytesMut::new();
        let result = codec.decode(&mut buf).unwrap();
        assert!(result.is_none(), "empty buffer should return None");
    }

    // -----------------------------------------------------------------------
    // Edge case tests
    // -----------------------------------------------------------------------

    #[test]
    fn max_message_size_exceeded_decode() {
        // Use a small max to make the test fast.
        let mut codec = ProtocolCodec::new(32);

        // Craft a varint prefix indicating a message of 1000 bytes (> 32).
        let mut buf = BytesMut::new();
        prost::encode_length_delimiter(1000, &mut buf).unwrap();
        // Add some dummy bytes (don't need the full 1000 for the size check).
        buf.extend_from_slice(&[0u8; 64]);

        match codec.decode(&mut buf) {
            Err(CodecError::MessageTooLarge { size, max }) => {
                assert_eq!(size, 1000);
                assert_eq!(max, 32);
            }
            other => panic!("expected MessageTooLarge, got {:?}", other),
        }
    }

    #[test]
    fn max_message_size_exceeded_encode() {
        let mut codec = ProtocolCodec::new(16);

        // Create a request with a payload larger than 16 bytes.
        let big_payload = vec![0xABu8; 100];
        let envelope = request_envelope("req-big", &big_payload);
        let mut buf = BytesMut::new();

        match codec.encode(envelope, &mut buf) {
            Err(CodecError::MessageTooLarge { size, max }) => {
                assert!(size > 16, "encoded size should exceed max");
                assert_eq!(max, 16);
            }
            other => panic!("expected MessageTooLarge, got {:?}", other),
        }
    }

    #[test]
    fn multiple_messages_in_buffer() {
        let mut codec = ProtocolCodec::default();
        let msg1 = request_envelope("req-multi-1", b"first message");
        let msg2 = data_chunk_envelope("req-multi-2", b"second message", 1, Some("json"));

        // Encode both into the same buffer.
        let mut buf = BytesMut::new();
        codec.encode(msg1.clone(), &mut buf).unwrap();
        codec.encode(msg2.clone(), &mut buf).unwrap();

        // Decode should return them in order.
        let decoded1 = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(msg1, decoded1);

        let decoded2 = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(msg2, decoded2);

        // Buffer should be empty after both messages consumed.
        assert!(buf.is_empty());
    }

    #[test]
    fn zero_length_message() {
        let mut codec = ProtocolCodec::default();

        // A zero-length frame: varint(0) followed by no body bytes.
        // This is a valid protobuf encoding of an Envelope with all default values.
        let mut buf = BytesMut::new();
        prost::encode_length_delimiter(0, &mut buf).unwrap();

        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        // All-defaults Envelope: empty request_id, no payload.
        assert_eq!(decoded.request_id, "");
        assert!(decoded.payload.is_none());
    }

    #[test]
    fn custom_max_size() {
        let mut codec = ProtocolCodec::new(64);

        // Small message should work.
        let small = Envelope {
            request_id: "s".to_string(),
            payload: None,
        };
        let mut buf = BytesMut::new();
        codec.encode(small.clone(), &mut buf).unwrap();
        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(small, decoded);

        // Large message should be rejected on encode.
        let big_payload = vec![0xFFu8; 100];
        let large = request_envelope("req-large", &big_payload);
        let mut buf = BytesMut::new();
        match codec.encode(large, &mut buf) {
            Err(CodecError::MessageTooLarge { max, .. }) => {
                assert_eq!(max, 64);
            }
            other => panic!("expected MessageTooLarge, got {:?}", other),
        }
    }

    // -----------------------------------------------------------------------
    // Error code tests
    // -----------------------------------------------------------------------

    #[test]
    fn error_code_values() {
        // Verify the integer values match the proto definition.
        assert_eq!(ErrorCode::Unspecified as i32, 0);
        assert_eq!(ErrorCode::ApplicationError as i32, 1);
        assert_eq!(ErrorCode::InvalidRequest as i32, 2);
        assert_eq!(ErrorCode::Timeout as i32, 3);
        assert_eq!(ErrorCode::Internal as i32, 10);
        assert_eq!(ErrorCode::CodecError as i32, 11);
        assert_eq!(ErrorCode::MessageTooLarge as i32, 12);
        assert_eq!(ErrorCode::Unavailable as i32, 13);
    }

    // -----------------------------------------------------------------------
    // Codec state integrity tests
    // -----------------------------------------------------------------------

    #[test]
    fn decode_does_not_corrupt_buffer_on_partial_read() {
        // Verify that after a partial read returns Ok(None), the buffer
        // content is unchanged (varint bytes not consumed prematurely).
        let mut codec = ProtocolCodec::default();
        let original = request_envelope("req-integrity", b"integrity check payload");
        let full_buf = encode_to_buf(&mut codec, &original);

        // Feed partial data.
        let partial = &full_buf[..full_buf.len() / 2];
        let mut buf = BytesMut::from(partial);
        let original_bytes = buf.to_vec();

        let result = codec.decode(&mut buf).unwrap();
        assert!(result.is_none());

        // Buffer content should be unchanged (bytes not consumed).
        assert_eq!(buf.to_vec(), original_bytes, "buffer must not be modified on partial read");
    }

    #[test]
    fn sequential_encode_decode_many() {
        // Stress: encode and decode 100 messages sequentially through the same codec.
        let mut codec = ProtocolCodec::default();
        let mut buf = BytesMut::new();

        for i in 0..100 {
            let msg = request_envelope(
                &format!("req-{i}"),
                format!("payload-{i}").as_bytes(),
            );
            codec.encode(msg, &mut buf).unwrap();
        }

        for i in 0..100 {
            let decoded = codec.decode(&mut buf).unwrap().unwrap();
            assert_eq!(decoded.request_id, format!("req-{i}"));
            if let Some(envelope::Payload::Request(req)) = decoded.payload {
                assert_eq!(req.payload, Bytes::from(format!("payload-{i}")));
            } else {
                panic!("expected Request payload for message {i}");
            }
        }

        assert!(buf.is_empty());
    }

    #[test]
    fn default_max_message_size_constant() {
        assert_eq!(DEFAULT_MAX_MESSAGE_SIZE, 4 * 1024 * 1024);
        let codec = ProtocolCodec::default();
        assert_eq!(codec.max_message_size, DEFAULT_MAX_MESSAGE_SIZE);
    }
}
