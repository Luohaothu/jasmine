use std::collections::HashMap;

use jasmine_core::protocol::{
    AckStatus, CallType, FolderFileEntry, FolderManifestData, ProtocolMessage,
    MAX_PROTOCOL_MESSAGE_BYTES,
};
use jasmine_core::CURRENT_PROTOCOL_VERSION;

fn protocol_roundtrip(message: ProtocolMessage) {
    let json = serde_json::to_string(&message).expect("serialize protocol message to json");
    assert!(json.len() <= MAX_PROTOCOL_MESSAGE_BYTES);

    let roundtrip = serde_json::from_str::<ProtocolMessage>(&json)
        .expect("deserialize protocol message from json");

    assert_eq!(roundtrip, message);
}

fn make_repeating_string(length: usize) -> String {
    "a".repeat(length)
}

fn sample_vector_clock() -> HashMap<String, u64> {
    HashMap::from([
        ("device-a".to_string(), 5_u64),
        ("device-b".to_string(), 3_u64),
    ])
}

#[test]
fn protocol_roundtrip_text_message() {
    protocol_roundtrip(ProtocolMessage::TextMessage {
        id: "msg-001".to_string(),
        chat_id: "chat-001".to_string(),
        sender_id: "user-001".to_string(),
        content: "Hello from protocol".to_string(),
        timestamp: 1_700_000_000_000,
        reply_to_id: None,
        reply_to_preview: None,
        vector_clock: None,
    });
}

#[test]
fn protocol_roundtrip_file_offer() {
    protocol_roundtrip(ProtocolMessage::FileOffer {
        id: "offer-001".to_string(),
        filename: "song.mp3".to_string(),
        size: 1_024,
        sha256: "deadbeef".to_string(),
        transfer_port: 9001,
    });
}

#[test]
fn protocol_roundtrip_text_message_with_reply_fields() {
    protocol_roundtrip(ProtocolMessage::TextMessage {
        id: "msg-002".to_string(),
        chat_id: "chat-002".to_string(),
        sender_id: "user-002".to_string(),
        content: "Reply with context".to_string(),
        timestamp: 1_700_000_000_001,
        reply_to_id: Some("msg-parent-001".to_string()),
        reply_to_preview: Some("Parent preview text".to_string()),
        vector_clock: None,
    });
}

#[test]
fn protocol_roundtrip_text_message_with_vector_clock() {
    protocol_roundtrip(ProtocolMessage::TextMessage {
        id: "msg-vector-clock".to_string(),
        chat_id: "chat-vector-clock".to_string(),
        sender_id: "user-vector-clock".to_string(),
        content: "Clocked message".to_string(),
        timestamp: 1_700_000_000_111,
        reply_to_id: None,
        reply_to_preview: None,
        vector_clock: Some(sample_vector_clock()),
    });
}

#[test]
fn protocol_roundtrip_message_edit() {
    protocol_roundtrip(ProtocolMessage::MessageEdit {
        message_id: "msg-parent-001".to_string(),
        chat_id: "chat-002".to_string(),
        sender_id: "user-002".to_string(),
        new_content: "Updated content".to_string(),
        edit_version: 2,
        timestamp_ms: 1_700_000_100_002,
    });
}

#[test]
fn protocol_roundtrip_message_delete() {
    protocol_roundtrip(ProtocolMessage::MessageDelete {
        message_id: "msg-parent-002".to_string(),
        chat_id: "chat-002".to_string(),
        sender_id: "user-002".to_string(),
        timestamp_ms: 1_700_000_100_003,
    });
}

#[test]
fn protocol_roundtrip_folder_accept_reject() {
    protocol_roundtrip(ProtocolMessage::FolderAccept {
        folder_transfer_id: "folder-transfer-001".to_string(),
    });

    protocol_roundtrip(ProtocolMessage::FolderReject {
        folder_transfer_id: "folder-transfer-002".to_string(),
        reason: "Declined by user".to_string(),
    });
}

