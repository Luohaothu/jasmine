use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use jasmine_core::{
    protocol::CallType, AppSettings, ChatId, DeviceId, DeviceIdentity, Message, MessageStatus,
    OgMetadata, PeerInfo, TransferRecord, TransferStatus, UserId,
};
use jasmine_crypto::{fingerprint, public_key_from_base64, public_key_to_base64, PublicKey};
use jasmine_storage::CachedOgMetadata;
use jasmine_transfer::{ManagedTransfer, TransferDirection, TransferProgress};
use tempfile::tempdir;
use uuid::Uuid;

use super::{
    accept_file_impl, accept_folder_transfer_impl, add_group_members_impl,
    cancel_folder_transfer_impl, cancel_transfer_impl, check_call_support_impl, create_group_impl,
    current_webview_backend, delete_message_impl, edit_message_impl, fetch_og_metadata_impl,
    fetch_og_metadata_with_cache_policy, get_group_info_impl, get_identity_impl, get_messages_impl,
    get_own_fingerprint_impl, get_peer_fingerprint_impl, get_peers_impl, get_reply_count_impl,
    get_reply_counts_impl, get_settings_impl, get_transfers_impl, get_webrtc_platform_info_impl,
    leave_group_impl, list_groups_impl, reject_file_impl, reject_folder_transfer_impl,
    remove_group_members_impl, resume_transfer_impl, retry_transfer_impl, send_call_answer_impl,
    send_call_hangup_impl, send_call_join_impl, send_call_leave_impl, send_call_offer_impl,
    send_call_reject_impl, send_file_impl, send_folder_impl, send_group_message_impl,
    send_ice_candidate_impl, send_message_impl, setup_app_state_with_factory, start_discovery_impl,
    stop_discovery_impl, toggle_peer_verified_impl, transfer_payload_from_managed,
    transfer_payload_from_record, update_avatar_impl, update_display_name_impl,
    update_settings_impl, AppSetupFactory, AppState, CallBridgeState, CallSupportInfo,
    ChatMessagePayload, DiscoveryServiceHandle, FrontendEmitter, GroupInfoResponse,
    GroupMemberInfo, IdentityServiceHandle, MessagingServiceHandle, OgMetadataServiceHandle,
    OwnFingerprintInfo, PeerFingerprintInfo, PeerPayload, SettingsServiceHandle, SetupContext,
    StoredGroupInfo, TransferPayload, TransferServiceHandle, WebRTCPlatformInfo,
};

fn device_id(seed: u128) -> DeviceId {
    DeviceId(Uuid::from_u128(seed))
}

fn user_id(seed: u128) -> UserId {
    UserId(Uuid::from_u128(seed))
}

fn chat_id(seed: u128) -> ChatId {
    ChatId(Uuid::from_u128(seed))
}

fn peer(seed: u128, display_name: &str) -> PeerInfo {
    PeerInfo {
        device_id: device_id(seed),
        user_id: None,
        display_name: display_name.to_string(),
        ws_port: Some(9735),
        addresses: vec!["127.0.0.1".to_string()],
    }
}

fn message(
    id: u128,
    chat_seed: u128,
    sender_seed: u128,
    content: &str,
    status: MessageStatus,
) -> Message {
    Message {
        id: Uuid::from_u128(id),
        chat_id: chat_id(chat_seed),
        sender_id: user_id(sender_seed),
        content: content.to_string(),
        timestamp_ms: 1_234,
        status,
        edit_version: 0,
        edited_at: None,
        is_deleted: false,
        deleted_at: None,
        reply_to_id: None,
        reply_to_preview: None,
    }
}

fn identity() -> DeviceIdentity {
    DeviceIdentity {
        device_id: Uuid::from_u128(1).to_string(),
        display_name: "Local Device".to_string(),
        avatar_path: Some("/tmp/avatar.png".to_string()),
        created_at: 42,
        public_key: public_key_to_base64(&PublicKey::from([7_u8; 32])),
        protocol_version: 2,
    }
}

fn settings() -> AppSettings {
    AppSettings {
        download_dir: "/tmp/downloads".to_string(),
        max_concurrent_transfers: 3,
    }
}

fn transfer(id: &str, state: &str) -> TransferPayload {
    TransferPayload {
        id: id.to_string(),
        filename: format!("{id}.bin"),
        size: 2_048,
        progress: 0.5,
        speed: 512,
        state: state.to_string(),
        resumable: false,
        sender_id: Some(Uuid::from_u128(9).to_string()),
        local_path: Some(format!("/tmp/{id}.bin")),
        thumbnail_path: None,
        direction: None,
        folder_id: None,
        folder_relative_path: None,
    }
}

fn stored_group(
    seed: u128,
    name: &str,
    member_ids: Vec<String>,
    created_at: i64,
) -> StoredGroupInfo {
    StoredGroupInfo {
        id: Uuid::from_u128(seed).to_string(),
        name: name.to_string(),
        member_ids,
        created_at,
    }
}

fn og_metadata(url: &str, title: Option<&str>, description: Option<&str>) -> OgMetadata {
    OgMetadata {
        url: url.to_string(),
        title: title.map(str::to_string),
        description: description.map(str::to_string),
        image_url: None,
        site_name: Some("Example".to_string()),
    }
}

fn cached_og_metadata(
    metadata: OgMetadata,
    fetched_at_ms: i64,
    ttl_seconds: i64,
) -> CachedOgMetadata {
    CachedOgMetadata::new(metadata, fetched_at_ms, ttl_seconds)
}

#[derive(Default)]
struct MockDiscoveryService {
    peers: Mutex<Vec<PeerInfo>>,
    start_calls: AtomicUsize,
    stop_calls: AtomicUsize,
    start_error: Mutex<Option<String>>,
}

impl MockDiscoveryService {
    fn with_peers(self, peers: Vec<PeerInfo>) -> Self {
        *self.peers.lock().expect("lock peers") = peers;
        self
    }

    fn with_start_error(self, error: &str) -> Self {
        *self.start_error.lock().expect("lock start error") = Some(error.to_string());
        self
    }
}

#[async_trait]
impl DiscoveryServiceHandle for MockDiscoveryService {
    async fn start(&self) -> Result<(), String> {
        self.start_calls.fetch_add(1, Ordering::SeqCst);
        if let Some(error) = self.start_error.lock().expect("lock start error").clone() {
            return Err(error);
        }
        Ok(())
    }

    async fn stop(&self) -> Result<(), String> {
        self.stop_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn peers(&self) -> Vec<PeerInfo> {
        self.peers.lock().expect("lock peers").clone()
    }
}

type MockIceCandidateRecord = (String, String, String, Option<String>, Option<u16>);

#[derive(Default)]
struct MockMessagingService {
    sent_messages: Mutex<Vec<(String, String)>>,
    call_joins: Mutex<Vec<(String, String)>>,
    call_leaves: Mutex<Vec<String>>,
    call_offers: Mutex<Vec<(String, String, String, CallType)>>,
    call_answers: Mutex<Vec<(String, String, String)>>,
    ice_candidates: Mutex<Vec<MockIceCandidateRecord>>,
    call_hangups: Mutex<Vec<(String, String)>>,
    call_rejects: Mutex<Vec<(String, String, Option<String>)>>,
    group_creations: Mutex<Vec<(String, Vec<String>)>>,
    group_additions: Mutex<Vec<(String, Vec<String>)>>,
    group_removals: Mutex<Vec<(String, Vec<String>)>>,
    group_leaves: Mutex<Vec<String>>,
    group_messages: Mutex<Vec<(String, String)>>,
    edit_calls: Mutex<Vec<(String, String, String)>>,
    delete_calls: Mutex<Vec<(String, String)>>,
    messages: Mutex<Vec<Message>>,
    reply_counts: Mutex<HashMap<String, i64>>,
    groups: Mutex<HashMap<String, StoredGroupInfo>>,
    send_error: Mutex<Option<String>>,
}

impl MockMessagingService {
    fn with_messages(self, messages: Vec<Message>) -> Self {
        *self.messages.lock().expect("lock messages") = messages;
        self
    }

    fn with_send_error(self, error: &str) -> Self {
        *self.send_error.lock().expect("lock send error") = Some(error.to_string());
        self
    }

    fn with_groups(self, groups: Vec<StoredGroupInfo>) -> Self {
        *self.groups.lock().expect("lock groups") = groups
            .into_iter()
            .map(|group| (group.id.clone(), group))
            .collect();
        self
    }

    fn with_reply_counts(self, reply_counts: HashMap<String, i64>) -> Self {
        *self.reply_counts.lock().expect("lock reply counts") = reply_counts;
        self
    }

    fn group_info_by_id(&self, group_id: &str) -> Result<StoredGroupInfo, String> {
        let _ = Uuid::parse_str(group_id)
            .map_err(|error| format!("invalid group_id {group_id}: {error}"))?;
        self.groups
            .lock()
            .expect("lock groups")
            .get(group_id)
            .cloned()
            .ok_or_else(|| format!("unknown group {group_id}"))
    }
}

#[async_trait]
impl MessagingServiceHandle for MockMessagingService {
    async fn send_message(
        &self,
        peer_id: &str,
        content: &str,
        _reply_to_id: Option<&str>,
    ) -> Result<Message, String> {
        self.sent_messages
            .lock()
            .expect("lock sent messages")
            .push((peer_id.to_string(), content.to_string()));

        if let Some(error) = self.send_error.lock().expect("lock send error").clone() {
            return Err(error);
        }

        Ok(message(11, 2, 1, content, MessageStatus::Sent))
    }

    async fn send_call_offer(
        &self,
        peer_id: &str,
        call_id: &str,
        sdp: &str,
        call_type: CallType,
    ) -> Result<(), String> {
        self.call_offers.lock().expect("lock call offers").push((
            peer_id.to_string(),
            call_id.to_string(),
            sdp.to_string(),
            call_type,
        ));
        Ok(())
    }

    async fn send_call_join(&self, group_id: &str, call_id: &str) -> Result<(), String> {
        self.call_joins
            .lock()
            .expect("lock call joins")
            .push((group_id.to_string(), call_id.to_string()));
        Ok(())
    }

    async fn send_call_leave(&self, call_id: &str) -> Result<(), String> {
        self.call_leaves
            .lock()
            .expect("lock call leaves")
            .push(call_id.to_string());
        Ok(())
    }

    async fn send_call_answer(
        &self,
        peer_id: &str,
        call_id: &str,
        sdp: &str,
    ) -> Result<(), String> {
        self.call_answers.lock().expect("lock call answers").push((
            peer_id.to_string(),
            call_id.to_string(),
            sdp.to_string(),
        ));
        Ok(())
    }

