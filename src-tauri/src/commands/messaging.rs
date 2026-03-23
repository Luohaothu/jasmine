use std::sync::Arc;

use jasmine_core::{Message, MessageStatus};
use tauri::State;

use super::types::{
    AppState, ChatMessagePayload, GroupInfoResponse, GroupMemberInfo, StoredGroupInfo,
};

#[allow(non_snake_case)]
#[tauri::command]
pub async fn send_message(
    state: State<'_, Arc<AppState>>,
    peerId: String,
    content: String,
    replyToId: Option<String>,
) -> Result<String, String> {
    send_message_impl(state.inner().as_ref(), peerId, content, replyToId).await
}

#[tauri::command]
pub async fn get_messages(
    state: State<'_, Arc<AppState>>,
    chat_id: String,
    limit: u32,
    offset: u32,
) -> Result<Vec<ChatMessagePayload>, String> {
    get_messages_impl(state.inner().as_ref(), chat_id, limit, offset).await
}

#[tauri::command]
pub async fn create_group(
    state: State<'_, Arc<AppState>>,
    name: String,
    members: Vec<String>,
) -> Result<GroupInfoResponse, String> {
    create_group_impl(state.inner().as_ref(), name, members).await
}

#[allow(non_snake_case)]
#[tauri::command]
pub async fn add_group_members(
    state: State<'_, Arc<AppState>>,
    groupId: String,
    memberIds: Vec<String>,
) -> Result<(), String> {
    add_group_members_impl(state.inner().as_ref(), groupId, memberIds).await
}

#[allow(non_snake_case)]
#[tauri::command]
pub async fn remove_group_members(
    state: State<'_, Arc<AppState>>,
    groupId: String,
    memberIds: Vec<String>,
) -> Result<(), String> {
    remove_group_members_impl(state.inner().as_ref(), groupId, memberIds).await
}

#[allow(non_snake_case)]
#[tauri::command]
pub async fn get_group_info(
    state: State<'_, Arc<AppState>>,
    groupId: String,
) -> Result<GroupInfoResponse, String> {
    get_group_info_impl(state.inner().as_ref(), groupId).await
}

