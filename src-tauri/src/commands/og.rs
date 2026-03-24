use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use jasmine_core::OgMetadata;
use serde::Serialize;
use serde_json::to_value;
use tauri::State;
use tracing::warn;

use super::types::{AppState, FrontendEmitter, OgMetadataServiceHandle, TauriEmitter};

const OG_CACHE_TTL_SECONDS: u64 = 86_400;
const OG_UPDATED_EVENT: &str = "og:updated";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct OgUpdatedPayload {
    pub(crate) url: String,
    pub(crate) metadata: OgMetadata,
}

#[tauri::command]
pub async fn fetch_og_metadata(
    app_handle: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    url: String,
) -> Result<OgMetadata, String> {
    let emitter: Arc<dyn FrontendEmitter> = Arc::new(TauriEmitter::new(app_handle));
    fetch_og_metadata_impl(state.inner().as_ref(), emitter, url).await
}

pub(crate) async fn fetch_og_metadata_impl(
    state: &AppState,
    emitter: Arc<dyn FrontendEmitter>,
    url: String,
) -> Result<OgMetadata, String> {
    fetch_og_metadata_with_cache_policy(Arc::clone(&state.og_metadata), emitter, url).await
}

pub(crate) async fn fetch_og_metadata_with_cache_policy(
    service: Arc<dyn OgMetadataServiceHandle>,
    emitter: Arc<dyn FrontendEmitter>,
    url: String,
) -> Result<OgMetadata, String> {
    if !is_valid_og_url(&url) {
        return Ok(OgMetadata::empty(url));
    }

    if let Some(cached) = service.get_cached_og_metadata(&url).await? {
        let cached_metadata = cached.metadata().clone();
        if is_cache_fresh(cached.fetched_at_ms, cached.ttl_seconds) {
            return Ok(cached_metadata);
        }

        spawn_stale_cache_refresh(service, emitter, url);
        return Ok(cached_metadata);
    }

    let metadata = service.fetch_remote_og_metadata(&url).await?;
    service
        .save_og_metadata(&metadata, OG_CACHE_TTL_SECONDS)
        .await?;
    Ok(metadata)
}

fn spawn_stale_cache_refresh(
    service: Arc<dyn OgMetadataServiceHandle>,
    emitter: Arc<dyn FrontendEmitter>,
    url: String,
) {
    tauri::async_runtime::spawn(async move {
        if let Err(error) = refresh_stale_og_metadata(service, emitter, url.clone()).await {
            warn!(url = url, error = %error, "OG stale cache refresh failed");
        }
    });
}

async fn refresh_stale_og_metadata(
    service: Arc<dyn OgMetadataServiceHandle>,
    emitter: Arc<dyn FrontendEmitter>,
    url: String,
) -> Result<(), String> {
    let metadata = service.fetch_remote_og_metadata(&url).await?;
    service
        .save_og_metadata(&metadata, OG_CACHE_TTL_SECONDS)
        .await?;
    emit_og_updated(emitter.as_ref(), &url, metadata)
}

fn emit_og_updated(
    emitter: &dyn FrontendEmitter,
    url: &str,
    metadata: OgMetadata,
) -> Result<(), String> {
    let payload = to_value(OgUpdatedPayload {
        url: url.to_string(),
        metadata,
    })
    .map_err(|error| error.to_string())?;

    emitter.emit_json(OG_UPDATED_EVENT, payload)
}

fn is_cache_fresh(fetched_at_ms: i64, ttl_seconds: i64) -> bool {
    match ttl_seconds.checked_mul(1_000) {
        Some(ttl_ms) => fetched_at_ms
            .checked_add(ttl_ms)
            .is_some_and(|expires_at| expires_at > now_ms()),
        None => false,
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn is_valid_og_url(url: &str) -> bool {
    let trimmed = url.trim();

    trimmed == url
        && !trimmed.is_empty()
        && (trimmed.starts_with("http://") || trimmed.starts_with("https://"))
        && trimmed
            .split_once("://")
            .is_some_and(|(_, remainder)| !remainder.is_empty())
        && !trimmed.chars().any(char::is_whitespace)
}