    async fn send_ice_candidate(
        &self,
        peer_id: &str,
        call_id: &str,
        candidate: &str,
        sdp_mid: Option<&str>,
        sdp_mline_index: Option<u16>,
    ) -> Result<(), String> {
        self.ice_candidates
            .lock()
            .expect("lock ice candidates")
            .push((
                peer_id.to_string(),
                call_id.to_string(),
                candidate.to_string(),
                sdp_mid.map(str::to_string),
                sdp_mline_index,
            ));
        Ok(())
    }

    async fn send_call_hangup(&self, peer_id: &str, call_id: &str) -> Result<(), String> {
        self.call_hangups
            .lock()
            .expect("lock call hangups")
            .push((peer_id.to_string(), call_id.to_string()));
        Ok(())
    }

    async fn send_call_reject(
        &self,
        peer_id: &str,
        call_id: &str,
        reason: Option<&str>,
    ) -> Result<(), String> {
        self.call_rejects.lock().expect("lock call rejects").push((
            peer_id.to_string(),
            call_id.to_string(),
            reason.map(str::to_string),
        ));
        Ok(())
    }

    async fn get_messages(
        &self,
        _chat_id: &str,
        _limit: u32,
        _offset: u32,
    ) -> Result<Vec<Message>, String> {
        Ok(self.messages.lock().expect("lock messages").clone())
    }

    async fn get_reply_count(&self, message_id: &str) -> Result<i64, String> {
        Ok(*self
            .reply_counts
            .lock()
            .expect("lock reply counts")
            .get(message_id)
            .unwrap_or(&0))
    }

    async fn get_reply_counts(
        &self,
        message_ids: &[String],
    ) -> Result<HashMap<String, i64>, String> {
        let reply_counts = self.reply_counts.lock().expect("lock reply counts");

        Ok(message_ids
            .iter()
            .cloned()
            .map(|message_id| {
                let count = reply_counts.get(&message_id).copied().unwrap_or(0);
                (message_id, count)
            })
            .collect())
    }

    async fn create_group(
        &self,
        name: &str,
        members: &[String],
    ) -> Result<StoredGroupInfo, String> {
        self.group_creations
            .lock()
            .expect("lock group creations")
            .push((name.to_string(), members.to_vec()));

        let mut member_ids = Vec::with_capacity(members.len() + 1);
        member_ids.push(identity().device_id);
        member_ids.extend(members.iter().cloned());
        let group = StoredGroupInfo {
            id: Uuid::from_u128(77).to_string(),
            name: name.to_string(),
            member_ids,
            created_at: 7_777,
        };
        self.groups
            .lock()
            .expect("lock groups")
            .insert(group.id.clone(), group.clone());

        Ok(group)
    }

    async fn add_group_members(
        &self,
        group_id: &str,
        member_ids: &[String],
    ) -> Result<StoredGroupInfo, String> {
        let _ = Uuid::parse_str(group_id)
            .map_err(|error| format!("invalid group_id {group_id}: {error}"))?;
        self.group_additions
            .lock()
            .expect("lock group additions")
            .push((group_id.to_string(), member_ids.to_vec()));
        let mut groups = self.groups.lock().expect("lock groups");
        let group = groups
            .get_mut(group_id)
            .ok_or_else(|| format!("unknown group {group_id}"))?;

        for member_id in member_ids {
            if !group.member_ids.contains(member_id) {
                group.member_ids.push(member_id.clone());
            }
        }

        Ok(group.clone())
    }

    async fn remove_group_members(
        &self,
        group_id: &str,
        member_ids: &[String],
    ) -> Result<StoredGroupInfo, String> {
        let _ = Uuid::parse_str(group_id)
            .map_err(|error| format!("invalid group_id {group_id}: {error}"))?;
        self.group_removals
            .lock()
            .expect("lock group removals")
            .push((group_id.to_string(), member_ids.to_vec()));
        let mut groups = self.groups.lock().expect("lock groups");
        let group = groups
            .get_mut(group_id)
            .ok_or_else(|| format!("unknown group {group_id}"))?;
        group
            .member_ids
            .retain(|member_id| !member_ids.contains(member_id));
        Ok(group.clone())
    }

    async fn get_group_info(&self, group_id: &str) -> Result<StoredGroupInfo, String> {
        self.group_info_by_id(group_id)
    }

    async fn list_groups(&self) -> Result<Vec<StoredGroupInfo>, String> {
        let mut groups = self
            .groups
            .lock()
            .expect("lock groups")
            .values()
            .cloned()
            .collect::<Vec<_>>();
        groups.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then(right.id.cmp(&left.id))
        });
        Ok(groups)
    }

    async fn leave_group(&self, group_id: &str) -> Result<(), String> {
        let _ = Uuid::parse_str(group_id)
            .map_err(|error| format!("invalid group_id {group_id}: {error}"))?;
        self.group_leaves
            .lock()
            .expect("lock group leaves")
            .push(group_id.to_string());
        let mut groups = self.groups.lock().expect("lock groups");
        let group = groups
            .get_mut(group_id)
            .ok_or_else(|| format!("unknown group {group_id}"))?;
        group
            .member_ids
            .retain(|member_id| member_id != &identity().device_id);
        Ok(())
    }

    async fn send_group_message(
        &self,
        group_id: &str,
        content: &str,
        _reply_to_id: Option<&str>,
    ) -> Result<Message, String> {
        self.group_messages
            .lock()
            .expect("lock group messages")
            .push((group_id.to_string(), content.to_string()));

        Ok(message(12, 77, 1, content, MessageStatus::Delivered))
    }

    async fn edit_message(
        &self,
        message_id: &str,
        new_content: &str,
        sender_id: &str,
    ) -> Result<(), String> {
        self.edit_calls.lock().expect("lock edit calls").push((
            message_id.to_string(),
            new_content.to_string(),
            sender_id.to_string(),
        ));
        Ok(())
    }

    async fn delete_message(&self, message_id: &str, sender_id: &str) -> Result<(), String> {
        self.delete_calls
            .lock()
            .expect("lock delete calls")
            .push((message_id.to_string(), sender_id.to_string()));
        Ok(())
    }

    async fn shutdown(&self) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Default)]
struct MockOgMetadataService {
    cache_reads: Mutex<Vec<String>>,
    remote_requests: Mutex<Vec<String>>,
    saved_entries: Mutex<Vec<(OgMetadata, u64)>>,
    cached_entries: Mutex<HashMap<String, CachedOgMetadata>>,
    remote_response: Mutex<Option<Result<OgMetadata, String>>>,
}

impl MockOgMetadataService {
    fn with_cached_entry(self, entry: CachedOgMetadata) -> Self {
        self.cached_entries
            .lock()
            .expect("lock cached og entries")
            .insert(entry.metadata().url.clone(), entry);
        self
    }

    fn with_remote_response(self, response: Result<OgMetadata, String>) -> Self {
        *self.remote_response.lock().expect("lock og response") = Some(response);
        self
    }
}

#[async_trait]
impl OgMetadataServiceHandle for MockOgMetadataService {
    async fn get_cached_og_metadata(&self, url: &str) -> Result<Option<CachedOgMetadata>, String> {
        self.cache_reads
            .lock()
            .expect("lock cached og urls")
            .push(url.to_string());

        Ok(self
            .cached_entries
            .lock()
            .expect("lock cached og entries")
            .get(url)
            .cloned())
    }

    async fn save_og_metadata(
        &self,
        metadata: &OgMetadata,
        ttl_seconds: u64,
    ) -> Result<(), String> {
        self.saved_entries
            .lock()
            .expect("lock saved og entries")
            .push((metadata.clone(), ttl_seconds));
        self.cached_entries
            .lock()
            .expect("lock cached og entries")
            .insert(
                metadata.url.clone(),
                CachedOgMetadata::new(metadata.clone(), 1_700_000_000_000, ttl_seconds as i64),
            );
        Ok(())
    }

    async fn fetch_remote_og_metadata(&self, url: &str) -> Result<OgMetadata, String> {
        self.remote_requests
            .lock()
            .expect("lock requested og urls")
            .push(url.to_string());

        match self
            .remote_response
            .lock()
            .expect("lock og response")
            .clone()
        {
            Some(result) => result,
            None => Ok(OgMetadata::empty(url)),
        }
    }
}

#[derive(Default)]
struct MockTransferService {
    sent_files: Mutex<Vec<(String, PathBuf)>>,
    sent_folders: Mutex<Vec<(String, PathBuf)>>,
    accepted_offers: Mutex<Vec<String>>,
    accepted_folders: Mutex<Vec<(String, PathBuf)>>,
    rejected_offers: Mutex<Vec<(String, Option<String>)>>,
    rejected_folders: Mutex<Vec<String>>,
    cancelled_transfers: Mutex<Vec<String>>,
    resumed_transfers: Mutex<Vec<String>>,
    retried_transfers: Mutex<Vec<String>>,
    cancelled_folders: Mutex<Vec<String>>,
    transfers: Mutex<Vec<TransferPayload>>,
    reject_error: Mutex<Option<String>>,
}

impl MockTransferService {
    fn with_transfers(self, transfers: Vec<TransferPayload>) -> Self {
        *self.transfers.lock().expect("lock transfers") = transfers;
        self
    }

    fn with_reject_error(self, error: &str) -> Self {
        *self.reject_error.lock().expect("lock reject error") = Some(error.to_string());
        self
    }
}

#[async_trait]
impl TransferServiceHandle for MockTransferService {
    async fn send_file(&self, peer_id: &str, file_path: &Path) -> Result<String, String> {
        self.sent_files
            .lock()
            .expect("lock sent files")
            .push((peer_id.to_string(), file_path.to_path_buf()));
        Ok("transfer-123".to_string())
    }

    async fn send_folder(&self, peer_id: &str, folder_path: &Path) -> Result<String, String> {
        self.sent_folders
            .lock()
            .expect("lock sent folders")
            .push((peer_id.to_string(), folder_path.to_path_buf()));
        Ok("folder-transfer-123".to_string())
    }

    async fn accept_file(&self, offer_id: &str) -> Result<String, String> {
        self.accepted_offers
            .lock()
            .expect("lock accepted offers")
            .push(offer_id.to_string());
        Ok(offer_id.to_string())
    }

    async fn reject_file(&self, offer_id: &str, reason: Option<String>) -> Result<(), String> {
        self.rejected_offers
            .lock()
            .expect("lock rejected offers")
            .push((offer_id.to_string(), reason.clone()));

        if let Some(error) = self.reject_error.lock().expect("lock reject error").clone() {
            return Err(error);
        }

        Ok(())
    }

    async fn cancel_transfer(&self, transfer_id: &str) -> Result<(), String> {
        self.cancelled_transfers
            .lock()
            .expect("lock cancelled transfers")
            .push(transfer_id.to_string());
        Ok(())
    }

    async fn resume_transfer(&self, transfer_id: &str) -> Result<String, String> {
        self.resumed_transfers
            .lock()
            .expect("lock resumed transfers")
            .push(transfer_id.to_string());
        Ok(format!("resumed-{transfer_id}"))
    }

