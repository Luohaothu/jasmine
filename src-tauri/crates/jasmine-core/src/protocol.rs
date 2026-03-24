use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{CoreError, Result};

pub const MAX_PROTOCOL_MESSAGE_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AckStatus {
    Received,
    Read,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CallType {
    Audio,
    Video,
    Screen,
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
        #[serde(default)]
        vector_clock: Option<HashMap<String, u64>>,
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
    CallOffer {
        call_id: String,
        sdp: String,
        caller_id: String,
        call_type: CallType,
    },
    CallAnswer {
        call_id: String,
        sdp: String,
    },
    IceCandidate {
        call_id: String,
        candidate: String,
        sdp_mid: Option<String>,
        sdp_mline_index: Option<u16>,
    },
    CallHangup {
        call_id: String,
    },
    CallReject {
        call_id: String,
        reason: Option<String>,
    },
    CallJoin {
        call_id: String,
        group_id: String,
    },
    CallLeave {
        call_id: String,
    },
    FolderManifest {
        folder_transfer_id: String,
        manifest: FolderManifestData,
        sender_id: String,
    },
    FolderAccept {
        folder_transfer_id: String,
    },
    FolderResumeRequest {
        folder_id: String,
        completed_files: Vec<String>,
        partial_files: Vec<(String, u64)>,
    },
    FolderResumeAccept {
        folder_id: String,
        files_to_send: Vec<FolderFileEntry>,
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
    KeyExchangeInit {
        ephemeral_public_key: String,
        protocol_version: u32,
    },
    KeyExchangeResponse {
        ephemeral_public_key: String,
    },
    VersionIncompatible {
        local_version: u32,
        remote_version: u32,
        message: String,
    },
    SenderKeyDistribution {
        group_id: String,
        sender_key_data: String,
        epoch: u32,
    },
    SenderKeyRequest {
        group_id: String,
        requesting_peer_id: String,
    },
    SenderKeyRotation {
        group_id: String,
        new_sender_key_data: String,
        new_epoch: u32,
    },
    GroupMessage {
        group_id: String,
        sender_key_id: Uuid,
        epoch: u32,
        nonce: [u8; 12],
        encrypted_content: Vec<u8>,
    },
    FileResumeRequest {
        offer_id: String,
        offset: u64,
    },
    FileResumeAccept {
        offer_id: String,
        offset: u64,
    },
    PeerInfo {
        device_id: String,
        display_name: String,
        avatar_hash: Option<String>,
        #[serde(default)]
        public_key: Option<String>,
        #[serde(default)]
        protocol_version: Option<u32>,
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
    GroupMemberLeft {
        group_id: String,
        member_id: String,
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
    use std::collections::HashMap;

    use super::{
        AckStatus, CallType, FolderFileEntry, FolderManifestData, ProtocolMessage,
        MAX_PROTOCOL_MESSAGE_BYTES,
    };

    fn sample_vector_clock() -> HashMap<String, u64> {
        HashMap::from([
            ("device-a".to_string(), 2_u64),
            ("device-b".to_string(), 1_u64),
        ])
    }

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
    fn protocol_text_message_with_vector_clock_roundtrip() {
        let message = ProtocolMessage::TextMessage {
            id: "msg-002".to_string(),
            chat_id: "chat-001".to_string(),
            sender_id: "user-001".to_string(),
            content: "Vector clock attached".to_string(),
            timestamp: 1_700_000_003,
            reply_to_id: None,
            reply_to_preview: None,
            vector_clock: Some(sample_vector_clock()),
        };

        let payload = message
            .to_json()
            .expect("serialize text message with vector clock");
        let decoded = ProtocolMessage::from_json(&payload)
            .expect("deserialize text message with vector clock");

        assert_eq!(message, decoded);
    }

    #[test]
    fn protocol_call_signaling_variants_roundtrip() {
        let messages = vec![
            ProtocolMessage::CallOffer {
                call_id: "call-audio".to_string(),
                sdp: "offer-sdp-audio".to_string(),
                caller_id: "user-001".to_string(),
                call_type: CallType::Audio,
            },
            ProtocolMessage::CallOffer {
                call_id: "call-video".to_string(),
                sdp: "offer-sdp-video".to_string(),
                caller_id: "user-001".to_string(),
                call_type: CallType::Video,
            },
            ProtocolMessage::CallOffer {
                call_id: "call-screen".to_string(),
                sdp: "offer-sdp-screen".to_string(),
                caller_id: "user-001".to_string(),
                call_type: CallType::Screen,
            },
            ProtocolMessage::CallAnswer {
                call_id: "call-answer".to_string(),
                sdp: "answer-sdp".to_string(),
            },
            ProtocolMessage::IceCandidate {
                call_id: "call-ice".to_string(),
                candidate: "candidate:1 1 udp 2122260223 192.168.1.5 54400 typ host".to_string(),
                sdp_mid: Some("0".to_string()),
                sdp_mline_index: Some(0),
            },
            ProtocolMessage::CallHangup {
                call_id: "call-hangup".to_string(),
            },
            ProtocolMessage::CallReject {
                call_id: "call-reject".to_string(),
                reason: Some("busy".to_string()),
            },
            ProtocolMessage::CallJoin {
                call_id: "call-join".to_string(),
                group_id: "group-001".to_string(),
            },
            ProtocolMessage::CallLeave {
                call_id: "call-leave".to_string(),
            },
        ];

        for message in messages {
            let payload = message.to_json().expect("serialize call signaling message");
            let decoded =
                ProtocolMessage::from_json(&payload).expect("deserialize call signaling message");

            assert_eq!(message, decoded);
        }
    }

    #[test]
    fn protocol_text_message_backward_compat_without_vector_clock() {
        let raw_json = r#"
            {
                "type": "TextMessage",
                "id": "msg-legacy",
                "chat_id": "chat-legacy",
                "sender_id": "user-legacy",
                "content": "legacy text",
                "timestamp": 1700000000000
            }
        "#;

        let message =
            ProtocolMessage::from_json(raw_json).expect("deserialize legacy text message payload");

        assert_eq!(
            message,
            ProtocolMessage::TextMessage {
                id: "msg-legacy".to_string(),
                chat_id: "chat-legacy".to_string(),
                sender_id: "user-legacy".to_string(),
                content: "legacy text".to_string(),
                timestamp: 1_700_000_000_000,
                reply_to_id: None,
                reply_to_preview: None,
                vector_clock: None,
            }
        );
    }

    #[test]
    fn protocol_version_incompatible_roundtrip() {
        let message = ProtocolMessage::VersionIncompatible {
            local_version: crate::CURRENT_PROTOCOL_VERSION,
            remote_version: 1,
            message: "peer protocol version 1 is incompatible; minimum supported version is 2"
                .to_string(),
        };

        let payload = message
            .to_json()
            .expect("serialize version incompatible message");
        let decoded =
            ProtocolMessage::from_json(&payload).expect("deserialize version incompatible message");

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
    fn protocol_group_member_left_roundtrip() {
        let message = ProtocolMessage::GroupMemberLeft {
            group_id: "group-001".to_string(),
            member_id: "user-002".to_string(),
        };

        let payload = message.to_json().expect("serialize group member left");
        let decoded = ProtocolMessage::from_json(&payload).expect("deserialize group member left");

        assert_eq!(message, decoded);
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
            vector_clock: None,
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
