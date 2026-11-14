use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Multipart, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use tokio::sync::RwLock;

use crate::sse::AppState;

const STAGING_TTL: Duration = Duration::from_secs(300); // 5 minutes
const MAX_STAGING_SIZE: usize = 50 * 1024 * 1024; // 50MB

pub struct StagingEntry {
    pub bytes: Vec<u8>,
    pub filename: String,
    pub mime_type: String,
    pub created_at: Instant,
    pub owner_hash: String,
}

pub type StagingStore = Arc<RwLock<HashMap<String, StagingEntry>>>;

pub fn new_staging_store() -> StagingStore {
    Arc::new(RwLock::new(HashMap::new()))
}

pub async fn consume_staged(
    store: &StagingStore,
    id: &str,
    owner_hash: &str,
) -> Option<StagingEntry> {
    let mut map = store.write().await;
    let entry = map.get(id)?;
    if entry.owner_hash != owner_hash
        || entry.bytes.is_empty()
        || entry.created_at.elapsed() >= STAGING_TTL
    {
        return None;
    }
    map.remove(id)
}

/// Memory limits for staged uploads. A slot is a bearer URL anyone holding it
/// can POST to, so both the slots an owner may open and the bytes held in
/// RAM are capped.
pub struct StagingQuota {
    pub max_total_bytes: usize,
    pub max_slots_per_owner: usize,
}

const DEFAULT_MAX_TOTAL_MB: usize = 256;
const DEFAULT_MAX_SLOTS_PER_OWNER: usize = 5;

fn parse_positive(raw: Option<&str>, default: usize) -> usize {
    raw.and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(default)
}