    async fn retry_transfer(&self, transfer_id: &str) -> Result<String, String> {
        self.retried_transfers
            .lock()
            .expect("lock retried transfers")
            .push(transfer_id.to_string());
        Ok(format!("retried-{transfer_id}"))
    }

    async fn accept_folder_transfer(
        &self,
        _folder_id: &str,
        _target_dir: &Path,
    ) -> Result<(), String> {
        self.accepted_folders
            .lock()
            .expect("lock accepted folders")
            .push((_folder_id.to_string(), _target_dir.to_path_buf()));
        Ok(())
    }

    async fn reject_folder_transfer(&self, _folder_id: &str) -> Result<(), String> {
        self.rejected_folders
            .lock()
            .expect("lock rejected folders")
            .push(_folder_id.to_string());
        Ok(())
    }

    async fn cancel_folder_transfer(&self, _folder_id: &str) -> Result<(), String> {
        self.cancelled_folders
            .lock()
            .expect("lock cancelled folders")
            .push(_folder_id.to_string());
        Ok(())
    }

    async fn get_transfers(&self) -> Result<Vec<TransferPayload>, String> {
        Ok(self.transfers.lock().expect("lock transfers").clone())
    }
}

#[derive(Default)]
struct MockIdentityService {
    update_names: Mutex<Vec<String>>,
    update_avatars: Mutex<Vec<String>>,
    peer_fingerprints: Mutex<HashMap<String, PeerFingerprintInfo>>,
}

impl MockIdentityService {
    fn with_peer_fingerprint(self, info: PeerFingerprintInfo) -> Self {
        self.peer_fingerprints
            .lock()
            .expect("lock peer fingerprints")
            .insert(info.peer_id.clone(), info);
        self
    }
}

impl IdentityServiceHandle for MockIdentityService {
    fn get_identity(&self) -> Result<DeviceIdentity, String> {
        Ok(identity())
    }

    fn update_display_name(&self, name: &str) -> Result<DeviceIdentity, String> {
        self.update_names
            .lock()
            .expect("lock update names")
            .push(name.to_string());
        Ok(DeviceIdentity {
            display_name: name.to_string(),
            ..identity()
        })
    }

    fn update_avatar(&self, path: &str) -> Result<DeviceIdentity, String> {
        self.update_avatars
            .lock()
            .expect("lock update avatars")
            .push(path.to_string());
        Ok(DeviceIdentity {
            avatar_path: Some(path.to_string()),
            ..identity()
        })
    }

    fn get_own_fingerprint(&self) -> Result<OwnFingerprintInfo, String> {
        let public_key =
            public_key_from_base64(&identity().public_key).map_err(|error| error.to_string())?;
        Ok(OwnFingerprintInfo {
            device_id: identity().device_id,
            fingerprint: fingerprint(&public_key),
        })
    }

    fn get_peer_fingerprint(&self, peer_id: &str) -> Result<PeerFingerprintInfo, String> {
        self.peer_fingerprints
            .lock()
            .expect("lock peer fingerprints")
            .get(peer_id)
            .cloned()
            .ok_or_else(|| format!("unknown peer fingerprint for {peer_id}"))
    }

    fn toggle_peer_verified(&self, peer_id: &str, verified: bool) -> Result<(), String> {
        let mut peer_fingerprints = self
            .peer_fingerprints
            .lock()
            .expect("lock peer fingerprints");
        let fingerprint = peer_fingerprints
            .get_mut(peer_id)
            .ok_or_else(|| format!("unknown peer fingerprint for {peer_id}"))?;
        fingerprint.verified = verified;
        Ok(())
    }
}

#[derive(Default)]
struct MockSettingsService {
    updated_settings: Mutex<Vec<AppSettings>>,
    update_error: Mutex<Option<String>>,
}

impl MockSettingsService {
    fn with_update_error(self, error: &str) -> Self {
        *self.update_error.lock().expect("lock update error") = Some(error.to_string());
        self
    }
}

#[async_trait]
impl SettingsServiceHandle for MockSettingsService {
    fn get_settings(&self) -> Result<AppSettings, String> {
        Ok(settings())
    }

    async fn update_settings(&self, settings: AppSettings) -> Result<AppSettings, String> {
        self.updated_settings
            .lock()
            .expect("lock updated settings")
            .push(settings.clone());

        if let Some(error) = self.update_error.lock().expect("lock update error").clone() {
            return Err(error);
        }

        Ok(settings)
    }
}

#[derive(Default)]
struct MockEmitter;

impl FrontendEmitter for MockEmitter {
    fn emit_json(&self, _event: &str, _payload: serde_json::Value) -> Result<(), String> {
        Ok(())
    }
}

fn app_state(
    discovery: Arc<dyn DiscoveryServiceHandle>,
    messaging: Arc<dyn MessagingServiceHandle>,
    og_metadata: Arc<dyn OgMetadataServiceHandle>,
    transfers: Arc<dyn TransferServiceHandle>,
    identity_service: Arc<dyn IdentityServiceHandle>,
    settings_service: Arc<dyn SettingsServiceHandle>,
) -> AppState {
    AppState::new(
        identity().device_id,
        discovery,
        messaging,
        og_metadata,
        transfers,
        identity_service,
        settings_service,
    )
}

mod commands {
    use super::*;

    #[derive(Default)]
    struct RecordingEmitter {
        events: Mutex<Vec<(String, serde_json::Value)>>,
    }

    impl RecordingEmitter {
        fn snapshot(&self) -> Vec<(String, serde_json::Value)> {
            self.events.lock().expect("lock og updated events").clone()
        }
    }

    impl FrontendEmitter for RecordingEmitter {
        fn emit_json(&self, event: &str, payload: serde_json::Value) -> Result<(), String> {
            self.events
                .lock()
                .expect("lock og updated events")
                .push((event.to_string(), payload));
            Ok(())
        }
    }

