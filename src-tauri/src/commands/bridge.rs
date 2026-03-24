use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Mutex;

use jasmine_core::{
    protocol::{CallType, FolderManifestData},
    DeviceId, ProtocolMessage, StorageEngine,
};
use jasmine_messaging::ChatServiceEvent;
use jasmine_storage::SqliteStorage;
use serde::Serialize;
use tracing::warn;

use super::messaging::{chat_message_payload, message_status_label};
use super::types::{
    FrontendEmitter, MentionReceivedPayload, MessageDeletedPayload, MessageEditedPayload,
};

const EVENT_MESSAGE_RECEIVED: &str = "message-received";
const EVENT_GROUP_MESSAGE_RECEIVED: &str = "group-message-received";
const EVENT_MESSAGE_EDITED: &str = "message-edited";
const EVENT_MESSAGE_DELETED: &str = "message-deleted";
const EVENT_MENTION_RECEIVED: &str = "mention-received";
const EVENT_GROUP_CREATED: &str = "group-created";
const EVENT_GROUP_UPDATED: &str = "group-updated";
const EVENT_GROUP_MEMBER_ADDED: &str = "group-member-added";
const EVENT_GROUP_MEMBER_REMOVED: &str = "group-member-removed";
const EVENT_CALL_OFFER: &str = "call-offer";
const EVENT_CALL_ANSWER: &str = "call-answer";
const EVENT_ICE_CANDIDATE: &str = "ice-candidate";
const EVENT_CALL_HANGUP: &str = "call-hangup";
const EVENT_CALL_REJECT: &str = "call-reject";
const EVENT_CALL_JOIN: &str = "call-join";
const EVENT_CALL_LEAVE: &str = "call-leave";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CallOfferPayload {
    pub(crate) peer_id: String,
    pub(crate) call_id: String,
    pub(crate) sdp: String,
    pub(crate) caller_id: String,
    pub(crate) call_type: CallType,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CallAnswerPayload {
    pub(crate) peer_id: String,
    pub(crate) call_id: String,
    pub(crate) sdp: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct IceCandidatePayload {
    pub(crate) peer_id: String,
    pub(crate) call_id: String,
    pub(crate) candidate: String,
    pub(crate) sdp_mid: Option<String>,
    pub(crate) sdp_mline_index: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CallHangupPayload {
    pub(crate) peer_id: String,
    pub(crate) call_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CallRejectPayload {
    pub(crate) peer_id: String,
    pub(crate) call_id: String,
    pub(crate) reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CallJoinPayload {
    pub(crate) peer_id: String,
    pub(crate) call_id: String,
    pub(crate) group_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CallLeavePayload {
    pub(crate) peer_id: String,
    pub(crate) call_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GroupEventPayload {
    group_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GroupMembershipEventPayload {
    group_id: String,
    member_ids: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GroupMessageEventPayload {
    group_id: String,
    id: String,
    sender_id: String,
    sender_name: Option<String>,
    content: String,
    timestamp: i64,
    encrypted: bool,
    is_own: bool,
    status: String,
    is_deleted: bool,
    edited_at: Option<u64>,
    reply_to_id: Option<String>,
    reply_to_preview: Option<String>,
}

pub(crate) async fn emit_chat_service_event(
    event: ChatServiceEvent,
    storage: &SqliteStorage,
    emitter: &dyn FrontendEmitter,
    local_device_id: &str,
) {
    match event {
        ChatServiceEvent::MessageReceived { peer_id, message } => {
            let chat_id = message.chat_id.0.to_string();
            if chat_id != peer_id && chat_id != local_device_id {
                let payload = GroupMessageEventPayload {
                    group_id: chat_id,
                    id: message.id.to_string(),
                    sender_id: peer_id,
                    sender_name: None,
                    content: message.content,
                    timestamp: message.timestamp_ms,
                    encrypted: true,
                    is_own: false,
                    status: message_status_label(&message.status).to_string(),
                    is_deleted: message.is_deleted,
                    edited_at: message.edited_at,
                    reply_to_id: message.reply_to_id,
                    reply_to_preview: message.reply_to_preview,
                };
                let _ = emit_payload(emitter, EVENT_GROUP_MESSAGE_RECEIVED, &payload);
            } else {
                let payload = chat_message_payload(local_device_id, &peer_id, message, Some(true));
                let _ = emit_payload(emitter, EVENT_MESSAGE_RECEIVED, &payload);
            }
        }
        ChatServiceEvent::GroupCreated { group_id } => {
            let payload = GroupEventPayload {
                group_id: group_id.0.to_string(),
            };
            let _ = emit_payload(emitter, EVENT_GROUP_CREATED, &payload);
        }
        ChatServiceEvent::GroupUpdated { group_id } => {
            let payload = GroupEventPayload {
                group_id: group_id.0.to_string(),
            };
            let _ = emit_payload(emitter, EVENT_GROUP_UPDATED, &payload);
        }
        ChatServiceEvent::GroupMembersAdded {
            group_id,
            member_ids,
        } => {
            let payload = GroupMembershipEventPayload {
                group_id: group_id.0.to_string(),
                member_ids,
            };
            let _ = emit_payload(emitter, EVENT_GROUP_MEMBER_ADDED, &payload);
        }
        ChatServiceEvent::GroupMembersRemoved {
            group_id,
            member_ids,
        } => {
            let payload = GroupMembershipEventPayload {
                group_id: group_id.0.to_string(),
                member_ids,
            };
            let _ = emit_payload(emitter, EVENT_GROUP_MEMBER_REMOVED, &payload);
        }
        ChatServiceEvent::MessageEdited {
            message_id,
            new_content,
            edit_version,
        } => {
            let edited_at = storage
                .get_message(&message_id.to_string())
                .await
                .ok()
                .flatten()
                .and_then(|message| message.edited_at)
                .unwrap_or_default();
            let payload = MessageEditedPayload {
                message_id: message_id.to_string(),
                new_content,
                edit_version,
                edited_at,
            };
            let _ = emit_payload(emitter, EVENT_MESSAGE_EDITED, &payload);
        }
        ChatServiceEvent::MessageDeleted { message_id } => {
            let payload = MessageDeletedPayload {
                message_id: message_id.to_string(),
            };
            let _ = emit_payload(emitter, EVENT_MESSAGE_DELETED, &payload);
        }
        ChatServiceEvent::MentionReceived {
            message_id,
            mentioned_user_id,
            sender_name,
        } => {
            let payload = MentionReceivedPayload {
                message_id: message_id.to_string(),
                mentioned_user_id,
                sender_name,
            };
            let _ = emit_payload(emitter, EVENT_MENTION_RECEIVED, &payload);
        }
        ChatServiceEvent::MessageStatusUpdated { .. } => {}
    }
}

pub(crate) fn emit_payload<S: Serialize>(
    emitter: &dyn FrontendEmitter,
    event: &str,
    payload: &S,
) -> Result<(), String> {
    let value = serde_json::to_value(payload).map_err(|error| error.to_string())?;
    emitter.emit_json(event, value)
}

#[derive(Default)]
pub(crate) struct CallBridgeState {
    known_call_ids: Mutex<HashSet<String>>,
    group_calls: Mutex<HashMap<String, String>>,
}

impl CallBridgeState {
    pub(crate) fn remember_call(&self, call_id: impl Into<String>) -> bool {
        self.known_call_ids
            .lock()
            .expect("lock known call ids")
            .insert(call_id.into())
    }

    pub(crate) fn remember_group_call(
        &self,
        call_id: impl Into<String>,
        group_id: impl Into<String>,
    ) -> bool {
        let call_id = call_id.into();
        let inserted = self.remember_call(call_id.clone());
        self.group_calls
            .lock()
            .expect("lock group call ids")
            .insert(call_id, group_id.into());
        inserted
    }

    pub(crate) fn is_known_call(&self, call_id: &str) -> bool {
        self.known_call_ids
            .lock()
            .expect("lock known call ids")
            .contains(call_id)
    }

    pub(crate) fn finish_call(&self, call_id: &str) {
        self.known_call_ids
            .lock()
            .expect("lock known call ids")
            .remove(call_id);
        self.group_calls
            .lock()
            .expect("lock group call ids")
            .remove(call_id);
    }

    pub(crate) fn is_group_call(&self, call_id: &str) -> bool {
        self.group_calls
            .lock()
            .expect("lock group call ids")
            .contains_key(call_id)
    }

    pub(crate) fn group_id_for_call(&self, call_id: &str) -> Option<String> {
        self.group_calls
            .lock()
            .expect("lock group call ids")
            .get(call_id)
            .cloned()
    }
}

pub(crate) fn bridge_call_signaling_message(
    peer_id: &str,
    message: ProtocolMessage,
    emitter: &dyn FrontendEmitter,
    call_bridge: &CallBridgeState,
) -> Result<bool, String> {
    match message {
        ProtocolMessage::CallOffer {
            call_id,
            sdp,
            caller_id,
            call_type,
        } => {
            call_bridge.remember_call(call_id.clone());
            emit_payload(
                emitter,
                EVENT_CALL_OFFER,
                &CallOfferPayload {
                    peer_id: peer_id.to_string(),
                    call_id,
                    sdp,
                    caller_id,
                    call_type,
                },
            )?;
            Ok(true)
        }
        ProtocolMessage::CallAnswer { call_id, sdp } => {
            if !call_bridge.is_known_call(&call_id) {
                warn!(peer_id = %peer_id, call_id = %call_id, "dropping call answer for unknown call");
                return Ok(false);
            }

            emit_payload(
                emitter,
                EVENT_CALL_ANSWER,
                &CallAnswerPayload {
                    peer_id: peer_id.to_string(),
                    call_id,
                    sdp,
                },
            )?;
            Ok(true)
        }
        ProtocolMessage::IceCandidate {
            call_id,
            candidate,
            sdp_mid,
            sdp_mline_index,
        } => {
            if !call_bridge.is_known_call(&call_id) {
                warn!(peer_id = %peer_id, call_id = %call_id, "dropping ICE candidate for unknown call");
                return Ok(false);
            }

            emit_payload(
                emitter,
                EVENT_ICE_CANDIDATE,
                &IceCandidatePayload {
                    peer_id: peer_id.to_string(),
                    call_id,
                    candidate,
                    sdp_mid,
                    sdp_mline_index,
                },
            )?;
            Ok(true)
        }
        ProtocolMessage::CallHangup { call_id } => {
            if !call_bridge.is_known_call(&call_id) {
                warn!(peer_id = %peer_id, call_id = %call_id, "dropping call hangup for unknown call");
                return Ok(false);
            }

            if !call_bridge.is_group_call(&call_id) {
                call_bridge.finish_call(&call_id);
            }
            emit_payload(
                emitter,
                EVENT_CALL_HANGUP,
                &CallHangupPayload {
                    peer_id: peer_id.to_string(),
                    call_id,
                },
            )?;
            Ok(true)
        }
        ProtocolMessage::CallReject { call_id, reason } => {
            if !call_bridge.is_known_call(&call_id) {
                warn!(peer_id = %peer_id, call_id = %call_id, "dropping call reject for unknown call");
                return Ok(false);
            }

            if !call_bridge.is_group_call(&call_id) {
                call_bridge.finish_call(&call_id);
            }
            emit_payload(
                emitter,
                EVENT_CALL_REJECT,
                &CallRejectPayload {
                    peer_id: peer_id.to_string(),
                    call_id,
                    reason,
                },
            )?;
            Ok(true)
        }
        ProtocolMessage::CallJoin { call_id, group_id } => {
            if !call_bridge.is_known_call(&call_id) {
                warn!(peer_id = %peer_id, call_id = %call_id, group_id = %group_id, "dropping call join for unknown call");
                return Ok(false);
            }

            call_bridge.remember_group_call(call_id.clone(), group_id.clone());
            emit_payload(
                emitter,
                EVENT_CALL_JOIN,
                &CallJoinPayload {
                    peer_id: peer_id.to_string(),
                    call_id,
                    group_id,
                },
            )?;
            Ok(true)
        }
        ProtocolMessage::CallLeave { call_id } => {
            if !call_bridge.is_known_call(&call_id) {
                warn!(peer_id = %peer_id, call_id = %call_id, "dropping call leave for unknown call");
                return Ok(false);
            }

            emit_payload(
                emitter,
                EVENT_CALL_LEAVE,
                &CallLeavePayload {
                    peer_id: peer_id.to_string(),
                    call_id,
                },
            )?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

#[derive(Default)]
pub(crate) struct FolderBridgeState {
    pending_offers: Mutex<HashMap<String, PendingFolderRoute>>,
    active_receives: Mutex<HashMap<String, ActiveFolderRoute>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingFolderRoute {
    sender_id: DeviceId,
    expected_files: VecDeque<FolderFileRoute>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ActiveFolderRoute {
    sender_id: DeviceId,
    expected_files: VecDeque<FolderFileRoute>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FolderFileRoute {
    filename: String,
    size: u64,
    sha256: String,
}

impl FolderFileRoute {
    fn from_manifest_path(path: &str, size: u64, sha256: &str) -> Option<Self> {
        let filename = path
            .rsplit('/')
            .find(|segment| !segment.is_empty())?
            .to_string();
        Some(Self {
            filename,
            size,
            sha256: sha256.to_string(),
        })
    }

    fn from_message(message: &ProtocolMessage) -> Option<Self> {
        let ProtocolMessage::FileOffer {
            filename,
            size,
            sha256,
            ..
        } = message
        else {
            return None;
        };

        Some(Self {
            filename: filename.clone(),
            size: *size,
            sha256: sha256.clone(),
        })
    }

    fn matches_offer(&self, offer: &Self) -> bool {
        self.filename == offer.filename
            && self.size == offer.size
            && self.sha256.eq_ignore_ascii_case(&offer.sha256)
    }
}

fn folder_file_routes(manifest: &FolderManifestData) -> VecDeque<FolderFileRoute> {
    manifest
        .files
        .iter()
        .filter_map(|entry| {
            FolderFileRoute::from_manifest_path(&entry.relative_path, entry.size, &entry.sha256)
        })
        .collect()
}

impl FolderBridgeState {
    pub(crate) fn remember_pending_offer(
        &self,
        offer: &jasmine_transfer::FolderOfferNotification,
        manifest: &FolderManifestData,
    ) {
        self.pending_offers
            .lock()
            .expect("lock pending folder offers")
            .insert(
                offer.folder_transfer_id.clone(),
                PendingFolderRoute {
                    sender_id: offer.sender_id.clone(),
                    expected_files: folder_file_routes(manifest),
                },
            );
    }

    pub(crate) fn take_pending_offer(&self, folder_id: &str) -> Option<PendingFolderRoute> {
        self.pending_offers
            .lock()
            .expect("lock pending folder offers")
            .remove(folder_id)
    }

    pub(crate) fn has_pending_offer(&self, folder_id: &str) -> bool {
        self.pending_offers
            .lock()
            .expect("lock pending folder offers")
            .contains_key(folder_id)
    }

    pub(crate) fn activate_receive(&self, folder_id: String, pending_offer: PendingFolderRoute) {
        self.active_receives
            .lock()
            .expect("lock active folder receives")
            .insert(
                folder_id,
                ActiveFolderRoute {
                    sender_id: pending_offer.sender_id,
                    expected_files: pending_offer.expected_files,
                },
            );
    }

    pub(crate) fn deactivate_receive(&self, folder_id: &str) {
        self.active_receives
            .lock()
            .expect("lock active folder receives")
            .remove(folder_id);
    }

    pub(crate) fn routes_file_offer(
        &self,
        sender_id: &DeviceId,
        message: &ProtocolMessage,
    ) -> bool {
        let Some(offer) = FolderFileRoute::from_message(message) else {
            return false;
        };

        let mut active_receives = self
            .active_receives
            .lock()
            .expect("lock active folder receives");
        let Some(active_receive) = active_receives
            .values_mut()
            .find(|active_receive| active_receive.sender_id == *sender_id)
        else {
            return false;
        };

        let Some(expected_offer) = active_receive.expected_files.front() else {
            return false;
        };
        if !expected_offer.matches_offer(&offer) {
            return false;
        }

        active_receive.expected_files.pop_front();
        true
    }
}