#[test]
fn protocol_roundtrip_folder_manifest_with_200_entries() {
    let files = (0..200)
        .map(|index| FolderFileEntry {
            relative_path: format!("notes/file-{index:03}.txt"),
            size: 1_024 + index as u64,
            sha256: format!("{:064x}", index),
        })
        .collect();

    protocol_roundtrip(ProtocolMessage::FolderManifest {
        folder_transfer_id: "folder-transfer-manifest".to_string(),
        sender_id: "sender-001".to_string(),
        manifest: FolderManifestData {
            folder_name: "notes".to_string(),
            total_size: (0..200).map(|index| 1_024 + index as u64).sum(),
            files,
        },
    });
}

#[test]
fn protocol_folder_manifest_200_entries_stays_under_protocol_limit() {
    let files = (0..200)
        .map(|index| FolderFileEntry {
            relative_path: format!("files/deep/path/{index:03}/document.txt"),
            size: 512 + index as u64,
            sha256: format!("{:064x}", 0xabc + index),
        })
        .collect();

    let message = ProtocolMessage::FolderManifest {
        folder_transfer_id: "folder-transfer-size".to_string(),
        sender_id: "sender-size".to_string(),
        manifest: FolderManifestData {
            folder_name: "project".to_string(),
            total_size: (0..200).map(|index| 512 + index as u64).sum(),
            files,
        },
    };

    let payload = serde_json::to_string(&message).expect("serialize 200-entry folder manifest");
    assert!(payload.len() <= MAX_PROTOCOL_MESSAGE_BYTES);
}

#[test]
fn protocol_roundtrip_file_accept() {
    protocol_roundtrip(ProtocolMessage::FileAccept {
        offer_id: "offer-001".to_string(),
    });
}

#[test]
fn protocol_roundtrip_file_reject() {
    protocol_roundtrip(ProtocolMessage::FileReject {
        offer_id: "offer-002".to_string(),
        reason: Some("No space".to_string()),
    });
}

#[test]
fn protocol_roundtrip_key_exchange_init() {
    protocol_roundtrip(ProtocolMessage::KeyExchangeInit {
        ephemeral_public_key: "ephemeral-init-key".to_string(),
        protocol_version: CURRENT_PROTOCOL_VERSION,
    });
}

#[test]
fn protocol_roundtrip_key_exchange_response() {
    protocol_roundtrip(ProtocolMessage::KeyExchangeResponse {
        ephemeral_public_key: "ephemeral-response-key".to_string(),
    });
}

#[test]
fn protocol_roundtrip_sender_key_distribution() {
    protocol_roundtrip(ProtocolMessage::SenderKeyDistribution {
        group_id: "group-key-dist-001".to_string(),
        sender_key_data: "sender-key-data".to_string(),
        epoch: 3,
    });
}

#[test]
fn protocol_roundtrip_sender_key_rotation() {
    protocol_roundtrip(ProtocolMessage::SenderKeyRotation {
        group_id: "group-key-rot-001".to_string(),
        new_sender_key_data: "new-sender-key-data".to_string(),
        new_epoch: 4,
    });
}

#[test]
fn protocol_roundtrip_file_resume_request() {
    protocol_roundtrip(ProtocolMessage::FileResumeRequest {
        offer_id: "offer-resume-001".to_string(),
        offset: 12_345,
    });
}

#[test]
fn protocol_roundtrip_file_resume_accept() {
    protocol_roundtrip(ProtocolMessage::FileResumeAccept {
        offer_id: "offer-resume-001".to_string(),
        offset: 12_345,
    });
}

#[test]
fn protocol_roundtrip_peer_info() {
    protocol_roundtrip(ProtocolMessage::PeerInfo {
        device_id: "dev-001".to_string(),
        display_name: "desktop-room".to_string(),
        avatar_hash: None,
        public_key: None,
        protocol_version: None,
    });
}