    #[tokio::test]
    async fn start_discovery_dispatches_to_discovery_service() {
        let discovery = Arc::new(MockDiscoveryService::default());
        let state = app_state(
            discovery.clone(),
            Arc::new(MockMessagingService::default()),
            Arc::new(MockOgMetadataService::default()),
            Arc::new(MockTransferService::default()),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        start_discovery_impl(&state)
            .await
            .expect("start discovery command");

        assert_eq!(discovery.start_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn stop_discovery_dispatches_to_discovery_service() {
        let discovery = Arc::new(MockDiscoveryService::default());
        let state = app_state(
            discovery.clone(),
            Arc::new(MockMessagingService::default()),
            Arc::new(MockOgMetadataService::default()),
            Arc::new(MockTransferService::default()),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        stop_discovery_impl(&state)
            .await
            .expect("stop discovery command");

        assert_eq!(discovery.stop_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn get_peers_returns_frontend_peer_shape() {
        let state = app_state(
            Arc::new(MockDiscoveryService::default().with_peers(vec![peer(2, "Alice")])),
            Arc::new(MockMessagingService::default()),
            Arc::new(MockOgMetadataService::default()),
            Arc::new(MockTransferService::default()),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        let peers = get_peers_impl(&state).await.expect("get peers command");

        assert_eq!(
            peers,
            vec![PeerPayload {
                id: Uuid::from_u128(2).to_string(),
                name: "Alice".to_string(),
                status: "online".to_string(),
                warning: None,
            }]
        );
    }

    #[test]
    fn get_webrtc_platform_info_reports_platform_and_backend() {
        assert_eq!(
            get_webrtc_platform_info_impl(),
            WebRTCPlatformInfo {
                platform: std::env::consts::OS.to_string(),
                webview: current_webview_backend().to_string(),
                webview_version: None,
            }
        );
    }

    #[test]
    fn check_call_support_reports_expected_platform_support() {
        let support = check_call_support_impl();

        assert_eq!(support.platform, std::env::consts::OS.to_string());

        if cfg!(target_os = "linux") {
            assert_eq!(
                support,
                CallSupportInfo {
                    supported: false,
                    platform: std::env::consts::OS.to_string(),
                    reason: Some("Linux calling support is not enabled yet".to_string()),
                }
            );
            return;
        }

        assert_eq!(
            support,
            CallSupportInfo {
                supported: true,
                platform: std::env::consts::OS.to_string(),
                reason: None,
            }
        );
    }

    #[tokio::test]
    async fn fetch_og_metadata_dispatches_to_service_on_cache_miss() {
        let url = "https://example.org/post".to_string();
        let expected = og_metadata(&url, Some("Example title"), Some("Example description"));
        let og_metadata =
            Arc::new(MockOgMetadataService::default().with_remote_response(Ok(expected.clone())));
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            Arc::new(MockMessagingService::default()),
            og_metadata.clone(),
            Arc::new(MockTransferService::default()),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        let metadata = fetch_og_metadata_impl(&state, Arc::new(MockEmitter), url.clone())
            .await
            .expect("fetch og metadata command");

        assert_eq!(metadata, expected);
        assert_eq!(
            og_metadata
                .cache_reads
                .lock()
                .expect("lock cached og urls")
                .clone(),
            vec![url]
        );
        assert_eq!(
            og_metadata
                .remote_requests
                .lock()
                .expect("lock requested og urls")
                .clone(),
            vec!["https://example.org/post".to_string()]
        );
        assert_eq!(
            og_metadata
                .saved_entries
                .lock()
                .expect("lock saved og entries")
                .clone(),
            vec![(expected, 86_400)]
        );
    }

    #[tokio::test]
    async fn fetch_og_metadata_uses_cached_row_when_fresh() {
        let url = "https://example.org/cached-preview";
        let metadata = og_metadata(url, Some("Cached title"), Some("Cached description"));
        let fresh_at = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("current time")
            .as_millis() as i64)
            - 1_000;
        let service = Arc::new(
            MockOgMetadataService::default().with_cached_entry(cached_og_metadata(
                metadata.clone(),
                fresh_at,
                86_400,
            )),
        );

        let loaded = fetch_og_metadata_with_cache_policy(
            service.clone(),
            Arc::new(MockEmitter),
            url.to_string(),
        )
        .await
        .expect("load cached og metadata");

        assert_eq!(loaded, metadata);
        assert!(service
            .remote_requests
            .lock()
            .expect("lock requested og urls")
            .is_empty());
        assert!(service
            .saved_entries
            .lock()
            .expect("lock saved og entries")
            .is_empty());
    }

    #[tokio::test]
    async fn fetch_og_metadata_returns_stale_cache_and_refreshes_in_background() {
        let url = "https://example.org/stale-preview".to_string();
        let stale_metadata = og_metadata(&url, Some("Stale title"), Some("Stale description"));
        let refreshed_metadata = og_metadata(&url, Some("Fresh title"), Some("Fresh description"));
        let stale_at = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("current time")
            .as_millis() as i64)
            - 86_401_000;
        let service = Arc::new(
            MockOgMetadataService::default()
                .with_cached_entry(cached_og_metadata(stale_metadata.clone(), stale_at, 86_400))
                .with_remote_response(Ok(refreshed_metadata.clone())),
        );
        let emitter = Arc::new(RecordingEmitter::default());

        let returned =
            fetch_og_metadata_with_cache_policy(service.clone(), emitter.clone(), url.clone())
                .await
                .expect("return stale og metadata immediately");

        assert_eq!(returned, stale_metadata);

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if !service
                    .saved_entries
                    .lock()
                    .expect("lock saved og entries")
                    .is_empty()
                    && !emitter.snapshot().is_empty()
                {
                    break;
                }

                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("background stale refresh should complete");

        assert_eq!(
            service
                .remote_requests
                .lock()
                .expect("lock requested og urls")
                .clone(),
            vec![url.clone()]
        );
        assert_eq!(
            service
                .saved_entries
                .lock()
                .expect("lock saved og entries")
                .clone(),
            vec![(refreshed_metadata.clone(), 86_400)]
        );
        assert_eq!(
            emitter.snapshot(),
            vec![(
                "og:updated".to_string(),
                serde_json::json!({
                    "url": url,
                    "metadata": refreshed_metadata,
                })
            )]
        );
    }

    #[tokio::test]
    async fn fetch_og_metadata_returns_empty_metadata_for_invalid_url_without_touching_service() {
        let service = Arc::new(MockOgMetadataService::default());

        let metadata = fetch_og_metadata_with_cache_policy(
            service.clone(),
            Arc::new(MockEmitter),
            "not a url".to_string(),
        )
        .await
        .expect("invalid url should degrade to empty metadata");

        assert_eq!(metadata, OgMetadata::empty("not a url"));
        assert!(service
            .cache_reads
            .lock()
            .expect("lock cached og urls")
            .is_empty());
        assert!(service
            .remote_requests
            .lock()
            .expect("lock requested og urls")
            .is_empty());
        assert!(service
            .saved_entries
            .lock()
            .expect("lock saved og entries")
            .is_empty());
    }

    #[test]
    fn fetch_og_command_is_registered_in_generate_handler() {
        let lib_rs = include_str!("../lib.rs");

        assert!(
            lib_rs.contains("commands::fetch_og_metadata"),
            "expected `commands::fetch_og_metadata` to be registered in generate_handler!"
        );
    }

    #[tokio::test]
    async fn call_signaling_commands_dispatch_arguments_to_messaging_service() {
        let messaging = Arc::new(MockMessagingService::default());
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            messaging.clone(),
            Arc::new(MockOgMetadataService::default()),
            Arc::new(MockTransferService::default()),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );
        let peer_id = Uuid::from_u128(2).to_string();
        let call_id = Uuid::from_u128(70).to_string();
        let group_id = Uuid::from_u128(77).to_string();

        send_call_join_impl(&state, group_id.clone(), call_id.clone())
            .await
            .expect("send call join command");

        send_call_offer_impl(
            &state,
            peer_id.clone(),
            call_id.clone(),
            "offer-sdp".to_string(),
            CallType::Video,
        )
        .await
        .expect("send call offer command");
        send_call_answer_impl(
            &state,
            peer_id.clone(),
            call_id.clone(),
            "answer-sdp".to_string(),
        )
        .await
        .expect("send call answer command");
        send_ice_candidate_impl(
            &state,
            peer_id.clone(),
            call_id.clone(),
            "candidate:1 1 udp 1 127.0.0.1 9000 typ host".to_string(),
            Some("audio".to_string()),
            Some(0),
        )
        .await
        .expect("send ice candidate command");
        send_call_hangup_impl(&state, peer_id.clone(), call_id.clone())
            .await
            .expect("send call hangup command");
        send_call_reject_impl(
            &state,
            peer_id.clone(),
            Uuid::from_u128(71).to_string(),
            Some("busy".to_string()),
        )
        .await
        .expect("send call reject command");
        send_call_leave_impl(&state, call_id.clone())
            .await
            .expect("send call leave command");

        assert_eq!(
            messaging
                .call_joins
                .lock()
                .expect("lock call joins")
                .clone(),
            vec![(group_id, call_id.clone())]
        );

        assert_eq!(
            messaging
                .call_offers
                .lock()
                .expect("lock call offers")
                .clone(),
            vec![(
                peer_id.clone(),
                call_id.clone(),
                "offer-sdp".to_string(),
                CallType::Video,
            )]
        );
        assert_eq!(
            messaging
                .call_answers
                .lock()
                .expect("lock call answers")
                .clone(),
            vec![(peer_id.clone(), call_id.clone(), "answer-sdp".to_string())]
        );
        assert_eq!(
            messaging
                .ice_candidates
                .lock()
                .expect("lock ice candidates")
                .clone(),
            vec![(
                peer_id.clone(),
                call_id.clone(),
                "candidate:1 1 udp 1 127.0.0.1 9000 typ host".to_string(),
                Some("audio".to_string()),
                Some(0),
            )]
        );
        assert_eq!(
            messaging
                .call_hangups
                .lock()
                .expect("lock call hangups")
                .clone(),
            vec![(peer_id.clone(), call_id.clone())]
        );
        assert_eq!(
            messaging
                .call_rejects
                .lock()
                .expect("lock call rejects")
                .clone(),
            vec![(
                peer_id,
                Uuid::from_u128(71).to_string(),
                Some("busy".to_string()),
            )]
        );
        assert_eq!(
            messaging
                .call_leaves
                .lock()
                .expect("lock call leaves")
                .clone(),
            vec![call_id]
        );
    }

    #[tokio::test]
    async fn send_message_returns_message_id_and_passes_arguments() {
        let messaging = Arc::new(MockMessagingService::default());
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            messaging.clone(),
            Arc::new(MockOgMetadataService::default()),
            Arc::new(MockTransferService::default()),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        let message_id = send_message_impl(
            &state,
            Uuid::from_u128(2).to_string(),
            "hello".to_string(),
            None,
        )
        .await
        .expect("send message command");

        assert_eq!(message_id, Uuid::from_u128(11).to_string());
        assert_eq!(
            messaging
                .sent_messages
                .lock()
                .expect("lock sent messages")
                .clone(),
            vec![(Uuid::from_u128(2).to_string(), "hello".to_string())]
        );
    }

    #[tokio::test]
    async fn get_messages_maps_local_and_remote_messages() {
        let local_uuid = identity().device_id;
        let local_user_uuid = Uuid::parse_str(&local_uuid).expect("parse local uuid");
        let peer_uuid = Uuid::from_u128(2);
        let state = AppState::new(
            local_uuid,
            Arc::new(MockDiscoveryService::default()),
            Arc::new(MockMessagingService::default().with_messages(vec![
                Message {
                    id: Uuid::from_u128(10),
                    chat_id: ChatId(peer_uuid),
                    sender_id: UserId(local_user_uuid),
                    content: "from local".to_string(),
                    timestamp_ms: 1_000,
                    status: MessageStatus::Sent,
                    edit_version: 0,
                    edited_at: None,
                    is_deleted: false,
                    deleted_at: None,
                    reply_to_id: None,
                    reply_to_preview: None,
                },
                Message {
                    id: Uuid::from_u128(11),
                    chat_id: ChatId(peer_uuid),
                    sender_id: UserId(peer_uuid),
                    content: "from peer".to_string(),
                    timestamp_ms: 2_000,
                    status: MessageStatus::Delivered,
                    edit_version: 0,
                    edited_at: None,
                    is_deleted: false,
                    deleted_at: None,
                    reply_to_id: None,
                    reply_to_preview: None,
                },
            ])),
            Arc::new(MockOgMetadataService::default()),
            Arc::new(MockTransferService::default()),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        let messages = get_messages_impl(&state, peer_uuid.to_string(), 20, 0)
            .await
            .expect("get messages command");

        assert_eq!(
            messages,
            vec![
                ChatMessagePayload {
                    id: Uuid::from_u128(10).to_string(),
                    sender_id: "local".to_string(),
                    receiver_id: peer_uuid.to_string(),
                    content: "from local".to_string(),
                    timestamp: 1_000,
                    encrypted: None,
                    status: "sent".to_string(),
                    edit_version: 0,
                    edited_at: None,
                    is_deleted: false,
                    deleted_at: None,
                    reply_to_id: None,
                    reply_to_preview: None,
                },
                ChatMessagePayload {
                    id: Uuid::from_u128(11).to_string(),
                    sender_id: peer_uuid.to_string(),
                    receiver_id: "local".to_string(),
                    content: "from peer".to_string(),
                    timestamp: 2_000,
                    encrypted: None,
                    status: "delivered".to_string(),
                    edit_version: 0,
                    edited_at: None,
                    is_deleted: false,
                    deleted_at: None,
                    reply_to_id: None,
                    reply_to_preview: None,
                },
            ]
        );
    }

    #[tokio::test]
    async fn get_reply_count_commands_return_single_and_batch_totals() {
        let first_message_id = Uuid::from_u128(31).to_string();
        let second_message_id = Uuid::from_u128(32).to_string();
        let missing_message_id = Uuid::from_u128(33).to_string();
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            Arc::new(
                MockMessagingService::default().with_reply_counts(HashMap::from([
                    (first_message_id.clone(), 3),
                    (second_message_id.clone(), 1),
                ])),
            ),
            Arc::new(MockOgMetadataService::default()),
            Arc::new(MockTransferService::default()),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        let first_reply_count = get_reply_count_impl(&state, first_message_id.clone())
            .await
            .expect("get single reply count command");
        assert_eq!(first_reply_count, 3);

        let reply_counts = get_reply_counts_impl(
            &state,
            vec![
                first_message_id.clone(),
                second_message_id.clone(),
                missing_message_id.clone(),
            ],
        )
        .await
        .expect("get reply counts command");

        assert_eq!(reply_counts.get(&first_message_id), Some(&3));
        assert_eq!(reply_counts.get(&second_message_id), Some(&1));
        assert_eq!(reply_counts.get(&missing_message_id), Some(&0));
    }

    #[tokio::test]
    async fn create_group_returns_rich_group_info_response() {
        let messaging = Arc::new(MockMessagingService::default());
        let state = app_state(
            Arc::new(MockDiscoveryService::default().with_peers(vec![peer(2, "Alice")])),
            messaging.clone(),
            Arc::new(MockOgMetadataService::default()),
            Arc::new(MockTransferService::default()),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        let group = create_group_impl(
            &state,
            "Project Room".to_string(),
            vec![Uuid::from_u128(2).to_string()],
        )
        .await
        .expect("create group command");

        assert_eq!(
            group,
            GroupInfoResponse {
                id: Uuid::from_u128(77).to_string(),
                name: "Project Room".to_string(),
                members: vec![
                    GroupMemberInfo {
                        device_id: identity().device_id,
                        name: "Local Device".to_string(),
                        status: "online".to_string(),
                    },
                    GroupMemberInfo {
                        device_id: Uuid::from_u128(2).to_string(),
                        name: "Alice".to_string(),
                        status: "online".to_string(),
                    },
                ],
                created_at: 7_777,
            }
        );
        assert_eq!(
            messaging
                .group_creations
                .lock()
                .expect("lock group creations")
                .clone(),
            vec![(
                "Project Room".to_string(),
                vec![Uuid::from_u128(2).to_string()]
            )]
        );
    }

    #[tokio::test]
    async fn group_commands_return_frontend_ready_group_shapes_and_dispatch() {
        let engineering = stored_group(
            77,
            "Engineering",
            vec![identity().device_id, Uuid::from_u128(2).to_string()],
            7_777,
        );
        let support = stored_group(
            88,
            "Support",
            vec![identity().device_id, Uuid::from_u128(3).to_string()],
            8_888,
        );
        let messaging = Arc::new(
            MockMessagingService::default().with_groups(vec![engineering.clone(), support.clone()]),
        );
        let state = app_state(
            Arc::new(
                MockDiscoveryService::default().with_peers(vec![peer(2, "Alice"), peer(3, "Bob")]),
            ),
            messaging.clone(),
            Arc::new(MockOgMetadataService::default()),
            Arc::new(MockTransferService::default()),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        let group_id = Uuid::from_u128(77).to_string();
        add_group_members_impl(
            &state,
            group_id.clone(),
            vec![Uuid::from_u128(4).to_string()],
        )
        .await
        .expect("add group members command");
        remove_group_members_impl(
            &state,
            group_id.clone(),
            vec![Uuid::from_u128(4).to_string()],
        )
        .await
        .expect("remove group members command");

        let group = get_group_info_impl(&state, group_id.clone())
            .await
            .expect("get group info command");
        assert_eq!(
            group,
            GroupInfoResponse {
                id: group_id.clone(),
                name: "Engineering".to_string(),
                members: vec![
                    GroupMemberInfo {
                        device_id: identity().device_id,
                        name: "Local Device".to_string(),
                        status: "online".to_string(),
                    },
                    GroupMemberInfo {
                        device_id: Uuid::from_u128(2).to_string(),
                        name: "Alice".to_string(),
                        status: "online".to_string(),
                    },
                ],
                created_at: engineering.created_at,
            }
        );

        let groups = list_groups_impl(&state).await.expect("list groups command");
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].id, support.id);
        assert_eq!(groups[1].id, engineering.id);
        assert_eq!(groups[0].members[1].name, "Bob");

        leave_group_impl(&state, group_id.clone())
            .await
            .expect("leave group command");
        assert_eq!(
            messaging
                .group_additions
                .lock()
                .expect("lock group additions")
                .clone(),
            vec![(group_id.clone(), vec![Uuid::from_u128(4).to_string()])]
        );
        assert_eq!(
            messaging
                .group_removals
                .lock()
                .expect("lock group removals")
                .clone(),
            vec![(group_id.clone(), vec![Uuid::from_u128(4).to_string()])]
        );
        assert_eq!(
            messaging
                .group_leaves
                .lock()
                .expect("lock group leaves")
                .clone(),
            vec![group_id]
        );
    }

    #[test]
    fn group_commands_are_registered_in_generate_handler() {
        let lib_rs = include_str!("../lib.rs");
        for command in [
            "commands::get_reply_count",
            "commands::get_reply_counts",
            "commands::create_group",
            "commands::add_group_members",
            "commands::remove_group_members",
            "commands::get_group_info",
            "commands::list_groups",
            "commands::leave_group",
            "commands::fetch_og_metadata",
            "commands::get_webrtc_platform_info",
            "commands::check_call_support",
        ] {
            assert!(
                lib_rs.contains(command),
                "expected `{command}` to be registered in generate_handler!"
            );
        }
    }

    #[test]
    fn call_signaling_commands_are_registered_in_generate_handler() {
        let lib_rs = include_str!("../lib.rs");
        for command in [
            "commands::send_call_join",
            "commands::send_call_leave",
            "commands::send_call_offer",
            "commands::send_call_answer",
            "commands::send_ice_candidate",
            "commands::send_call_hangup",
            "commands::send_call_reject",
        ] {
            assert!(
                lib_rs.contains(command),
                "expected `{command}` to be registered in generate_handler!"
            );
        }
    }

    #[tokio::test]
    async fn send_group_message_returns_message_id() {
        let messaging = Arc::new(MockMessagingService::default());
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            messaging.clone(),
            Arc::new(MockOgMetadataService::default()),
            Arc::new(MockTransferService::default()),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        let message_id = send_group_message_impl(
            &state,
            Uuid::from_u128(77).to_string(),
            "hello group".to_string(),
            None,
        )
        .await
        .expect("send group message command");

        assert_eq!(message_id, Uuid::from_u128(12).to_string());
        assert_eq!(
            messaging
                .group_messages
                .lock()
                .expect("lock group messages")
                .clone(),
            vec![(Uuid::from_u128(77).to_string(), "hello group".to_string())]
        );
    }

    #[tokio::test]
    async fn edit_message_dispatches_to_messaging_service() {
        let messaging = Arc::new(MockMessagingService::default());
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            messaging.clone(),
            Arc::new(MockOgMetadataService::default()),
            Arc::new(MockTransferService::default()),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        edit_message_impl(&state, "msg-1".to_string(), "updated".to_string())
            .await
            .expect("edit message command");

        assert_eq!(
            messaging
                .edit_calls
                .lock()
                .expect("lock edit calls")
                .clone(),
            vec![(
                "msg-1".to_string(),
                "updated".to_string(),
                identity().device_id
            )]
        );
    }

    #[tokio::test]
    async fn delete_message_dispatches_to_messaging_service() {
        let messaging = Arc::new(MockMessagingService::default());
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            messaging.clone(),
            Arc::new(MockOgMetadataService::default()),
            Arc::new(MockTransferService::default()),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        delete_message_impl(&state, "msg-1".to_string())
            .await
            .expect("delete message command");

        assert_eq!(
            messaging
                .delete_calls
                .lock()
                .expect("lock delete calls")
                .clone(),
            vec![("msg-1".to_string(), identity().device_id)]
        );
    }

    #[tokio::test]
    async fn send_file_returns_transfer_id() {
        let transfers = Arc::new(MockTransferService::default());
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            Arc::new(MockMessagingService::default()),
            Arc::new(MockOgMetadataService::default()),
            transfers.clone(),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        let transfer_id = send_file_impl(
            &state,
            Uuid::from_u128(2).to_string(),
            "/tmp/file.txt".to_string(),
        )
        .await
        .expect("send file command");

        assert_eq!(transfer_id, "transfer-123");
        assert_eq!(
            transfers
                .sent_files
                .lock()
                .expect("lock sent files")
                .clone(),
            vec![(
                Uuid::from_u128(2).to_string(),
                PathBuf::from("/tmp/file.txt")
            )]
        );
    }

    #[tokio::test]
    async fn send_folder_returns_transfer_id() {
        let transfers = Arc::new(MockTransferService::default());
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            Arc::new(MockMessagingService::default()),
            Arc::new(MockOgMetadataService::default()),
            transfers.clone(),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        let transfer_id = send_folder_impl(
            &state,
            Uuid::from_u128(2).to_string(),
            "/tmp/folder".to_string(),
        )
        .await
        .expect("send folder command");

        assert_eq!(transfer_id, "folder-transfer-123");
        assert_eq!(
            transfers
                .sent_folders
                .lock()
                .expect("lock sent folders")
                .clone(),
            vec![(Uuid::from_u128(2).to_string(), PathBuf::from("/tmp/folder"))]
        );
    }

    #[tokio::test]
    async fn accept_file_dispatches_to_transfer_service() {
        let transfers = Arc::new(MockTransferService::default());
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            Arc::new(MockMessagingService::default()),
            Arc::new(MockOgMetadataService::default()),
            transfers.clone(),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        accept_file_impl(&state, "offer-1".to_string())
            .await
            .expect("accept file command");

        assert_eq!(
            transfers
                .accepted_offers
                .lock()
                .expect("lock accepted offers")
                .clone(),
            vec!["offer-1".to_string()]
        );
    }

    #[tokio::test]
    async fn reject_file_passes_reason_through() {
        let transfers = Arc::new(MockTransferService::default());
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            Arc::new(MockMessagingService::default()),
            Arc::new(MockOgMetadataService::default()),
            transfers.clone(),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        reject_file_impl(&state, "offer-2".to_string(), Some("busy".to_string()))
            .await
            .expect("reject file command");

        assert_eq!(
            transfers
                .rejected_offers
                .lock()
                .expect("lock rejected offers")
                .clone(),
            vec![("offer-2".to_string(), Some("busy".to_string()))]
        );
    }

    #[tokio::test]
    async fn cancel_transfer_dispatches_to_transfer_service() {
        let transfers = Arc::new(MockTransferService::default());
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            Arc::new(MockMessagingService::default()),
            Arc::new(MockOgMetadataService::default()),
            transfers.clone(),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        cancel_transfer_impl(&state, "transfer-9".to_string())
            .await
            .expect("cancel transfer command");

        assert_eq!(
            transfers
                .cancelled_transfers
                .lock()
                .expect("lock cancelled transfers")
                .clone(),
            vec!["transfer-9".to_string()]
        );
    }

    #[tokio::test]
    async fn accept_folder_transfer_dispatches_to_transfer_service() {
        let transfers = Arc::new(MockTransferService::default());
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            Arc::new(MockMessagingService::default()),
            Arc::new(MockOgMetadataService::default()),
            transfers.clone(),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        accept_folder_transfer_impl(&state, "folder-1".to_string(), "/tmp/downloads".to_string())
            .await
            .expect("accept folder transfer command");

        assert_eq!(
            transfers
                .accepted_folders
                .lock()
                .expect("lock accepted folders")
                .clone(),
            vec![("folder-1".to_string(), PathBuf::from("/tmp/downloads"))]
        );
    }

    #[tokio::test]
    async fn reject_folder_transfer_dispatches_to_transfer_service() {
        let transfers = Arc::new(MockTransferService::default());
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            Arc::new(MockMessagingService::default()),
            Arc::new(MockOgMetadataService::default()),
            transfers.clone(),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        reject_folder_transfer_impl(&state, "folder-1".to_string())
            .await
            .expect("reject folder transfer command");

        assert_eq!(
            transfers
                .rejected_folders
                .lock()
                .expect("lock rejected folders")
                .clone(),
            vec!["folder-1".to_string()]
        );
    }

    #[tokio::test]
    async fn cancel_folder_transfer_dispatches_to_transfer_service() {
        let transfers = Arc::new(MockTransferService::default());
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            Arc::new(MockMessagingService::default()),
            Arc::new(MockOgMetadataService::default()),
            transfers.clone(),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        cancel_folder_transfer_impl(&state, "folder-1".to_string())
            .await
            .expect("cancel folder transfer command");

        assert_eq!(
            transfers
                .cancelled_folders
                .lock()
                .expect("lock cancelled folders")
                .clone(),
            vec!["folder-1".to_string()]
        );
    }

    #[tokio::test]
    async fn resume_transfer_dispatches_to_transfer_service() {
        let transfers = Arc::new(MockTransferService::default());
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            Arc::new(MockMessagingService::default()),
            Arc::new(MockOgMetadataService::default()),
            transfers.clone(),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        let resumed = resume_transfer_impl(&state, "transfer-10".to_string())
            .await
            .expect("resume transfer command");

        assert_eq!(resumed, "resumed-transfer-10");
        assert_eq!(
            transfers
                .resumed_transfers
                .lock()
                .expect("lock resumed transfers")
                .clone(),
            vec!["transfer-10".to_string()]
        );
    }

    #[tokio::test]
    async fn retry_transfer_dispatches_to_transfer_service() {
        let transfers = Arc::new(MockTransferService::default());
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            Arc::new(MockMessagingService::default()),
            Arc::new(MockOgMetadataService::default()),
            transfers.clone(),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        let retried = retry_transfer_impl(&state, "transfer-11".to_string())
            .await
            .expect("retry transfer command");

        assert_eq!(retried, "retried-transfer-11");
        assert_eq!(
            transfers
                .retried_transfers
                .lock()
                .expect("lock retried transfers")
                .clone(),
            vec!["transfer-11".to_string()]
        );
    }

    #[tokio::test]
    async fn get_transfers_returns_payloads() {
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            Arc::new(MockMessagingService::default()),
            Arc::new(MockOgMetadataService::default()),
            Arc::new(
                MockTransferService::default()
                    .with_transfers(vec![transfer("transfer-1", "active")]),
            ),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        let transfers = get_transfers_impl(&state)
            .await
            .expect("get transfers command");

        assert_eq!(transfers, vec![transfer("transfer-1", "active")]);
    }

    #[test]
    fn transfer_payload_from_managed_includes_folder_linkage_metadata() {
        let transfer = ManagedTransfer {
            id: "transfer-1".to_string(),
            peer_id: device_id(9),
            chat_id: None,
            file_name: "report.txt".to_string(),
            local_path: PathBuf::from("/tmp/report.txt"),
            direction: TransferDirection::Send,
            status: TransferStatus::Active,
            total_bytes: 200,
            progress: Some(TransferProgress {
                transfer_id: "transfer-1".to_string(),
                bytes_sent: 50,
                total_bytes: 200,
                speed_bps: 25,
            }),
            folder_id: Some("folder-1".to_string()),
            folder_relative_path: Some("docs/report.txt".to_string()),
            resume_token: None,
        };

        let payload = transfer_payload_from_managed(&transfer);

        assert_eq!(payload.folder_id.as_deref(), Some("folder-1"));
        assert_eq!(
            payload.folder_relative_path.as_deref(),
            Some("docs/report.txt")
        );
        assert_eq!(payload.direction.as_deref(), Some("send"));
        assert_eq!(payload.progress, 0.25);
        assert_eq!(payload.speed, 25);
        assert!(!payload.resumable);
    }

    #[test]
    fn transfer_payload_from_record_includes_persisted_folder_linkage_metadata() {
        let temp = tempdir().expect("tempdir");
        let local_path = temp.path().join("report.txt");
        std::fs::write(&local_path, vec![0_u8; 64]).expect("write persisted transfer file");

        let record = TransferRecord {
            id: Uuid::from_u128(55),
            peer_id: device_id(9),
            chat_id: None,
            file_name: "report.txt".to_string(),
            local_path,
            status: TransferStatus::Completed,
            thumbnail_path: None,
            folder_id: Some("folder-1".to_string()),
            folder_relative_path: Some("docs/report.txt".to_string()),
            bytes_transferred: 0,
            resume_token: None,
        };

        let payload = transfer_payload_from_record(&record);

        assert_eq!(payload.folder_id.as_deref(), Some("folder-1"));
        assert_eq!(
            payload.folder_relative_path.as_deref(),
            Some("docs/report.txt")
        );
        assert_eq!(payload.progress, 1.0);
        assert_eq!(payload.size, 64);
        assert!(!payload.resumable);
    }

    #[test]
    fn transfer_payload_marks_failed_receive_with_resume_token_as_resumable() {
        let transfer = ManagedTransfer {
            id: "transfer-resume".to_string(),
            peer_id: device_id(9),
            chat_id: None,
            file_name: "resume.bin".to_string(),
            local_path: PathBuf::from("/tmp/resume.bin"),
            direction: TransferDirection::Receive,
            status: TransferStatus::Failed,
            total_bytes: 100,
            progress: Some(TransferProgress {
                transfer_id: "transfer-resume".to_string(),
                bytes_sent: 40,
                total_bytes: 100,
                speed_bps: 0,
            }),
            folder_id: None,
            folder_relative_path: None,
            resume_token: Some("resume-token".to_string()),
        };

        let payload = transfer_payload_from_managed(&transfer);

        assert!(payload.resumable);
        assert_eq!(payload.direction.as_deref(), Some("receive"));
        assert_eq!(payload.progress, 0.4);
    }

    #[test]
    fn get_identity_returns_device_identity() {
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            Arc::new(MockMessagingService::default()),
            Arc::new(MockOgMetadataService::default()),
            Arc::new(MockTransferService::default()),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        assert_eq!(
            get_identity_impl(&state).expect("get identity command"),
            identity()
        );
    }

    #[test]
    fn update_display_name_dispatches_to_identity_service() {
        let identities = Arc::new(MockIdentityService::default());
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            Arc::new(MockMessagingService::default()),
            Arc::new(MockOgMetadataService::default()),
            Arc::new(MockTransferService::default()),
            identities.clone(),
            Arc::new(MockSettingsService::default()),
        );

        update_display_name_impl(&state, "New Name".to_string())
            .expect("update display name command");

        assert_eq!(
            identities
                .update_names
                .lock()
                .expect("lock update names")
                .clone(),
            vec!["New Name".to_string()]
        );
    }

    #[test]
    fn update_avatar_dispatches_path_to_identity_service() {
        let identities = Arc::new(MockIdentityService::default());
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            Arc::new(MockMessagingService::default()),
            Arc::new(MockOgMetadataService::default()),
            Arc::new(MockTransferService::default()),
            identities.clone(),
            Arc::new(MockSettingsService::default()),
        );

        update_avatar_impl(&state, "/tmp/new-avatar.png".to_string())
            .expect("update avatar command");

        assert_eq!(
            identities
                .update_avatars
                .lock()
                .expect("lock update avatars")
                .clone(),
            vec!["/tmp/new-avatar.png".to_string()]
        );
    }

    #[test]
    fn fingerprint_get_own_returns_expected_shape() {
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            Arc::new(MockMessagingService::default()),
            Arc::new(MockOgMetadataService::default()),
            Arc::new(MockTransferService::default()),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        let public_key = public_key_from_base64(&identity().public_key)
            .expect("identity public key should decode");
        assert_eq!(
            get_own_fingerprint_impl(&state).expect("get own fingerprint command"),
            OwnFingerprintInfo {
                device_id: identity().device_id,
                fingerprint: fingerprint(&public_key),
            }
        );
    }

    #[test]
    fn fingerprint_get_peer_returns_stored_fingerprint_info() {
        let identities = Arc::new(MockIdentityService::default().with_peer_fingerprint(
            PeerFingerprintInfo {
                peer_id: Uuid::from_u128(2).to_string(),
                fingerprint: "AA11 BB22 CC33 DD44".to_string(),
                verified: false,
                first_seen: 1_234,
            },
        ));
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            Arc::new(MockMessagingService::default()),
            Arc::new(MockOgMetadataService::default()),
            Arc::new(MockTransferService::default()),
            identities,
            Arc::new(MockSettingsService::default()),
        );

        assert_eq!(
            get_peer_fingerprint_impl(&state, Uuid::from_u128(2).to_string())
                .expect("get peer fingerprint command"),
            PeerFingerprintInfo {
                peer_id: Uuid::from_u128(2).to_string(),
                fingerprint: "AA11 BB22 CC33 DD44".to_string(),
                verified: false,
                first_seen: 1_234,
            }
        );
    }

    #[test]
    fn fingerprint_toggle_peer_verified_persists_and_round_trips() {
        let identities = Arc::new(MockIdentityService::default().with_peer_fingerprint(
            PeerFingerprintInfo {
                peer_id: Uuid::from_u128(9).to_string(),
                fingerprint: "EE55 FF66 7788 9900".to_string(),
                verified: false,
                first_seen: 9_999,
            },
        ));
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            Arc::new(MockMessagingService::default()),
            Arc::new(MockOgMetadataService::default()),
            Arc::new(MockTransferService::default()),
            identities,
            Arc::new(MockSettingsService::default()),
        );

        toggle_peer_verified_impl(&state, Uuid::from_u128(9).to_string(), true)
            .expect("toggle peer verified command");

        assert_eq!(
            get_peer_fingerprint_impl(&state, Uuid::from_u128(9).to_string())
                .expect("reload toggled peer fingerprint"),
            PeerFingerprintInfo {
                peer_id: Uuid::from_u128(9).to_string(),
                fingerprint: "EE55 FF66 7788 9900".to_string(),
                verified: true,
                first_seen: 9_999,
            }
        );
    }

    #[test]
    fn get_settings_returns_current_settings() {
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            Arc::new(MockMessagingService::default()),
            Arc::new(MockOgMetadataService::default()),
            Arc::new(MockTransferService::default()),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        assert_eq!(
            get_settings_impl(&state).expect("get settings command"),
            settings()
        );
    }

    #[tokio::test]
    async fn update_settings_dispatches_and_returns_normalized_settings() {
        let settings_service = Arc::new(MockSettingsService::default());
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            Arc::new(MockMessagingService::default()),
            Arc::new(MockOgMetadataService::default()),
            Arc::new(MockTransferService::default()),
            Arc::new(MockIdentityService::default()),
            settings_service.clone(),
        );
        let next = AppSettings {
            download_dir: "/tmp/other".to_string(),
            max_concurrent_transfers: 5,
        };

        let saved = update_settings_impl(&state, next.clone())
            .await
            .expect("update settings command");

        assert_eq!(saved, next);
        assert_eq!(
            settings_service
                .updated_settings
                .lock()
                .expect("lock updated settings")
                .clone(),
            vec![next]
        );
    }
}