#[tauri::command]
pub async fn list_groups(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<GroupInfoResponse>, String> {
    list_groups_impl(state.inner().as_ref()).await
}

#[allow(non_snake_case)]
#[tauri::command]
pub async fn leave_group(state: State<'_, Arc<AppState>>, groupId: String) -> Result<(), String> {
    leave_group_impl(state.inner().as_ref(), groupId).await
}

#[allow(non_snake_case)]
#[tauri::command]
pub async fn send_group_message(
    state: State<'_, Arc<AppState>>,
    groupId: String,
    content: String,
    replyToId: Option<String>,
) -> Result<String, String> {
    send_group_message_impl(state.inner().as_ref(), groupId, content, replyToId).await
}

#[allow(non_snake_case)]
#[tauri::command]
pub async fn edit_message(
    state: State<'_, Arc<AppState>>,
    messageId: String,
    newContent: String,
) -> Result<(), String> {
    edit_message_impl(state.inner().as_ref(), messageId, newContent).await
}

#[allow(non_snake_case)]
#[tauri::command]
pub async fn delete_message(
    state: State<'_, Arc<AppState>>,
    messageId: String,
) -> Result<(), String> {
    delete_message_impl(state.inner().as_ref(), messageId).await
}

pub(crate) async fn send_message_impl(
    state: &AppState,
    peer_id: String,
    content: String,
    reply_to_id: Option<String>,
) -> Result<String, String> {
    let message = state
        .messaging
        .send_message(&peer_id, &content, reply_to_id.as_deref())
        .await?;
    Ok(message.id.to_string())
}

pub(crate) async fn get_messages_impl(
    state: &AppState,
    chat_id: String,
    limit: u32,
    offset: u32,
) -> Result<Vec<ChatMessagePayload>, String> {
    let local_device_id = state.local_device_id();
    let messages = state
        .messaging
        .get_messages(&chat_id, limit, offset)
        .await?;
    Ok(messages
        .into_iter()
        .map(|message| chat_message_payload(&local_device_id, &chat_id, message, None))
        .collect())
}

pub(crate) async fn create_group_impl(
    state: &AppState,
    name: String,
    members: Vec<String>,
) -> Result<GroupInfoResponse, String> {
    let group = state.messaging.create_group(&name, &members).await?;
    group_info_response(state, group)
}

pub(crate) async fn add_group_members_impl(
    state: &AppState,
    group_id: String,
    member_ids: Vec<String>,
) -> Result<(), String> {
    state
        .messaging
        .add_group_members(&group_id, &member_ids)
        .await
        .map(|_| ())
}

pub(crate) async fn remove_group_members_impl(
    state: &AppState,
    group_id: String,
    member_ids: Vec<String>,
) -> Result<(), String> {
    state
        .messaging
        .remove_group_members(&group_id, &member_ids)
        .await
        .map(|_| ())
}

pub(crate) async fn get_group_info_impl(
    state: &AppState,
    group_id: String,
) -> Result<GroupInfoResponse, String> {
    let group = state.messaging.get_group_info(&group_id).await?;
    group_info_response(state, group)
}

pub(crate) async fn list_groups_impl(state: &AppState) -> Result<Vec<GroupInfoResponse>, String> {
    let groups = state.messaging.list_groups().await?;
    groups
        .into_iter()
        .map(|group| group_info_response(state, group))
        .collect()
}

pub(crate) async fn leave_group_impl(state: &AppState, group_id: String) -> Result<(), String> {
    state.messaging.leave_group(&group_id).await
}

pub(crate) async fn send_group_message_impl(
    state: &AppState,
    group_id: String,
    content: String,
    reply_to_id: Option<String>,
) -> Result<String, String> {
    let message = state
        .messaging
        .send_group_message(&group_id, &content, reply_to_id.as_deref())
        .await?;
    Ok(message.id.to_string())
}

pub(crate) async fn edit_message_impl(
    state: &AppState,
    message_id: String,
    new_content: String,
) -> Result<(), String> {
    state
        .messaging
        .edit_message(&message_id, &new_content, &state.local_device_id())
        .await
}

pub(crate) async fn delete_message_impl(
    state: &AppState,
    message_id: String,
) -> Result<(), String> {
    state
        .messaging
        .delete_message(&message_id, &state.local_device_id())
        .await
}

pub(crate) fn chat_message_payload(
    local_device_id: &str,
    peer_id: &str,
    message: Message,
    encrypted: Option<bool>,
) -> ChatMessagePayload {
    let Message {
        id,
        chat_id,
        sender_id,
        content,
        timestamp_ms,
        status,
        edit_version,
        edited_at,
        is_deleted,
        deleted_at,
        reply_to_id,
        reply_to_preview,
    } = message;
    let sender_uuid = sender_id.0.to_string();
    let chat_id = chat_id.0.to_string();
    let sender_is_local = sender_uuid == local_device_id;

    ChatMessagePayload {
        id: id.to_string(),
        sender_id: if sender_is_local {
            "local".to_string()
        } else {
            sender_uuid.clone()
        },
        receiver_id: if sender_is_local {
            chat_id.clone()
        } else if chat_id == peer_id || chat_id == sender_uuid {
            "local".to_string()
        } else {
            chat_id.clone()
        },
        content,
        timestamp: timestamp_ms,
        encrypted,
        status: message_status_label(&status).to_string(),
        edit_version,
        edited_at,
        is_deleted,
        deleted_at,
        reply_to_id,
        reply_to_preview,
    }
}

pub(crate) fn message_status_label(status: &MessageStatus) -> &'static str {
    match status {
        MessageStatus::Sent => "sent",
        MessageStatus::Delivered => "delivered",
        MessageStatus::Failed => "failed",
    }
}

fn group_info_response(
    state: &AppState,
    group: StoredGroupInfo,
) -> Result<GroupInfoResponse, String> {
    let identity = state.identity_service.get_identity()?;
    let local_device_id = state.local_device_id();
    let peers = state.discovery.peers();

    let members = group
        .member_ids
        .into_iter()
        .map(|member_id| group_member_info(&identity, &local_device_id, &peers, member_id))
        .collect();

    Ok(GroupInfoResponse {
        id: group.id,
        name: group.name,
        members,
        created_at: group.created_at,
    })
}

fn group_member_info(
    identity: &jasmine_core::DeviceIdentity,
    local_device_id: &str,
    peers: &[jasmine_core::PeerInfo],
    member_id: String,
) -> GroupMemberInfo {
    if member_id == local_device_id {
        return GroupMemberInfo {
            device_id: member_id,
            name: identity.display_name.clone(),
            status: "online".to_string(),
        };
    }

    if let Some(peer) = peers
        .iter()
        .find(|peer| peer.device_id.0.to_string() == member_id)
    {
        return GroupMemberInfo {
            device_id: member_id,
            name: peer.display_name.clone(),
            status: "online".to_string(),
        };
    }

    GroupMemberInfo {
        device_id: member_id.clone(),
        name: member_id,
        status: "offline".to_string(),
    }
}