#[test]
fn protocol_peer_info_backward_compatible_without_new_fields() {
    let raw_json = r#"
        {
            "type": "PeerInfo",
            "device_id": "dev-legacy",
            "display_name": "legacy-device",
            "avatar_hash": "avatar-legacy"
        }
    "#;

    let message =
        serde_json::from_str::<ProtocolMessage>(raw_json).expect("deserialize legacy peer info");

    assert_eq!(
        message,
        ProtocolMessage::PeerInfo {
            device_id: "dev-legacy".to_string(),
            display_name: "legacy-device".to_string(),
            avatar_hash: Some("avatar-legacy".to_string()),
            public_key: None,
            protocol_version: None,
        }
    );
}

#[test]
fn protocol_peer_info_serializes_with_optional_fields() {
    let message = ProtocolMessage::PeerInfo {
        device_id: "dev-002".to_string(),
        display_name: "secure-device".to_string(),
        avatar_hash: Some("avatar-002".to_string()),
        public_key: Some("base64-25519-key".to_string()),
        protocol_version: Some(CURRENT_PROTOCOL_VERSION),
    };

    let json = serde_json::to_string(&message).expect("serialize peer info with optional fields");
    let parsed = serde_json::from_str::<serde_json::Value>(&json).expect("parse peer info json");

    assert_eq!(parsed["type"], "PeerInfo");
    assert_eq!(parsed["device_id"], "dev-002");
    assert_eq!(parsed["display_name"], "secure-device");
    assert_eq!(parsed["avatar_hash"], "avatar-002");
    assert_eq!(parsed["public_key"], "base64-25519-key");
    assert_eq!(parsed["protocol_version"], CURRENT_PROTOCOL_VERSION);
}

#[test]
fn call_offer_roundtrip_includes_all_signaling_variants() {
    protocol_roundtrip(ProtocolMessage::CallOffer {
        call_id: "call-audio".to_string(),
        sdp: "offer-audio".to_string(),
        caller_id: "caller-audio".to_string(),
        call_type: CallType::Audio,
    });
    protocol_roundtrip(ProtocolMessage::CallOffer {
        call_id: "call-video".to_string(),
        sdp: "offer-video".to_string(),
        caller_id: "caller-video".to_string(),
        call_type: CallType::Video,
    });
    protocol_roundtrip(ProtocolMessage::CallOffer {
        call_id: "call-screen".to_string(),
        sdp: "offer-screen".to_string(),
        caller_id: "caller-screen".to_string(),
        call_type: CallType::Screen,
    });
    protocol_roundtrip(ProtocolMessage::CallAnswer {
        call_id: "call-answer".to_string(),
        sdp: "answer-sdp".to_string(),
    });
    protocol_roundtrip(ProtocolMessage::IceCandidate {
        call_id: "call-ice".to_string(),
        candidate: "candidate:1 1 udp 2122260223 192.168.1.5 54400 typ host".to_string(),
        sdp_mid: Some("0".to_string()),
        sdp_mline_index: Some(0),
    });
    protocol_roundtrip(ProtocolMessage::CallHangup {
        call_id: "call-hangup".to_string(),
    });
    protocol_roundtrip(ProtocolMessage::CallReject {
        call_id: "call-reject".to_string(),
        reason: Some("busy".to_string()),
    });
    protocol_roundtrip(ProtocolMessage::CallJoin {
        call_id: "call-join".to_string(),
        group_id: "group-001".to_string(),
    });
    protocol_roundtrip(ProtocolMessage::CallLeave {
        call_id: "call-leave".to_string(),
    });
}

#[test]
fn protocol_roundtrip_group_create() {
    protocol_roundtrip(ProtocolMessage::GroupCreate {
        group_id: "group-001".to_string(),
        name: "friends".to_string(),
        members: vec!["u1".to_string(), "u2".to_string()],
    });
}

#[test]
fn protocol_roundtrip_group_invite() {
    protocol_roundtrip(ProtocolMessage::GroupInvite {
        group_id: "group-001".to_string(),
        inviter_id: "u1".to_string(),
    });
}

#[test]
fn protocol_roundtrip_ack_received() {
    protocol_roundtrip(ProtocolMessage::Ack {
        message_id: "msg-001".to_string(),
        status: AckStatus::Received,
    });
}

