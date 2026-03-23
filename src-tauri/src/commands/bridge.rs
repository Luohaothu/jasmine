use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use jasmine_core::{protocol::FolderManifestData, DeviceId, ProtocolMessage, StorageEngine};
use jasmine_messaging::ChatServiceEvent;
use jasmine_storage::SqliteStorage;
use serde::Serialize;

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
