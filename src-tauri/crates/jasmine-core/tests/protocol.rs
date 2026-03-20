use jasmine_core::protocol::{AckStatus, ProtocolMessage, MAX_PROTOCOL_MESSAGE_BYTES};

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

#[test]
fn protocol_roundtrip_text_message() {
    protocol_roundtrip(ProtocolMessage::TextMessage {
        id: "msg-001".to_string(),
        chat_id: "chat-001".to_string(),
        sender_id: "user-001".to_string(),
        content: "Hello from protocol".to_string(),
        timestamp: 1_700_000_000_000,
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
fn protocol_roundtrip_peer_info() {
    protocol_roundtrip(ProtocolMessage::PeerInfo {
        device_id: "dev-001".to_string(),
        display_name: "desktop-room".to_string(),
        avatar_hash: None,
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
    };

    let payload = serde_json::to_value(&message).expect("convert to json value");
    let timestamp = payload
        .get("timestamp")
        .and_then(serde_json::Value::as_i64)
        .expect("timestamp should be integer");

    assert!(timestamp > 0);
}
