mod bridge;
pub mod calls;
pub mod discovery;
pub mod environment;
pub mod identity;
pub mod messaging;
pub mod og;
pub mod setup;
pub mod transfers;
pub mod types;

pub use calls::*;
pub use discovery::*;
pub use environment::*;
pub use identity::*;
pub use messaging::*;
pub use og::*;
pub use setup::*;
pub use transfers::*;
pub use types::*;

#[allow(unused_imports)]
pub(crate) use environment::{
    check_call_support_impl, current_webview_backend, get_webrtc_platform_info_impl,
};

#[allow(unused_imports)]
pub(crate) use bridge::{
    bridge_call_signaling_message, emit_chat_service_event, CallBridgeState, FolderBridgeState,
};

#[allow(unused_imports)]
pub(crate) use calls::{
    send_call_answer_impl, send_call_hangup_impl, send_call_join_impl, send_call_leave_impl,
    send_call_offer_impl, send_call_reject_impl, send_ice_candidate_impl,
};

#[allow(unused_imports)]
pub(crate) use discovery::{get_peers_impl, start_discovery_impl, stop_discovery_impl};

#[allow(unused_imports)]
pub(crate) use identity::{
    get_identity_impl, get_own_fingerprint_impl, get_peer_fingerprint_impl, get_settings_impl,
    toggle_peer_verified_impl, update_avatar_impl, update_display_name_impl, update_settings_impl,
};

#[allow(unused_imports)]
pub(crate) use messaging::{
    create_group_impl, delete_message_impl, edit_message_impl, get_messages_impl,
    get_reply_count_impl, get_reply_counts_impl, send_group_message_impl, send_message_impl,
};

#[allow(unused_imports)]
pub(crate) use og::{fetch_og_metadata_impl, fetch_og_metadata_with_cache_policy};

#[allow(unused_imports)]
pub(crate) use transfers::{
    accept_file_impl, accept_folder_transfer_impl, cancel_folder_transfer_impl,
    cancel_transfer_impl, get_transfers_impl, reject_file_impl, reject_folder_transfer_impl,
    resume_transfer_impl, retry_transfer_impl, send_file_impl, send_folder_impl,
    transfer_payload_from_managed, transfer_payload_from_record,
};

#[cfg(test)]
mod tests;