impl StagingQuota {
    pub fn from_env() -> Self {
        let mb = parse_positive(
            std::env::var("BSMCP_STAGING_MAX_TOTAL_MB").ok().as_deref(),
            DEFAULT_MAX_TOTAL_MB,
        );
        Self {
            max_total_bytes: mb.saturating_mul(1024 * 1024),
            max_slots_per_owner: parse_positive(
                std::env::var("BSMCP_STAGING_MAX_SLOTS_PER_OWNER")
                    .ok()
                    .as_deref(),
                DEFAULT_MAX_SLOTS_PER_OWNER,
            ),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum StoreError {
    /// Unknown, expired, already filled or consumed slot.
    Gone,
    /// Keeping this file would exceed the total byte cap.
    OverQuota,
}

pub fn used_bytes(map: &HashMap<String, StagingEntry>) -> usize {
    map.values().map(|e| e.bytes.len()).sum()
}

/// Bytes still available under the cap, for bounding an in-progress read.
pub async fn remaining_bytes(store: &StagingStore, quota: &StagingQuota) -> usize {
    let map = store.read().await;
    quota.max_total_bytes.saturating_sub(used_bytes(&map))
}

/// Open a new empty slot unless its owner already holds the maximum number of
/// live slots. Check and insert happen under one lock.
pub async fn register_slot(
    store: &StagingStore,
    id: String,
    entry: StagingEntry,
    quota: &StagingQuota,
) -> Result<(), String> {
    let mut map = store.write().await;
    map.retain(|_, e| e.created_at.elapsed() < STAGING_TTL);
    let open = map
        .values()
        .filter(|e| e.owner_hash == entry.owner_hash)
        .count();
    if open >= quota.max_slots_per_owner {
        return Err(format!(
            "Too many pending uploads ({}): upload or let existing staging slots expire (5 minutes) before preparing another",
            quota.max_slots_per_owner
        ));
    }
    map.insert(id, entry);
    Ok(())
}

async fn try_store(
    store: &StagingStore,
    id: &str,
    bytes: Vec<u8>,
    filename: String,
    mime_type: String,
    quota: &StagingQuota,
) -> Result<(), StoreError> {
    let mut map = store.write().await;
    map.retain(|_, e| e.created_at.elapsed() < STAGING_TTL);
    let used = used_bytes(&map);
    let Some(entry) = map.get_mut(id) else {
        return Err(StoreError::Gone);
    };
    if !entry.bytes.is_empty() {
        return Err(StoreError::Gone);
    }
    if used.saturating_add(bytes.len()) > quota.max_total_bytes {
        return Err(StoreError::OverQuota);
    }
    entry.bytes = bytes;
    entry.filename = filename;
    entry.mime_type = mime_type;
    Ok(())
}

pub fn cleanup_expired_sync(store: &StagingStore) {
    if let Ok(mut map) = store.try_write() {
        let before = map.len();
        map.retain(|_, entry| entry.created_at.elapsed() < STAGING_TTL);
        let removed = before - map.len();
        if removed > 0 {
            tracing::debug!(removed, "staging_cleanup_expired");
        }
    }
}

pub async fn handle_stage_upload(
    State(state): State<AppState>,
    Path(staging_id): Path<String>,
    mut multipart: Multipart,
) -> Response {
    // Auth is the staging_id itself — only someone who called prepare_upload
    // (which requires MCP auth) has the UUID. Validate it exists in the store.
    {
        let store = state.staging.read().await;
        if !store.contains_key(&staging_id) {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Unknown staging_id — call prepare_upload first"})),
            )
                .into_response();
        }
    }

    // Extract the file from multipart
    let field = match multipart.next_field().await {
        Ok(Some(field)) => field,
        Ok(None) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "No file field in multipart body"})),
            )
                .into_response();
        }
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("Multipart parse error: {e}")})),
            )
                .into_response();
        }
    };

    let filename = field.file_name().unwrap_or("upload").to_string();
    let mime_type = field
        .content_type()
        .unwrap_or("application/octet-stream")
        .to_string();

    // Read in chunks and stop as soon as the file would exceed the per-file
    // limit or the memory left under the total cap, instead of buffering a
    // whole multipart body first.
    // shortcut: bytes of uploads still in flight are not reserved, so
    // concurrent uploads can briefly exceed the total cap by up to one file
    // per open slot; the final check in `try_store` keeps only what fits.
    let quota = StagingQuota::from_env();
    let remaining = remaining_bytes(&state.staging, &quota).await;
    let limit = remaining.min(MAX_STAGING_SIZE);
    let mut field = field;
    let mut bytes: Vec<u8> = Vec::new();
    loop {
        match field.chunk().await {
            Ok(Some(chunk)) => {
                if bytes.len() + chunk.len() > limit {
                    return if limit < MAX_STAGING_SIZE {
                        (
                            StatusCode::INSUFFICIENT_STORAGE,
                            Json(
                                json!({"error": "Staging memory is full; retry in a few minutes"}),
                            ),
                        )
                            .into_response()
                    } else {
                        (
                            StatusCode::PAYLOAD_TOO_LARGE,
                            Json(json!({"error": format!("File exceeds maximum size of {}MB", MAX_STAGING_SIZE / 1024 / 1024)})),
                        )
                            .into_response()
                    };
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": format!("Failed to read file: {e}")})),
                )
                    .into_response();
            }
        }
    }

    let size = bytes.len();

    // Replace the pre-registered slot with actual file data
    match try_store(
        &state.staging,
        &staging_id,
        bytes,
        filename.clone(),
        mime_type.clone(),
        &quota,
    )
    .await
    {
        Ok(()) => {}
        Err(StoreError::OverQuota) => {
            return (
                StatusCode::INSUFFICIENT_STORAGE,
                Json(json!({"error": "Staging memory is full; retry in a few minutes"})),
            )
                .into_response();
        }
        Err(StoreError::Gone) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Staging slot expired, filled, or consumed"})),
            )
                .into_response();
        }
    }

    tracing::info!(
        staging_id = %staging_id,
        filename = %filename,
        mime_type = %mime_type,
        size,
        "staging_file_stored"
    );

    (
        StatusCode::OK,
        Json(json!({
            "staging_id": staging_id,
            "filename": filename,
            "mime_type": mime_type,
            "size": size,
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn only_creator_can_consume_filled_slot() {
        let store = new_staging_store();
        store.write().await.insert(
            "slot".into(),
            StagingEntry {
                bytes: vec![1],
                filename: "file".into(),
                mime_type: "text/plain".into(),
                created_at: Instant::now(),
                owner_hash: "first".into(),
            },
        );
        assert!(consume_staged(&store, "slot", "second").await.is_none());
        assert_eq!(
            consume_staged(&store, "slot", "first").await.unwrap().bytes,
            vec![1]
        );
    }

    async fn store_staged(
        store: &StagingStore,
        id: &str,
        bytes: Vec<u8>,
        filename: String,
        mime_type: String,
    ) -> bool {
        let quota = StagingQuota {
            max_total_bytes: usize::MAX,
            max_slots_per_owner: usize::MAX,
        };
        try_store(store, id, bytes, filename, mime_type, &quota)
            .await
            .is_ok()
    }

    fn empty_slot(owner: &str) -> StagingEntry {
        StagingEntry {
            bytes: Vec::new(),
            filename: String::new(),
            mime_type: String::new(),
            created_at: Instant::now(),
            owner_hash: owner.into(),
        }
    }

    #[tokio::test]
    async fn an_owner_at_the_slot_cap_is_refused_and_others_are_not() {
        let store = new_staging_store();
        let quota = StagingQuota {
            max_total_bytes: 1000,
            max_slots_per_owner: 2,
        };
        assert!(
            register_slot(&store, "a1".into(), empty_slot("alice"), &quota)
                .await
                .is_ok()
        );
        assert!(
            register_slot(&store, "a2".into(), empty_slot("alice"), &quota)
                .await
                .is_ok()
        );
        let err = register_slot(&store, "a3".into(), empty_slot("alice"), &quota)
            .await
            .unwrap_err();
        assert!(err.contains("2"), "{err}");
        assert!(
            register_slot(&store, "b1".into(), empty_slot("bob"), &quota)
                .await
                .is_ok()
        );
        assert_eq!(store.read().await.len(), 3);
    }

    #[tokio::test]
    async fn consumed_or_expired_slots_free_the_owner_quota() {
        let store = new_staging_store();
        let quota = StagingQuota {
            max_total_bytes: 1000,
            max_slots_per_owner: 1,
        };
        register_slot(&store, "s1".into(), empty_slot("alice"), &quota)
            .await
            .unwrap();
        assert!(
            register_slot(&store, "s2".into(), empty_slot("alice"), &quota)
                .await
                .is_err()
        );
        assert!(store_staged(&store, "s1", vec![1], "f".into(), "text/plain".into()).await);
        consume_staged(&store, "s1", "alice").await.unwrap();
        assert!(
            register_slot(&store, "s2".into(), empty_slot("alice"), &quota)
                .await
                .is_ok()
        );
        // an expired slot no longer counts either
        store.write().await.get_mut("s2").unwrap().created_at =
            Instant::now() - STAGING_TTL - Duration::from_secs(1);
        assert!(
            register_slot(&store, "s3".into(), empty_slot("alice"), &quota)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn upload_over_the_total_byte_cap_is_refused_and_the_store_is_unchanged() {
        let store = new_staging_store();
        let quota = StagingQuota {
            max_total_bytes: 10,
            max_slots_per_owner: 5,
        };
        register_slot(&store, "s1".into(), empty_slot("alice"), &quota)
            .await
            .unwrap();
        register_slot(&store, "s2".into(), empty_slot("bob"), &quota)
            .await
            .unwrap();
        assert!(
            try_store(&store, "s1", vec![0; 8], "f".into(), "t".into(), &quota)
                .await
                .is_ok()
        );
        let err = try_store(&store, "s2", vec![0; 3], "f".into(), "t".into(), &quota)
            .await
            .unwrap_err();
        assert_eq!(err, StoreError::OverQuota);
        let map = store.read().await;
        assert!(map["s2"].bytes.is_empty(), "refused file must not be kept");
        assert_eq!(used_bytes(&map), 8);
    }

    #[test]
    fn quota_env_values_fall_back_when_missing_zero_or_garbage() {
        assert_eq!(parse_positive(Some("512"), 256), 512);
        assert_eq!(parse_positive(Some(" 7 "), 5), 7);
        assert_eq!(parse_positive(Some("0"), 256), 256);
        assert_eq!(parse_positive(Some("lots"), 5), 5);
        assert_eq!(parse_positive(None, 5), 5);
    }

    #[tokio::test]
    async fn completed_upload_cannot_recreate_consumed_slot() {
        let store = new_staging_store();
        store.write().await.insert(
            "slot".into(),
            StagingEntry {
                bytes: Vec::new(),
                filename: String::new(),
                mime_type: String::new(),
                created_at: Instant::now(),
                owner_hash: "first".into(),
            },
        );
        assert!(store_staged(&store, "slot", vec![1], "file".into(), "text/plain".into()).await);
        assert!(consume_staged(&store, "slot", "first").await.is_some());
        assert!(!store_staged(&store, "slot", vec![2], "file".into(), "text/plain".into()).await);
        assert!(!store.read().await.contains_key("slot"));
    }
}
