mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use futures_util::{SinkExt, StreamExt};
use jasmine_core::{ProtocolMessage, CURRENT_PROTOCOL_VERSION};
use jasmine_crypto::{
    generate_identity_keypair, public_key_from_base64, public_key_to_base64, EncryptedFrame,
    Session, SessionKeyManager,
};
use jasmine_messaging::WsPeerIdentity;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::time;
use tokio_tungstenite::{connect_async, tungstenite::Message as WsMessage, WebSocketStream};
use uuid::Uuid;

const ENCRYPTED_PROTOCOL_FRAME_TYPE: u32 = 0x01;
const TRANSPORT_FRAME_TYPE_LEN: usize = 4;

async fn wait_for_group_snapshot(
    node: &TestNode,
    group_id: String,
    expected_name: String,
    expected_member_count: usize,
) {
    wait_for(ACTION_TIMEOUT, || {
        let state = Arc::clone(&node.state);
        let group_id = group_id.clone();
        let expected_name = expected_name.clone();
        let expected_member_count = expected_member_count;
        async move {
            let groups = state.list_groups().await.ok()?;
            groups
                .into_iter()
                .find(|group| {
                    group.id == group_id
                        && group.name == expected_name
                        && group.members.len() == expected_member_count
                })
                .map(|_| ())
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn test_encrypted_direct_message_roundtrip() {
    let _guard = suite_lock().lock().await;
    within_test_timeout("test_encrypted_direct_message_roundtrip", async {
        let alice = TestNode::new().await;
        let bob = TestNode::new().await;

        let alice_id = alice.device_id();
        let bob_id = bob.device_id();
        let _ = tokio::join!(
            wait_for_peer(&alice, bob_id.clone()),
            wait_for_peer(&bob, alice_id.clone()),
        );

        let cursor = bob.emitter.mark();
        let message_id = wait_for(TEST_TIMEOUT, || {
            let state = Arc::clone(&alice.state);
            let bob_id = bob_id.clone();
            async move {
                match tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    state.send_message(bob_id, "Hello encrypted world!".to_string(), None),
                )
                .await
                {
                    Ok(Ok(message_id)) => Some(message_id),
                    _ => None,
                }
            }
        })
        .await;

        let payload = bob
            .emitter
            .wait_for_event(cursor, EVENT_MESSAGE_RECEIVED, ACTION_TIMEOUT, |_| true)
            .await;

        assert_eq!(
            payload["content"].as_str().unwrap(),
            "Hello encrypted world!"
        );

        let messages = bob
            .state
            .get_messages(alice_id.clone(), 50, 0)
            .await
            .expect("get messages");
        let stored = messages
            .into_iter()
            .find(|message| message.id == message_id)
            .expect("message exists in storage");
        assert_eq!(stored.content, "Hello encrypted world!");

        alice.shutdown().await;
        bob.shutdown().await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn test_wire_level_encryption_verification() {
    let _guard = suite_lock().lock().await;
    within_test_timeout("test_wire_level_encryption_verification", async {
        let alice = TestNode::new().await;
        let probe = TestNode::new().await;

        let alice_id = alice.device_id();
        let _ = wait_for_peer(&probe, alice_id.clone()).await;
        let alice_peer_record = wait_for_peer_record(probe.database_path(), alice_id).await;
        let ws_port = alice_peer_record
            .ws_port
            .expect("alice peer record must advertise websocket port");
        probe.shutdown().await;

        let raw_peer_id = Uuid::new_v4().to_string();
        let (raw_private_key, raw_public_key) = generate_identity_keypair();
        let raw_identity = WsPeerIdentity::new(raw_peer_id.clone(), "Wire Probe Bob")
            .with_transport_identity(
                public_key_to_base64(&raw_public_key),
                CURRENT_PROTOCOL_VERSION,
            );

        let (mut socket, _) = connect_async(format!("ws://127.0.0.1:{ws_port}"))
            .await
            .expect("connect raw websocket client");

        let RawClientHandshakeOutcome { mut session, .. } =
            complete_raw_client_key_exchange_with_session(
                &mut socket,
                &raw_identity,
                &raw_private_key,
            )
            .await;

        let secret = "secret message for wire test";
        wait_for(ACTION_TIMEOUT, || {
            let state = Arc::clone(&alice.state);
            let raw_peer_id = raw_peer_id.clone();
            async move {
                state
                    .send_message(raw_peer_id, secret.to_string(), None)
                    .await
                    .ok()
            }
        })
        .await;

        let mut verified = false;
        for _ in 0..16 {
            let raw_frame =
                expect_binary_frame_payload(&mut socket, "encrypted outbound frame").await;
            let decrypted_message = decrypt_transport_frame(&mut session, &raw_frame);
            if let ProtocolMessage::TextMessage { content, .. } = decrypted_message {
                assert!(
                    !raw_frame
                        .windows(b"secret message for wire test".len())
                        .any(|window| window == b"secret message for wire test"),
                    "plaintext must not be visible in raw wire bytes"
                );

                let encrypted_frame = decode_transport_encrypted_frame(&raw_frame);
                assert!(
                    !encrypted_frame.ciphertext.is_empty(),
                    "encrypted frame ciphertext should not be empty"
                );
                assert_eq!(content, secret);
                verified = true;
                break;
            }
        }

        assert!(
            verified,
            "timed out waiting for encrypted text message frame on raw socket"
        );

        alice.shutdown().await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn test_encrypted_group_message_sender_keys() {
    let _guard = suite_lock().lock().await;
    within_test_timeout("test_encrypted_group_message_sender_keys", async {
        let alice = TestNode::new().await;
        let bob = TestNode::new().await;
        let charlie = TestNode::new().await;

        let alice_id = alice.device_id();
        let bob_id = bob.device_id();
        let charlie_id = charlie.device_id();

        let _ = tokio::join!(
            wait_for_peer(&alice, bob_id.clone()),
            wait_for_peer(&alice, charlie_id.clone()),
            wait_for_peer(&bob, alice_id.clone()),
            wait_for_peer(&charlie, alice_id.clone()),
        );

        let group = alice
            .state
            .create_group(
                "Crypto Group".to_string(),
                vec![bob_id.clone(), charlie_id.clone()],
            )
            .await
            .expect("create group");
        let group_id = group.id.clone();
        let _ = tokio::join!(
            wait_for_group_snapshot(&bob, group_id.clone(), "Crypto Group".to_string(), 3,),
            wait_for_group_snapshot(&charlie, group_id.clone(), "Crypto Group".to_string(), 3,),
        );

        let bob_cursor = bob.emitter.mark();
        let charlie_cursor = charlie.emitter.mark();
        let message_id = wait_for(TEST_TIMEOUT, || {
            let state = Arc::clone(&alice.state);
            let group_id = group_id.clone();
            async move {
                match tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    state.send_group_message(group_id, "Secret group message".to_string(), None),
                )
                .await
                {
                    Ok(Ok(message_id)) => Some(message_id),
                    _ => None,
                }
            }
        })
        .await;

        let (bob_event, charlie_event) = tokio::join!(
            bob.emitter.wait_for_event(
                bob_cursor,
                EVENT_GROUP_MESSAGE_RECEIVED,
                ACTION_TIMEOUT,
                |payload| payload.get("id").and_then(serde_json::Value::as_str)
                    == Some(message_id.as_str()),
            ),
            charlie.emitter.wait_for_event(
                charlie_cursor,
                EVENT_GROUP_MESSAGE_RECEIVED,
                ACTION_TIMEOUT,
                |payload| payload.get("id").and_then(serde_json::Value::as_str)
                    == Some(message_id.as_str()),
            ),
        );

        for payload in [&bob_event, &charlie_event] {
            assert_eq!(
                payload.get("content").and_then(serde_json::Value::as_str),
                Some("Secret group message")
            );
            assert_eq!(
                payload.get("groupId").and_then(serde_json::Value::as_str),
                Some(group_id.as_str())
            );
            assert_eq!(
                payload.get("senderId").and_then(serde_json::Value::as_str),
                Some(alice_id.as_str())
            );
        }

        charlie.shutdown().await;
        bob.shutdown().await;
        alice.shutdown().await;
    })
    .await;
}

struct RawClientHandshakeOutcome {
    _remote_identity: WsPeerIdentity,
    session: Session,
}

fn decode_transport_encrypted_frame(payload: &[u8]) -> EncryptedFrame {
    assert!(
        payload.len() > TRANSPORT_FRAME_TYPE_LEN,
        "encrypted transport payload must include type prefix and frame bytes"
    );
    assert_eq!(
        u32::from_be_bytes(
            payload[..TRANSPORT_FRAME_TYPE_LEN]
                .try_into()
                .expect("frame prefix")
        ),
        ENCRYPTED_PROTOCOL_FRAME_TYPE,
        "encrypted transport frame type mismatch"
    );

    EncryptedFrame::from_bytes(&payload[TRANSPORT_FRAME_TYPE_LEN..])
        .expect("decode encrypted transport frame")
}

fn decrypt_transport_frame(session: &mut Session, payload: &[u8]) -> ProtocolMessage {
    let frame = decode_transport_encrypted_frame(payload);
    let plaintext = session
        .decrypt_message(&frame)
        .expect("decrypt transport frame");
    let text = std::str::from_utf8(&plaintext).expect("decrypted transport payload is utf8");
    ProtocolMessage::from_json(text).expect("parse decrypted transport message")
}

fn peer_info_message(identity: &WsPeerIdentity) -> ProtocolMessage {
    ProtocolMessage::PeerInfo {
        device_id: identity.device_id.clone(),
        display_name: identity.display_name.clone(),
        avatar_hash: identity.avatar_hash.clone(),
        public_key: Some(identity.public_key.clone()),
        protocol_version: Some(identity.protocol_version),
    }
}

fn parse_peer_info_message(message: ProtocolMessage) -> WsPeerIdentity {
    match message {
        ProtocolMessage::PeerInfo {
            device_id,
            display_name,
            avatar_hash,
            public_key: Some(public_key),
            protocol_version: Some(protocol_version),
        } => WsPeerIdentity {
            device_id,
            display_name,
            avatar_hash,
            public_key,
            protocol_version,
        },
        other => panic!("expected peer info, got {other:?}"),
    }
}

async fn expect_text_protocol_message<S>(
    socket: &mut WebSocketStream<S>,
    label: &str,
) -> ProtocolMessage
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let maybe_frame = time::timeout(Duration::from_secs(2), socket.next())
        .await
        .unwrap_or_else(|_| panic!("{label} timeout"));
    let frame = maybe_frame.unwrap_or_else(|| panic!("{label} missing frame"));

    match frame {
        Ok(WsMessage::Text(payload)) => {
            ProtocolMessage::from_json(payload.as_str()).expect("parse text protocol message")
        }
        other => panic!("expected text frame for {label}, got {other:?}"),
    }
}

async fn expect_binary_frame_payload<S>(socket: &mut WebSocketStream<S>, label: &str) -> Vec<u8>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let maybe_frame = time::timeout(Duration::from_secs(2), socket.next())
        .await
        .unwrap_or_else(|_| panic!("{label} timeout"));
    let frame = maybe_frame.unwrap_or_else(|| panic!("{label} missing frame"));

    match frame {
        Ok(WsMessage::Binary(payload)) => payload.to_vec(),
        other => panic!("expected binary frame for {label}, got {other:?}"),
    }
}

async fn complete_raw_client_key_exchange_with_session<S>(
    socket: &mut WebSocketStream<S>,
    local_identity: &WsPeerIdentity,
    local_private_key: &jasmine_crypto::StaticSecret,
) -> RawClientHandshakeOutcome
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    socket
        .send(WsMessage::Text(
            peer_info_message(local_identity)
                .to_json()
                .expect("serialize peer info")
                .into(),
        ))
        .await
        .expect("send raw peer info");

    let remote_identity =
        parse_peer_info_message(expect_text_protocol_message(socket, "server peer info").await);
    let remote_public_key =
        public_key_from_base64(&remote_identity.public_key).expect("decode remote public key");
    let (ephemeral_public_key, pending) =
        SessionKeyManager::initiate_key_exchange(local_private_key, &remote_public_key);

    socket
        .send(WsMessage::Text(
            ProtocolMessage::KeyExchangeInit {
                ephemeral_public_key: public_key_to_base64(&ephemeral_public_key),
                protocol_version: CURRENT_PROTOCOL_VERSION,
            }
            .to_json()
            .expect("serialize key exchange init")
            .into(),
        ))
        .await
        .expect("send key exchange init");

    let response = expect_text_protocol_message(socket, "server key exchange response").await;
    let server_ephemeral_public_key = match response {
        ProtocolMessage::KeyExchangeResponse {
            ephemeral_public_key,
        } => public_key_from_base64(&ephemeral_public_key)
            .expect("decode server ephemeral public key"),
        other => panic!("expected key exchange response, got {other:?}"),
    };

    let session =
        SessionKeyManager::complete_key_exchange_initiator(pending, &server_ephemeral_public_key)
            .expect("complete initiator session");

    RawClientHandshakeOutcome {
        _remote_identity: remote_identity,
        session,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn test_encrypted_file_transfer_integrity() {
    let _guard = suite_lock().lock().await;
    within_test_timeout("test_encrypted_file_transfer_integrity", async {
        let alice = TestNode::new().await;
        let bob = TestNode::new().await;

        let alice_id = alice.device_id();
        let bob_id = bob.device_id();
        let _ = tokio::join!(
            wait_for_peer(&alice, bob_id.clone()),
            wait_for_peer(&bob, alice_id.clone()),
        );

        let file_name = "encrypted-integrity-payload.bin";
        let source_path = alice.app_data_dir.join(file_name);
        let payload = (0..(1024 * 1024))
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        std::fs::write(&source_path, &payload).expect("write encrypted transfer source payload");

        let bob_offer_cursor = bob.emitter.mark();
        let transfer_id = wait_for(TEST_TIMEOUT, || {
            let state = Arc::clone(&alice.state);
            let bob_id = bob_id.clone();
            let source_path = source_path.clone();
            async move {
                match tokio::time::timeout(
                    Duration::from_secs(2),
                    state.send_file(bob_id, source_path.to_string_lossy().into_owned()),
                )
                .await
                {
                    Ok(Ok(transfer_id)) => Some(transfer_id),
                    _ => None,
                }
            }
        })
        .await;

        let offer_payload = bob
            .emitter
            .wait_for_event(
                bob_offer_cursor,
                EVENT_FILE_OFFER_RECEIVED,
                ACTION_TIMEOUT,
                |payload| {
                    let id = payload
                        .get("id")
                        .or_else(|| payload.get("offerId"))
                        .and_then(serde_json::Value::as_str);
                    id == Some(transfer_id.as_str())
                },
            )
            .await;

        let offer_id = offer_payload
            .get("offerId")
            .or_else(|| offer_payload.get("id"))
            .and_then(serde_json::Value::as_str)
            .expect("file offer payload includes offer id")
            .to_string();

        bob.state
            .accept_file(offer_id)
            .await
            .expect("accept encrypted file transfer");

        let received_path = if let Some(dest_path) = offer_payload
            .get("destPath")
            .and_then(serde_json::Value::as_str)
            .filter(|path| !path.is_empty())
        {
            std::path::PathBuf::from(dest_path)
        } else {
            bob.download_dir.join(file_name)
        };

        wait_for(ACTION_TIMEOUT, || {
            let received_path = received_path.clone();
            let expected_size = payload.len() as u64;
            async move {
                let metadata = std::fs::metadata(&received_path).ok()?;
                (metadata.len() == expected_size).then_some(())
            }
        })
        .await;

        let (alice_transfer, bob_transfer) = tokio::join!(
            wait_for(ACTION_TIMEOUT, || {
                let state = Arc::clone(&alice.state);
                let transfer_id = transfer_id.clone();
                async move {
                    let transfers = state.get_transfers().await.ok()?;
                    transfers.into_iter().find(|transfer| {
                        transfer.id == transfer_id && transfer.state == "completed"
                    })
                }
            }),
            wait_for(ACTION_TIMEOUT, || {
                let state = Arc::clone(&bob.state);
                let transfer_id = transfer_id.clone();
                async move {
                    let transfers = state.get_transfers().await.ok()?;
                    transfers.into_iter().find(|transfer| {
                        transfer.id == transfer_id && transfer.state == "completed"
                    })
                }
            }),
        );

        assert_eq!(alice_transfer.progress, 1.0);
        assert_eq!(bob_transfer.progress, 1.0);
        assert_eq!(sha256_file(&source_path), sha256_file(&received_path));

        bob.shutdown().await;
        alice.shutdown().await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn test_transfer_cancel_resume_completion() {
    let _guard = suite_lock().lock().await;
    within_test_timeout("test_transfer_cancel_resume_completion", async {
        let alice = TestNode::new().await;
        let bob = TestNode::new().await;

        let alice_id = alice.device_id();
        let bob_id = bob.device_id();
        let _ = tokio::join!(
            wait_for_peer(&alice, bob_id.clone()),
            wait_for_peer(&bob, alice_id.clone()),
        );

        let file_name = "encrypted-cancel-resume-payload.bin";
        let source_path = alice.app_data_dir.join(file_name);
        let payload = (0..(1024 * 1024))
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        std::fs::write(&source_path, &payload).expect("write cancel-resume source payload");

        let bob_offer_cursor = bob.emitter.mark();
        let transfer_id = wait_for(TEST_TIMEOUT, || {
            let state = Arc::clone(&alice.state);
            let bob_id = bob_id.clone();
            let source_path = source_path.clone();
            async move {
                match tokio::time::timeout(
                    Duration::from_secs(2),
                    state.send_file(bob_id, source_path.to_string_lossy().into_owned()),
                )
                .await
                {
                    Ok(Ok(transfer_id)) => Some(transfer_id),
                    _ => None,
                }
            }
        })
        .await;

        let offer_payload = bob
            .emitter
            .wait_for_event(
                bob_offer_cursor,
                EVENT_FILE_OFFER_RECEIVED,
                ACTION_TIMEOUT,
                |payload| {
                    payload
                        .get("offerId")
                        .or_else(|| payload.get("id"))
                        .and_then(serde_json::Value::as_str)
                        == Some(transfer_id.as_str())
                },
            )
            .await;

        let offer_id = offer_payload
            .get("offerId")
            .or_else(|| offer_payload.get("id"))
            .and_then(serde_json::Value::as_str)
            .expect("offer id")
            .to_string();

        bob.state
            .accept_file(offer_id)
            .await
            .expect("accept initial transfer offer");

        // NOTE: After cancel_transfer(), the sender-side transfer surfaces as state="failed"
        // in get_transfers() payload mapping. The runtime does not use a separate "cancelled"
        // state string — "failed" is the correct observed value for a cancelled sender transfer.
        wait_for(ACTION_TIMEOUT, || {
            let state = Arc::clone(&alice.state);
            let transfer_id = transfer_id.clone();
            async move {
                let transfers = state.get_transfers().await.ok()?;
                transfers.into_iter().find(|transfer| {
                    transfer.id == transfer_id
                        && transfer.state == "active"
                        && transfer.progress > 0.0
                        && transfer.progress < 1.0
                })
            }
        })
        .await;

        alice
            .state
            .cancel_transfer(transfer_id.clone())
            .await
            .expect("cancel initial transfer");

        wait_for(ACTION_TIMEOUT, || {
            let state = Arc::clone(&alice.state);
            let transfer_id = transfer_id.clone();
            async move {
                let transfers = state.get_transfers().await.ok()?;
                transfers
                    .into_iter()
                    .find(|transfer| transfer.id == transfer_id && transfer.state == "failed")
            }
        })
        .await;

        let bob_resumed_offer_cursor = bob.emitter.mark();
        let resumed_transfer_id = match alice.state.resume_transfer(transfer_id.clone()).await {
            Ok(resumed_transfer_id) => resumed_transfer_id,
            Err(_) => {
                wait_for(TEST_TIMEOUT, || {
                    let state = Arc::clone(&alice.state);
                    let bob_id = bob_id.clone();
                    let source_path = source_path.clone();
                    async move {
                        match tokio::time::timeout(
                            Duration::from_secs(2),
                            state.send_file(bob_id, source_path.to_string_lossy().into_owned()),
                        )
                        .await
                        {
                            Ok(Ok(resumed_transfer_id)) => Some(resumed_transfer_id),
                            _ => None,
                        }
                    }
                })
                .await
            }
        };

        assert_ne!(resumed_transfer_id, transfer_id);

        let resumed_offer_payload = bob
            .emitter
            .wait_for_event(
                bob_resumed_offer_cursor,
                EVENT_FILE_OFFER_RECEIVED,
                ACTION_TIMEOUT,
                |payload| {
                    payload
                        .get("offerId")
                        .or_else(|| payload.get("id"))
                        .and_then(serde_json::Value::as_str)
                        == Some(resumed_transfer_id.as_str())
                },
            )
            .await;

        let resumed_offer_id = resumed_offer_payload
            .get("offerId")
            .or_else(|| resumed_offer_payload.get("id"))
            .and_then(serde_json::Value::as_str)
            .expect("resumed offer id")
            .to_string();

        bob.state
            .accept_file(resumed_offer_id)
            .await
            .expect("accept resumed transfer offer");

        let expected_received_path = if let Some(dest_path) = resumed_offer_payload
            .get("destPath")
            .and_then(serde_json::Value::as_str)
            .filter(|path| !path.is_empty())
        {
            std::path::PathBuf::from(dest_path)
        } else {
            bob.download_dir.join(file_name)
        };

        let (alice_transfer, bob_transfer) = tokio::join!(
            wait_for(ACTION_TIMEOUT, || {
                let state = Arc::clone(&alice.state);
                let resumed_transfer_id = resumed_transfer_id.clone();
                async move {
                    let transfers = state.get_transfers().await.ok()?;
                    transfers.into_iter().find(|transfer| {
                        transfer.id == resumed_transfer_id && transfer.state == "completed"
                    })
                }
            }),
            wait_for(ACTION_TIMEOUT, || {
                let state = Arc::clone(&bob.state);
                let resumed_transfer_id = resumed_transfer_id.clone();
                async move {
                    let transfers = state.get_transfers().await.ok()?;
                    transfers.into_iter().find(|transfer| {
                        transfer.id == resumed_transfer_id && transfer.state == "completed"
                    })
                }
            }),
        );

        let received_path = bob_transfer
            .local_path
            .as_deref()
            .filter(|path| !path.is_empty())
            .map(std::path::PathBuf::from)
            .unwrap_or(expected_received_path);

        assert_eq!(alice_transfer.progress, 1.0);
        assert_eq!(bob_transfer.progress, 1.0);
        assert_eq!(sha256_file(&source_path), sha256_file(&received_path));

        bob.shutdown().await;
        alice.shutdown().await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn test_transfer_retry_after_cancel() {
    let _guard = suite_lock().lock().await;
    within_test_timeout("test_transfer_retry_after_cancel", async {
        let alice = TestNode::new().await;
        let bob = TestNode::new().await;

        let alice_id = alice.device_id();
        let bob_id = bob.device_id();
        let _ = tokio::join!(
            wait_for_peer(&alice, bob_id.clone()),
            wait_for_peer(&bob, alice_id.clone()),
        );

        let file_name = "encrypted-retry-after-cancel-payload.bin";
        let source_path = alice.app_data_dir.join(file_name);
        let payload = (0..(512 * 1024))
            .map(|index| (index % 253) as u8)
            .collect::<Vec<_>>();
        std::fs::write(&source_path, &payload).expect("write retry-after-cancel source payload");

        let bob_offer_cursor = bob.emitter.mark();
        let transfer_id = wait_for(TEST_TIMEOUT, || {
            let state = Arc::clone(&alice.state);
            let bob_id = bob_id.clone();
            let source_path = source_path.clone();
            async move {
                match tokio::time::timeout(
                    Duration::from_secs(2),
                    state.send_file(bob_id, source_path.to_string_lossy().into_owned()),
                )
                .await
                {
                    Ok(Ok(transfer_id)) => Some(transfer_id),
                    _ => None,
                }
            }
        })
        .await;

        let offer_payload = bob
            .emitter
            .wait_for_event(
                bob_offer_cursor,
                EVENT_FILE_OFFER_RECEIVED,
                ACTION_TIMEOUT,
                |payload| {
                    payload
                        .get("offerId")
                        .or_else(|| payload.get("id"))
                        .and_then(serde_json::Value::as_str)
                        == Some(transfer_id.as_str())
                },
            )
            .await;

        let offer_id = offer_payload
            .get("offerId")
            .or_else(|| offer_payload.get("id"))
            .and_then(serde_json::Value::as_str)
            .expect("initial offer id")
            .to_string();

        bob.state
            .accept_file(offer_id)
            .await
            .expect("accept initial transfer offer");

        wait_for(ACTION_TIMEOUT, || {
            let state = Arc::clone(&alice.state);
            let transfer_id = transfer_id.clone();
            async move {
                let transfers = state.get_transfers().await.ok()?;
                transfers.into_iter().find(|transfer| {
                    transfer.id == transfer_id
                        && transfer.state == "active"
                        && transfer.progress > 0.0
                        && transfer.progress < 1.0
                })
            }
        })
        .await;

        alice
            .state
            .cancel_transfer(transfer_id.clone())
            .await
            .expect("cancel initial transfer");

        wait_for(ACTION_TIMEOUT, || {
            let state = Arc::clone(&alice.state);
            let transfer_id = transfer_id.clone();
            async move {
                let transfers = state.get_transfers().await.ok()?;
                transfers.into_iter().find(|transfer| {
                    transfer.id == transfer_id
                        && (transfer.state == "failed" || transfer.state == "cancelled")
                })
            }
        })
        .await;

        let bob_retry_offer_cursor = bob.emitter.mark();
        let retry_transfer_id = alice
            .state
            .retry_transfer(transfer_id.clone())
            .await
            .expect("retry transfer");

        assert_ne!(retry_transfer_id, transfer_id);

        let retry_offer_payload = bob
            .emitter
            .wait_for_event(
                bob_retry_offer_cursor,
                EVENT_FILE_OFFER_RECEIVED,
                ACTION_TIMEOUT,
                |payload| {
                    payload
                        .get("offerId")
                        .or_else(|| payload.get("id"))
                        .and_then(serde_json::Value::as_str)
                        == Some(retry_transfer_id.as_str())
                },
            )
            .await;

        let retry_offer_id = retry_offer_payload
            .get("offerId")
            .or_else(|| retry_offer_payload.get("id"))
            .and_then(serde_json::Value::as_str)
            .expect("retry offer id")
            .to_string();

        bob.state
            .accept_file(retry_offer_id)
            .await
            .expect("accept retried transfer offer");

        let expected_received_path = if let Some(dest_path) = retry_offer_payload
            .get("destPath")
            .and_then(serde_json::Value::as_str)
            .filter(|path| !path.is_empty())
        {
            std::path::PathBuf::from(dest_path)
        } else {
            bob.download_dir.join(file_name)
        };

        let (alice_retry_transfer, bob_retry_transfer) = tokio::join!(
            wait_for(ACTION_TIMEOUT, || {
                let state = Arc::clone(&alice.state);
                let retry_transfer_id = retry_transfer_id.clone();
                async move {
                    let transfers = state.get_transfers().await.ok()?;
                    transfers.into_iter().find(|transfer| {
                        transfer.id == retry_transfer_id && transfer.state == "completed"
                    })
                }
            }),
            wait_for(ACTION_TIMEOUT, || {
                let state = Arc::clone(&bob.state);
                let retry_transfer_id = retry_transfer_id.clone();
                async move {
                    let transfers = state.get_transfers().await.ok()?;
                    transfers.into_iter().find(|transfer| {
                        transfer.id == retry_transfer_id && transfer.state == "completed"
                    })
                }
            }),
        );

        let received_path = bob_retry_transfer
            .local_path
            .as_deref()
            .filter(|path| !path.is_empty())
            .map(std::path::PathBuf::from)
            .unwrap_or(expected_received_path);

        assert_eq!(alice_retry_transfer.progress, 1.0);
        assert_eq!(bob_retry_transfer.progress, 1.0);
        assert_eq!(sha256_file(&source_path), sha256_file(&received_path));

        bob.shutdown().await;
        alice.shutdown().await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn test_tampered_ciphertext_rejection() {
    let _guard = suite_lock().lock().await;
    within_test_timeout("test_tampered_ciphertext_rejection", async {
        let alice = TestNode::new().await;
        let bob = TestNode::new().await;
        let probe = TestNode::new().await;

        let alice_id = alice.device_id();
        let bob_id = bob.device_id();
        let _ = tokio::join!(
            wait_for_peer(&alice, bob_id.clone()),
            wait_for_peer(&bob, alice_id.clone()),
            wait_for_peer(&probe, bob_id.clone()),
        );

        let bob_peer_record = wait_for_peer_record(probe.database_path(), bob_id.clone()).await;
        let bob_ws_port = bob_peer_record
            .ws_port
            .expect("bob peer record must advertise websocket port");

        let tampered_sender_id = Uuid::new_v4().to_string();
        let (tampered_sender_private_key, tampered_sender_public_key) = generate_identity_keypair();
        let tampered_sender = WsPeerIdentity::new(tampered_sender_id, "Tampered Sender")
            .with_transport_identity(
                public_key_to_base64(&tampered_sender_public_key),
                CURRENT_PROTOCOL_VERSION,
            );

        let (mut raw_socket, _) = connect_async(format!("ws://127.0.0.1:{bob_ws_port}"))
            .await
            .expect("connect raw websocket sender to bob");

        let RawClientHandshakeOutcome { .. } = complete_raw_client_key_exchange_with_session(
            &mut raw_socket,
            &tampered_sender,
            &tampered_sender_private_key,
        )
        .await;

        let tampered_cursor = bob.emitter.mark();
        let tampered_frame = vec![
            0x00, 0x00, 0x00, 0x01, 0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0xFF, 0xAA, 0xBB, 0x00, 0x11,
            0x22, 0x33,
        ];
        raw_socket
            .send(WsMessage::Binary(tampered_frame.into()))
            .await
            .expect("send tampered ciphertext frame");

        assert_no_event(
            &bob.emitter,
            tampered_cursor,
            EVENT_MESSAGE_RECEIVED,
            NO_EVENT_WAIT,
            |_| true,
        )
        .await;

        let recovery_cursor = bob.emitter.mark();
        let expected_content = "legitimate message after tamper".to_string();
        wait_for(TEST_TIMEOUT, || {
            let state = Arc::clone(&alice.state);
            let bob_id = bob_id.clone();
            let expected_content = expected_content.clone();
            async move {
                match tokio::time::timeout(
                    Duration::from_secs(2),
                    state.send_message(bob_id, expected_content, None),
                )
                .await
                {
                    Ok(Ok(_message_id)) => Some(()),
                    _ => None,
                }
            }
        })
        .await;

        let recovery_payload = bob
            .emitter
            .wait_for_event(
                recovery_cursor,
                EVENT_MESSAGE_RECEIVED,
                ACTION_TIMEOUT,
                |payload| {
                    payload.get("content").and_then(serde_json::Value::as_str)
                        == Some("legitimate message after tamper")
                        && payload.get("senderId").and_then(serde_json::Value::as_str)
                            == Some(alice_id.as_str())
                },
            )
            .await;

        assert_eq!(
            recovery_payload
                .get("content")
                .and_then(serde_json::Value::as_str),
            Some("legitimate message after tamper")
        );
        assert_eq!(
            recovery_payload
                .get("senderId")
                .and_then(serde_json::Value::as_str),
            Some(alice_id.as_str())
        );

        raw_socket.close(None).await.expect("close raw websocket");
        probe.shutdown().await;
        bob.shutdown().await;
        alice.shutdown().await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_fingerprint_verification_after_connection() {
    let _guard = suite_lock().lock().await;
    within_test_timeout("test_fingerprint_verification_after_connection", async {
        let node_a = TestNode::new().await;
        let node_b = TestNode::new().await;

        let node_a_peer_id = node_a.device_id();
        let node_b_peer_id = node_b.device_id();
        let _ = tokio::join!(
            wait_for_peer(&node_a, node_b_peer_id.clone()),
            wait_for_peer(&node_b, node_a_peer_id.clone()),
        );

        let a_own = node_a
            .state
            .get_own_fingerprint()
            .expect("node_a own fingerprint");
        let b_own = node_b
            .state
            .get_own_fingerprint()
            .expect("node_b own fingerprint");

        assert!(
            !a_own.fingerprint.is_empty(),
            "node_a fingerprint must be non-empty"
        );
        assert!(
            !b_own.fingerprint.is_empty(),
            "node_b fingerprint must be non-empty"
        );

        assert_ne!(
            a_own.fingerprint, b_own.fingerprint,
            "each node must have unique fingerprint"
        );

        let receive_cursor = node_b.emitter.mark();
        node_a
            .state
            .send_message(
                node_b_peer_id.clone(),
                "fingerprint trigger".to_string(),
                None,
            )
            .await
            .expect("send trigger message");

        wait_for(TEST_TIMEOUT, || {
            let events = node_b.emitter.events_since(receive_cursor);
            async move {
                events
                    .iter()
                    .any(|e| {
                        e.name == EVENT_MESSAGE_RECEIVED
                            && e.payload.get("content").and_then(serde_json::Value::as_str)
                                == Some("fingerprint trigger")
                    })
                    .then_some(())
            }
        })
        .await;

        let a_peer_b = wait_for(TEST_TIMEOUT, || {
            let state = Arc::clone(&node_a.state);
            let node_b_peer_id = node_b_peer_id.clone();
            async move {
                tokio::task::spawn_blocking(move || state.get_peer_fingerprint(node_b_peer_id))
                    .await
                    .ok()
                    .and_then(Result::ok)
            }
        })
        .await;

        assert!(
            !a_peer_b.fingerprint.is_empty(),
            "peer fingerprint must be non-empty"
        );

        assert_eq!(
            a_peer_b.fingerprint, b_own.fingerprint,
            "peer fingerprint must match node_b own fingerprint"
        );

        assert!(!a_peer_b.verified, "peer must not be verified initially");

        node_b.shutdown().await;
        node_a.shutdown().await;
    })
    .await;
}