mod commands_error {
    use super::*;

    #[tokio::test]
    async fn send_message_returns_serializable_error_instead_of_panicking() {
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            Arc::new(MockMessagingService::default().with_send_error("peer offline")),
            Arc::new(MockOgMetadataService::default()),
            Arc::new(MockTransferService::default()),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        let error = send_message_impl(
            &state,
            Uuid::from_u128(2).to_string(),
            "hello".to_string(),
            None,
        )
        .await
        .expect_err("send message should fail");

        assert!(error.contains("peer offline"));
    }

    #[tokio::test]
    async fn reject_file_propagates_service_errors() {
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            Arc::new(MockMessagingService::default()),
            Arc::new(MockOgMetadataService::default()),
            Arc::new(MockTransferService::default().with_reject_error("unknown offer")),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        let error = reject_file_impl(&state, "offer-404".to_string(), None)
            .await
            .expect_err("reject file should fail");

        assert!(error.contains("unknown offer"));
    }

    #[tokio::test]
    async fn update_settings_propagates_dependency_errors() {
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            Arc::new(MockMessagingService::default()),
            Arc::new(MockOgMetadataService::default()),
            Arc::new(MockTransferService::default()),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default().with_update_error("transfer manager failed")),
        );

        let error = update_settings_impl(&state, settings())
            .await
            .expect_err("update settings should fail");

        assert!(error.contains("transfer manager failed"));
    }

    #[tokio::test]
    async fn start_discovery_propagates_start_errors() {
        let state = app_state(
            Arc::new(MockDiscoveryService::default().with_start_error("mdns unavailable")),
            Arc::new(MockMessagingService::default()),
            Arc::new(MockOgMetadataService::default()),
            Arc::new(MockTransferService::default()),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        let error = start_discovery_impl(&state)
            .await
            .expect_err("start discovery should fail");

        assert!(error.contains("mdns unavailable"));
    }
}