#[test]
fn protocol_roundtrip_ack_read() {
    protocol_roundtrip(ProtocolMessage::Ack {
        message_id: "msg-001".to_string(),
        status: AckStatus::Read,
    });
}

#[test]
fn protocol_roundtrip_ping() {
    protocol_roundtrip(ProtocolMessage::Ping);
}

#[test]
fn protocol_roundtrip_pong() {
    protocol_roundtrip(ProtocolMessage::Pong);
}

#[test]
fn protocol_max_size_rejects_oversized_text_messages() {
    let message = ProtocolMessage::TextMessage {
        id: "msg-oversize".to_string(),
        chat_id: "chat-oversize".to_string(),
        sender_id: "user-oversize".to_string(),
        content: make_repeating_string(65 * 1024),
        timestamp: 1_700_000_000_000,
        reply_to_id: None,
        reply_to_preview: None,
        vector_clock: None,
    };

    let json = serde_json::to_string(&message).expect("serialize message for test");
    assert!(json.len() > MAX_PROTOCOL_MESSAGE_BYTES);
}

#[test]
fn protocol_max_size_accepts_within_limit() {
    let message = ProtocolMessage::TextMessage {
        id: "msg-small".to_string(),
        chat_id: "chat-small".to_string(),
        sender_id: "user-small".to_string(),
        content: make_repeating_string(63 * 1024),
        timestamp: 1_700_000_000_000,
        reply_to_id: None,
        reply_to_preview: None,
        vector_clock: None,
    };

    let json = serde_json::to_string(&message).expect("serialize message for limit test");
    assert!(json.len() <= MAX_PROTOCOL_MESSAGE_BYTES);
}

#[test]
fn protocol_unknown_fields_are_ignored_on_deserialize() {
    let raw_json = r#"
        {
            "type": "TextMessage",
            "id": "msg-extra",
            "chat_id": "chat-extra",
            "sender_id": "user-extra",
            "content": "hello",
            "timestamp": 1700000000000,
            "future_feature": "reserved",
            "unused_flag": true
        }
    "#;

    let message = serde_json::from_str::<ProtocolMessage>(raw_json)
        .expect("deserialize message with unknown fields");

    assert_eq!(
        message,
        ProtocolMessage::TextMessage {
            id: "msg-extra".to_string(),
            chat_id: "chat-extra".to_string(),
            sender_id: "user-extra".to_string(),
            content: "hello".to_string(),
            timestamp: 1700000000000,
            reply_to_id: None,
            reply_to_preview: None,
            vector_clock: None,
        }
    );
}

#[test]
fn text_message_backward_compat_without_reply_or_vector_clock_fields() {
    let raw_json = r#"
        {
            "type": "TextMessage",
            "id": "msg-reply-backward",
            "chat_id": "chat-backward",
            "sender_id": "user-backward",
            "content": "legacy",
            "timestamp": 1700000000000
        }
    "#;

    let message = serde_json::from_str::<ProtocolMessage>(raw_json)
        .expect("deserialize backward-compatible text message");

    assert_eq!(
        message,
        ProtocolMessage::TextMessage {
            id: "msg-reply-backward".to_string(),
            chat_id: "chat-backward".to_string(),
            sender_id: "user-backward".to_string(),
            content: "legacy".to_string(),
            timestamp: 1700000000000,
            reply_to_id: None,
            reply_to_preview: None,
            vector_clock: None,
        }
    );
}

#[test]
fn protocol_timestamp_is_unix_epoch_milliseconds() {
    let message = ProtocolMessage::TextMessage {
        id: "msg-ts".to_string(),
        chat_id: "chat-ts".to_string(),
        sender_id: "user-ts".to_string(),
        content: "timestamp test".to_string(),
        timestamp: 1_760_000_000_000,
        reply_to_id: None,
        reply_to_preview: None,
        vector_clock: None,
    };

    let payload = serde_json::to_value(&message).expect("convert to json value");
    let timestamp = payload
        .get("timestamp")
        .and_then(serde_json::Value::as_i64)
        .expect("timestamp should be integer");

    assert!(timestamp > 0);
}
