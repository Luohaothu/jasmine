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
        #[serde(default)]
        reply_to_id: Option<String>,
        #[serde(default)]
        reply_to_preview: Option<String>,
    },
    MessageEdit {
        message_id: String,
        chat_id: String,
        sender_id: String,
        new_content: String,
        edit_version: u32,
        timestamp_ms: u64,
    },
    MessageDelete {
        message_id: String,
        chat_id: String,
        sender_id: String,
        timestamp_ms: u64,
    },
    FolderManifest {
        folder_transfer_id: String,
        manifest: FolderManifestData,
        sender_id: String,
    },
    FolderAccept {
        folder_transfer_id: String,
    },
    FolderReject {
        folder_transfer_id: String,
        reason: String,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderManifestData {
    pub folder_name: String,
    pub files: Vec<FolderFileEntry>,
    pub total_size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderFileEntry {
    pub relative_path: String,
    pub size: u64,
    pub sha256: String,
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
    use super::{
        AckStatus, FolderFileEntry, FolderManifestData, ProtocolMessage, MAX_PROTOCOL_MESSAGE_BYTES,
    };

    #[test]
    fn protocol_json_roundtrip_with_size_guard_roundtrip() {
        let message = ProtocolMessage::Ping;
        let payload = message.to_json().expect("serialize to json");
        let decoded = ProtocolMessage::from_json(&payload).expect("deserialize from json");

        assert_eq!(decoded, message);
        assert!(payload.len() <= MAX_PROTOCOL_MESSAGE_BYTES);
    }

    #[test]
    fn protocol_message_edit_roundtrip() {
        let message = ProtocolMessage::MessageEdit {
            message_id: "msg-001".to_string(),
            chat_id: "chat-001".to_string(),
            sender_id: "user-001".to_string(),
            new_content: "Edited content".to_string(),
            edit_version: 2,
            timestamp_ms: 1_700_000_001,
        };

        let payload = message.to_json().expect("serialize message edit");
        let decoded = ProtocolMessage::from_json(&payload).expect("deserialize message edit");

        assert_eq!(message, decoded);
    }

    #[test]
    fn protocol_message_delete_roundtrip() {
        let message = ProtocolMessage::MessageDelete {
            message_id: "msg-001".to_string(),
            chat_id: "chat-001".to_string(),
            sender_id: "user-001".to_string(),
            timestamp_ms: 1_700_000_002,
        };

        let payload = message.to_json().expect("serialize message delete");
        let decoded = ProtocolMessage::from_json(&payload).expect("deserialize message delete");

        assert_eq!(message, decoded);
    }

    #[test]
    fn protocol_folder_manifest_roundtrip() {
        let message = ProtocolMessage::FolderManifest {
            folder_transfer_id: "folder-001".to_string(),
            sender_id: "user-001".to_string(),
            manifest: FolderManifestData {
                folder_name: "notes".to_string(),
                total_size: 3_145,
                files: vec![
                    FolderFileEntry {
                        relative_path: "a.txt".to_string(),
                        size: 1_024,
                        sha256: "abc123".to_string(),
                    },
                    FolderFileEntry {
                        relative_path: "b.txt".to_string(),
                        size: 2_121,
                        sha256: "def456".to_string(),
                    },
                ],
            },
        };

        let payload = message.to_json().expect("serialize folder manifest");
        let decoded = ProtocolMessage::from_json(&payload).expect("deserialize folder manifest");

        assert_eq!(message, decoded);
    }

    #[test]
    fn protocol_folder_accept_reject_roundtrip() {
        let accept = ProtocolMessage::FolderAccept {
            folder_transfer_id: "folder-002".to_string(),
        };

        let reject = ProtocolMessage::FolderReject {
            folder_transfer_id: "folder-003".to_string(),
            reason: "Declined by user".to_string(),
        };

        let accept_payload = accept.to_json().expect("serialize folder accept");
        let reject_payload = reject.to_json().expect("serialize folder reject");
        let decoded_accept =
            ProtocolMessage::from_json(&accept_payload).expect("deserialize folder accept");
        let decoded_reject =
            ProtocolMessage::from_json(&reject_payload).expect("deserialize folder reject");

        assert_eq!(accept, decoded_accept);
        assert_eq!(reject, decoded_reject);
    }

    #[test]
    fn protocol_to_json_blocks_oversized_payload() {
        let message = ProtocolMessage::TextMessage {
            id: "id".to_string(),
            chat_id: "chat".to_string(),
            sender_id: "sender".to_string(),
            content: "a".repeat(65 * 1024),
            timestamp: 0,
            reply_to_id: None,
            reply_to_preview: None,
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