mod group_error_handling {
    use super::*;

    #[tokio::test]
    async fn get_group_info_propagates_unknown_group_errors() {
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            Arc::new(MockMessagingService::default()),
            Arc::new(MockOgMetadataService::default()),
            Arc::new(MockTransferService::default()),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        let error = get_group_info_impl(&state, Uuid::from_u128(404).to_string())
            .await
            .expect_err("missing group should fail");

        assert!(error.contains("unknown group"));
    }

    #[tokio::test]
    async fn remove_group_members_propagates_invalid_group_id_errors() {
        let state = app_state(
            Arc::new(MockDiscoveryService::default()),
            Arc::new(MockMessagingService::default()),
            Arc::new(MockOgMetadataService::default()),
            Arc::new(MockTransferService::default()),
            Arc::new(MockIdentityService::default()),
            Arc::new(MockSettingsService::default()),
        );

        let error = remove_group_members_impl(
            &state,
            "not-a-uuid".to_string(),
            vec![Uuid::from_u128(2).to_string()],
        )
        .await
        .expect_err("invalid group id should fail");

        assert!(error.contains("invalid group_id"));
    }
}

mod setup_init {
    use super::*;

    struct RecordingFactory {
        called: AtomicUsize,
        started_discovery: AtomicBool,
        last_context: Mutex<Option<SetupContext>>,
    }

