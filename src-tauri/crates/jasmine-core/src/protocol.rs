use serde::{Deserialize, Serialize};

use crate::{CoreError, Result};

pub const MAX_PROTOCOL_MESSAGE_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AckStatus {
    Received,
    Read,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ProtocolMessage {
    TextMessage {
        id: String,
        chat_id: String,
        sender_id: String,
        content: String,
        timestamp: i64,
    },
    FileOffer {
        id: String,
        filename: String,
        size: u64,
        sha256: String,
        transfer_port: u16,
    },
    FileAccept {
        offer_id: String,
    },
    FileReject {
        offer_id: String,
        reason: Option<String>,
    },
    PeerInfo {
        device_id: String,
        display_name: String,
        avatar_hash: Option<String>,
    },
    GroupCreate {
        group_id: String,
        name: String,
        members: Vec<String>,
    },
    GroupInvite {
        group_id: String,
        inviter_id: String,
    },
    Ack {
        message_id: String,
        status: AckStatus,
    },
    Ping,
    Pong,
}

impl ProtocolMessage {
    fn validate_serialized_size(payload_size: usize) -> Result<()> {
        if payload_size > MAX_PROTOCOL_MESSAGE_BYTES {
            Err(CoreError::Validation(format!(
                "protocol message size exceeds limit of {MAX_PROTOCOL_MESSAGE_BYTES} bytes"
            )))
        } else {
            Ok(())
        }
    }

    pub fn to_json(&self) -> Result<String> {
        let payload = serde_json::to_string(self)?;
        Self::validate_serialized_size(payload.len())?;
        Ok(payload)
    }

    pub fn from_json(raw: &str) -> Result<Self> {
        Self::validate_serialized_size(raw.len())?;
        Ok(serde_json::from_str(raw)?)
    }
}

#[cfg(test)]
mod tests {
    use super::{AckStatus, ProtocolMessage, MAX_PROTOCOL_MESSAGE_BYTES};

    #[test]
    fn protocol_json_roundtrip_with_size_guard_roundtrip() {
        let message = ProtocolMessage::Ping;
        let payload = message.to_json().expect("serialize to json");
        let decoded = ProtocolMessage::from_json(&payload).expect("deserialize from json");

        assert_eq!(decoded, message);
        assert!(payload.len() <= MAX_PROTOCOL_MESSAGE_BYTES);
    }

    #[test]
    fn protocol_to_json_blocks_oversized_payload() {
        let message = ProtocolMessage::TextMessage {
            id: "id".to_string(),
            chat_id: "chat".to_string(),
            sender_id: "sender".to_string(),
            content: "a".repeat(65 * 1024),
            timestamp: 0,
        };

        let err = message
            .to_json()
            .expect_err("serialization should fail due to size");
        let text = format!("{err}");

        assert!(text.contains("exceeds limit"));
    }

    #[test]
    fn ack_status_is_roundtrippable_case() {
        let value = ProtocolMessage::Ack {
            message_id: "m".to_string(),
            status: AckStatus::Read,
        };

        let payload = value.to_json().expect("serialize ack");
        let decoded = ProtocolMessage::from_json(&payload).expect("deserialize ack");

        assert_eq!(value, decoded);
        assert!(payload.contains("\"Read\""));
    }
}