    impl RecordingFactory {
        fn new() -> Self {
            Self {
                called: AtomicUsize::new(0),
                started_discovery: AtomicBool::new(false),
                last_context: Mutex::new(None),
            }
        }
    }

    #[async_trait]
    impl AppSetupFactory for RecordingFactory {
        async fn build(
            &self,
            context: SetupContext,
            _emitter: Arc<dyn FrontendEmitter>,
        ) -> Result<AppState, String> {
            self.called.fetch_add(1, Ordering::SeqCst);
            *self.last_context.lock().expect("lock last context") = Some(context.clone());
            std::fs::create_dir_all(&context.app_data_dir).map_err(|error| error.to_string())?;
            std::fs::write(&context.database_path, b"").map_err(|error| error.to_string())?;

            let discovery = Arc::new(MockDiscoveryService::default());
            discovery.start().await?;
            self.started_discovery.store(true, Ordering::SeqCst);

            Ok(app_state(
                discovery,
                Arc::new(MockMessagingService::default()),
                Arc::new(MockOgMetadataService::default()),
                Arc::new(MockTransferService::default()),
                Arc::new(MockIdentityService::default()),
                Arc::new(MockSettingsService::default()),
            ))
        }
    }

    #[tokio::test]
    async fn setup_init_creates_database_path_and_returns_state() {
        let dir = tempdir().expect("create temp dir");
        let factory = Arc::new(RecordingFactory::new());
        let state =
            setup_app_state_with_factory(dir.path(), Arc::new(MockEmitter), factory.clone())
                .await
                .expect("setup app state");

        let context = factory
            .last_context
            .lock()
            .expect("lock last context")
            .clone()
            .expect("setup context captured");

        assert_eq!(factory.called.load(Ordering::SeqCst), 1);
        assert_eq!(context.app_data_dir, dir.path().to_path_buf());
        assert_eq!(context.database_path, dir.path().join("jasmine.db"));
        assert!(context.database_path.exists());
        assert_eq!(state.local_device_id(), identity().device_id);
    }

    #[tokio::test]
    async fn setup_init_starts_discovery_during_factory_build() {
        let dir = tempdir().expect("create temp dir");
        let factory = Arc::new(RecordingFactory::new());

        setup_app_state_with_factory(dir.path(), Arc::new(MockEmitter), factory.clone())
            .await
            .expect("setup app state");

        assert!(factory.started_discovery.load(Ordering::SeqCst));
    }
}

mod chat_bridge {
    use super::*;
    use crate::commands::emit_chat_service_event;
    use jasmine_core::StorageEngine;
    use jasmine_messaging::ChatServiceEvent;
    use jasmine_storage::SqliteStorage;
    use serde_json::json;

    struct RecordingEmitter {
        events: Mutex<Vec<(String, serde_json::Value)>>,
    }

    impl RecordingEmitter {
        fn new() -> Self {
            Self {
                events: Mutex::new(Vec::new()),
            }
        }

        fn snapshot(&self) -> Vec<(String, serde_json::Value)> {
            self.events.lock().expect("lock emitted events").clone()
        }
    }

    impl FrontendEmitter for RecordingEmitter {
        fn emit_json(&self, event: &str, payload: serde_json::Value) -> Result<(), String> {
            self.events
                .lock()
                .expect("lock emitted events")
                .push((event.to_string(), payload));
            Ok(())
        }
    }

    #[tokio::test]
    async fn emit_chat_service_event_bridges_edit_delete_and_mention_payloads() {
        let dir = tempdir().expect("create temp dir");
        let storage =
            SqliteStorage::open(dir.path().join("commands.db")).expect("open sqlite storage");
        let emitter = RecordingEmitter::new();
        let mut stored_message = message(10, 2, 2, "before", MessageStatus::Delivered);
        stored_message.edit_version = 2;
        stored_message.edited_at = Some(7_654);
        storage
            .save_message(&stored_message)
            .await
            .expect("save bridge test message");

        emit_chat_service_event(
            ChatServiceEvent::MessageEdited {
                message_id: stored_message.id,
                new_content: "after".to_string(),
                edit_version: 2,
            },
            &storage,
            &emitter,
            &identity().device_id,
        )
        .await;
        emit_chat_service_event(
            ChatServiceEvent::MessageDeleted {
                message_id: stored_message.id,
            },
            &storage,
            &emitter,
            &identity().device_id,
        )
        .await;
        emit_chat_service_event(
            ChatServiceEvent::MentionReceived {
                message_id: stored_message.id,
                mentioned_user_id: "user-1".to_string(),
                sender_name: "Alice".to_string(),
            },
            &storage,
            &emitter,
            &identity().device_id,
        )
        .await;

        assert_eq!(
            emitter.snapshot(),
            vec![
                (
                    "message-edited".to_string(),
                    json!({
                        "messageId": stored_message.id.to_string(),
                        "newContent": "after",
                        "editVersion": 2,
                        "editedAt": 7654
                    })
                ),
                (
                    "message-deleted".to_string(),
                    json!({
                        "messageId": stored_message.id.to_string()
                    })
                ),
                (
                    "mention-received".to_string(),
                    json!({
                        "messageId": stored_message.id.to_string(),
                        "mentionedUserId": "user-1",
                        "senderName": "Alice"
                    })
                )
            ]
        );
    }

    #[tokio::test]
    async fn emit_chat_service_event_bridges_group_message_and_group_state_payloads() {
        let dir = tempdir().expect("create temp dir");
        let storage =
            SqliteStorage::open(dir.path().join("commands-group.db")).expect("open sqlite storage");
        let emitter = RecordingEmitter::new();
        let mut group_message = message(20, 77, 2, "group hello", MessageStatus::Delivered);
        group_message.reply_to_id = Some(Uuid::from_u128(21).to_string());
        group_message.reply_to_preview = Some("preview text".to_string());

        emit_chat_service_event(
            ChatServiceEvent::MessageReceived {
                peer_id: Uuid::from_u128(2).to_string(),
                message: group_message.clone(),
            },
            &storage,
            &emitter,
            &identity().device_id,
        )
        .await;
        emit_chat_service_event(
            ChatServiceEvent::GroupCreated {
                group_id: chat_id(77),
            },
            &storage,
            &emitter,
            &identity().device_id,
        )
        .await;
        emit_chat_service_event(
            ChatServiceEvent::GroupMembersAdded {
                group_id: chat_id(77),
                member_ids: vec![Uuid::from_u128(3).to_string()],
            },
            &storage,
            &emitter,
            &identity().device_id,
        )
        .await;
        emit_chat_service_event(
            ChatServiceEvent::GroupMembersRemoved {
                group_id: chat_id(77),
                member_ids: vec![Uuid::from_u128(4).to_string()],
            },
            &storage,
            &emitter,
            &identity().device_id,
        )
        .await;
        emit_chat_service_event(
            ChatServiceEvent::GroupUpdated {
                group_id: chat_id(77),
            },
            &storage,
            &emitter,
            &identity().device_id,
        )
        .await;

        assert_eq!(
            emitter.snapshot(),
            vec![
                (
                    "group-message-received".to_string(),
                    json!({
                        "groupId": chat_id(77).0.to_string(),
                        "id": group_message.id.to_string(),
                        "senderId": Uuid::from_u128(2).to_string(),
                        "senderName": null,
                        "content": "group hello",
                        "timestamp": 1234,
                        "encrypted": true,
                        "isOwn": false,
                        "status": "delivered",
                        "isDeleted": false,
                        "editedAt": null,
                        "replyToId": group_message.reply_to_id,
                        "replyToPreview": group_message.reply_to_preview,
                    })
                ),
                (
                    "group-created".to_string(),
                    json!({
                        "groupId": chat_id(77).0.to_string()
                    })
                ),
                (
                    "group-member-added".to_string(),
                    json!({
                        "groupId": chat_id(77).0.to_string(),
                        "memberIds": [Uuid::from_u128(3).to_string()]
                    })
                ),
                (
                    "group-member-removed".to_string(),
                    json!({
                        "groupId": chat_id(77).0.to_string(),
                        "memberIds": [Uuid::from_u128(4).to_string()]
                    })
                ),
                (
                    "group-updated".to_string(),
                    json!({
                        "groupId": chat_id(77).0.to_string()
                    })
                ),
            ]
        );
    }
}

mod call_bridge {
    use super::*;
    use crate::commands::bridge_call_signaling_message;
    use jasmine_core::ProtocolMessage;
    use serde_json::json;

    struct RecordingEmitter {
        events: Mutex<Vec<(String, serde_json::Value)>>,
    }

    impl RecordingEmitter {
        fn new() -> Self {
            Self {
                events: Mutex::new(Vec::new()),
            }
        }

        fn snapshot(&self) -> Vec<(String, serde_json::Value)> {
            self.events.lock().expect("lock emitted events").clone()
        }
    }

    impl FrontendEmitter for RecordingEmitter {
        fn emit_json(&self, event: &str, payload: serde_json::Value) -> Result<(), String> {
            self.events
                .lock()
                .expect("lock emitted events")
                .push((event.to_string(), payload));
            Ok(())
        }
    }

    #[test]
    fn call_signaling_bridge_emits_events_and_ignores_unknown_call_ids() {
        let emitter = RecordingEmitter::new();
        let call_bridge = CallBridgeState::default();
        let peer_id = Uuid::from_u128(2).to_string();
        let active_call_id = Uuid::from_u128(70).to_string();
        let group_call_id = Uuid::from_u128(72).to_string();
        let group_id = Uuid::from_u128(77).to_string();
        let rejected_call_id = Uuid::from_u128(71).to_string();

        assert!(!bridge_call_signaling_message(
            &peer_id,
            ProtocolMessage::CallAnswer {
                call_id: Uuid::from_u128(99).to_string(),
                sdp: "unexpected-answer".to_string(),
            },
            &emitter,
            &call_bridge,
        )
        .expect("drop unknown call answer"));

        assert!(bridge_call_signaling_message(
            &peer_id,
            ProtocolMessage::CallOffer {
                call_id: active_call_id.clone(),
                sdp: "offer-sdp".to_string(),
                caller_id: peer_id.clone(),
                call_type: CallType::Audio,
            },
            &emitter,
            &call_bridge,
        )
        .expect("bridge call offer"));
        assert!(bridge_call_signaling_message(
            &peer_id,
            ProtocolMessage::CallAnswer {
                call_id: active_call_id.clone(),
                sdp: "answer-sdp".to_string(),
            },
            &emitter,
            &call_bridge,
        )
        .expect("bridge call answer"));
        assert!(bridge_call_signaling_message(
            &peer_id,
            ProtocolMessage::IceCandidate {
                call_id: active_call_id.clone(),
                candidate: "candidate:1 1 udp 1 127.0.0.1 9000 typ host".to_string(),
                sdp_mid: Some("audio".to_string()),
                sdp_mline_index: Some(0),
            },
            &emitter,
            &call_bridge,
        )
        .expect("bridge ice candidate"));
        assert!(bridge_call_signaling_message(
            &peer_id,
            ProtocolMessage::CallHangup {
                call_id: active_call_id.clone(),
            },
            &emitter,
            &call_bridge,
        )
        .expect("bridge call hangup"));

        assert!(bridge_call_signaling_message(
            &peer_id,
            ProtocolMessage::CallOffer {
                call_id: group_call_id.clone(),
                sdp: "offer-group".to_string(),
                caller_id: peer_id.clone(),
                call_type: CallType::Video,
            },
            &emitter,
            &call_bridge,
        )
        .expect("bridge group call offer"));
        assert!(bridge_call_signaling_message(
            &peer_id,
            ProtocolMessage::CallJoin {
                call_id: group_call_id.clone(),
                group_id: group_id.clone(),
            },
            &emitter,
            &call_bridge,
        )
        .expect("bridge call join"));
        assert!(bridge_call_signaling_message(
            &peer_id,
            ProtocolMessage::CallLeave {
                call_id: group_call_id.clone(),
            },
            &emitter,
            &call_bridge,
        )
        .expect("bridge call leave"));

        assert!(bridge_call_signaling_message(
            &peer_id,
            ProtocolMessage::CallOffer {
                call_id: rejected_call_id.clone(),
                sdp: "offer-reject".to_string(),
                caller_id: peer_id.clone(),
                call_type: CallType::Video,
            },
            &emitter,
            &call_bridge,
        )
        .expect("bridge second call offer"));
        assert!(bridge_call_signaling_message(
            &peer_id,
            ProtocolMessage::CallReject {
                call_id: rejected_call_id.clone(),
                reason: Some("busy".to_string()),
            },
            &emitter,
            &call_bridge,
        )
        .expect("bridge call reject"));

        assert!(!call_bridge.is_known_call(&active_call_id));
        assert!(call_bridge.is_known_call(&group_call_id));
        assert!(call_bridge.is_group_call(&group_call_id));
        assert_eq!(
            call_bridge.group_id_for_call(&group_call_id),
            Some(group_id.clone())
        );
        assert!(!call_bridge.is_known_call(&rejected_call_id));
        assert_eq!(
            emitter.snapshot(),
            vec![
                (
                    "call-offer".to_string(),
                    json!({
                        "peerId": peer_id.clone(),
                        "callId": active_call_id.clone(),
                        "sdp": "offer-sdp",
                        "callerId": Uuid::from_u128(2).to_string(),
                        "callType": "Audio",
                    })
                ),
                (
                    "call-answer".to_string(),
                    json!({
                        "peerId": Uuid::from_u128(2).to_string(),
                        "callId": Uuid::from_u128(70).to_string(),
                        "sdp": "answer-sdp",
                    })
                ),
                (
                    "ice-candidate".to_string(),
                    json!({
                        "peerId": Uuid::from_u128(2).to_string(),
                        "callId": Uuid::from_u128(70).to_string(),
                        "candidate": "candidate:1 1 udp 1 127.0.0.1 9000 typ host",
                        "sdpMid": "audio",
                        "sdpMlineIndex": 0,
                    })
                ),
                (
                    "call-hangup".to_string(),
                    json!({
                        "peerId": Uuid::from_u128(2).to_string(),
                        "callId": Uuid::from_u128(70).to_string(),
                    })
                ),
                (
                    "call-offer".to_string(),
                    json!({
                        "peerId": Uuid::from_u128(2).to_string(),
                        "callId": Uuid::from_u128(72).to_string(),
                        "sdp": "offer-group",
                        "callerId": Uuid::from_u128(2).to_string(),
                        "callType": "Video",
                    })
                ),
                (
                    "call-join".to_string(),
                    json!({
                        "peerId": Uuid::from_u128(2).to_string(),
                        "callId": Uuid::from_u128(72).to_string(),
                        "groupId": Uuid::from_u128(77).to_string(),
                    })
                ),
                (
                    "call-leave".to_string(),
                    json!({
                        "peerId": Uuid::from_u128(2).to_string(),
                        "callId": Uuid::from_u128(72).to_string(),
                    })
                ),
                (
                    "call-offer".to_string(),
                    json!({
                        "peerId": Uuid::from_u128(2).to_string(),
                        "callId": Uuid::from_u128(71).to_string(),
                        "sdp": "offer-reject",
                        "callerId": Uuid::from_u128(2).to_string(),
                        "callType": "Video",
                    })
                ),
                (
                    "call-reject".to_string(),
                    json!({
                        "peerId": Uuid::from_u128(2).to_string(),
                        "callId": Uuid::from_u128(71).to_string(),
                        "reason": "busy",
                    })
                ),
            ]
        );
    }
}

mod transfer_bridge {
    use super::*;
    use crate::commands::FolderBridgeState;
    use jasmine_core::protocol::{FolderFileEntry, FolderManifestData, ProtocolMessage};
    use jasmine_transfer::FolderOfferNotification;

    fn folder_offer_notification(folder_id: &str, sender_id: DeviceId) -> FolderOfferNotification {
        FolderOfferNotification {
            folder_transfer_id: folder_id.to_string(),
            sender_id,
            folder_name: "project".to_string(),
            file_count: 2,
            total_size: 12,
        }
    }

    fn folder_manifest() -> FolderManifestData {
        FolderManifestData {
            folder_name: "project".to_string(),
            files: vec![
                FolderFileEntry {
                    relative_path: "docs/report.txt".to_string(),
                    size: 8,
                    sha256: "abc123".to_string(),
                },
                FolderFileEntry {
                    relative_path: "images/icon.png".to_string(),
                    size: 4,
                    sha256: "def456".to_string(),
                },
            ],
            total_size: 12,
        }
    }

    fn file_offer(id: &str, filename: &str, size: u64, sha256: &str) -> ProtocolMessage {
        ProtocolMessage::FileOffer {
            id: id.to_string(),
            filename: filename.to_string(),
            size,
            sha256: sha256.to_string(),
            transfer_port: 4040,
        }
    }

    #[test]
    fn folder_bridge_routes_only_matching_manifest_files_for_active_receive() {
        let bridge = FolderBridgeState::default();
        let sender_id = device_id(88);
        let folder_id = "folder-1";

        bridge.remember_pending_offer(
            &folder_offer_notification(folder_id, sender_id.clone()),
            &folder_manifest(),
        );
        let pending_offer = bridge
            .take_pending_offer(folder_id)
            .expect("pending folder offer exists");
        bridge.activate_receive(folder_id.to_string(), pending_offer);

        assert!(!bridge.routes_file_offer(
            &sender_id,
            &file_offer("standalone-1", "notes.txt", 8, "abc123")
        ));
        assert!(bridge.routes_file_offer(
            &sender_id,
            &file_offer("folder-1a", "report.txt", 8, "abc123")
        ));
        assert!(bridge.routes_file_offer(
            &sender_id,
            &file_offer("folder-1b", "icon.png", 4, "def456")
        ));
    }

    #[test]
    fn folder_bridge_does_not_consume_expected_folder_offer_when_standalone_offer_arrives() {
        let bridge = FolderBridgeState::default();
        let sender_id = device_id(89);
        let folder_id = "folder-2";

        bridge.remember_pending_offer(
            &folder_offer_notification(folder_id, sender_id.clone()),
            &folder_manifest(),
        );
        let pending_offer = bridge
            .take_pending_offer(folder_id)
            .expect("pending folder offer exists");
        bridge.activate_receive(folder_id.to_string(), pending_offer);

        assert!(!bridge.routes_file_offer(
            &sender_id,
            &file_offer("standalone-2", "notes.txt", 9, "fff999")
        ));
        assert!(bridge.routes_file_offer(
            &sender_id,
            &file_offer("folder-2a", "report.txt", 8, "ABC123")
        ));
    }
}
