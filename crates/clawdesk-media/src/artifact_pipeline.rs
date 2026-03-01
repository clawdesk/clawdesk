//! GAP-E: Content-addressed artifact pipeline.
//!
//! Provides a concrete `ArtifactStore` implementation backed by `MediaCache`
//! (for binary data) + an in-memory/serializable index (for metadata).
//!
//! ## Cross-channel flow
//!
//! 1. **Ingest**: Channel adapter receives media → calls `ingest_media()` →
//!    stores data in MediaCache, creates ArtifactRef with content-addressed ID
//! 2. **Resolve**: Agent runner or UI calls `get(id)` → retrieves ArtifactRef
//!    metadata + raw data from MediaCache
//! 3. **Bridge**: ACP artifacts from agent-to-agent messages → `ingest_acp()` →
//!    converted and stored uniformly
//! 4. **Evict**: Background task calls `evict_expired()` periodically

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use clawdesk_storage::artifact_store::{ArtifactStore, ArtifactStoreError, ArtifactStoreStats};
use clawdesk_types::artifact::{ArtifactData, ArtifactId, ArtifactRef};
use clawdesk_types::message::MediaAttachment;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::cache::MediaCache;

/// Serializable artifact index (metadata only — data lives in MediaCache).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ArtifactMetaIndex {
    artifacts: HashMap<ArtifactId, ArtifactRef>,
}

/// Content-addressed artifact pipeline backed by MediaCache + in-memory index.
pub struct ArtifactPipeline {
    /// File-backed content-addressed cache for binary data.
    cache: Arc<MediaCache>,
    /// In-memory metadata index (ArtifactRef keyed by content hash).
    index: RwLock<ArtifactMetaIndex>,
}

impl ArtifactPipeline {
    /// Create a new artifact pipeline wrapping an existing MediaCache.
    pub fn new(cache: Arc<MediaCache>) -> Self {
        Self {
            cache,
            index: RwLock::new(ArtifactMetaIndex::default()),
        }
    }

    /// Ingest a `MediaAttachment` from any channel, storing data and metadata.
    /// Returns the content-addressed artifact ID.
    pub async fn ingest_media(
        &self,
        media: &MediaAttachment,
        session_id: Option<&str>,
        channel_name: Option<&str>,
    ) -> Result<ArtifactId, ArtifactStoreError> {
        let data = media.data.as_deref();

        // Build artifact ref from media attachment
        let mut art = ArtifactRef::from(media);
        if let Some(sid) = session_id {
            art.owner_session = Some(sid.to_string());
        }
        if let Some(ch) = channel_name {
            art.accessed_from.push(ch.to_string());
        }

        // Store binary data in cache if available
        if let Some(bytes) = data {
            let cache_key = self
                .cache
                .put(bytes, &art.mime_type, Some(&art.name))
                .map_err(ArtifactStoreError::Io)?;

            // Update artifact to reference the cache store instead of inline data
            art.id = cache_key.clone();
            art.size_bytes = bytes.len() as u64;
            art.data = ArtifactData::StoreRef {
                store_id: cache_key,
            };
        }

        let id = art.id.clone();
        let mut idx = self.index.write().await;
        idx.artifacts.insert(id.clone(), art);
        Ok(id)
    }

    /// Ingest an ACP Artifact from an agent-to-agent message.
    pub async fn ingest_acp(
        &self,
        acp: &AcpArtifactInput,
        session_id: Option<&str>,
    ) -> Result<ArtifactId, ArtifactStoreError> {
        let (data_variant, raw_data) = match &acp.data {
            AcpDataInput::Text(text) => (
                ArtifactData::Text {
                    content: text.clone(),
                },
                Some(text.as_bytes().to_vec()),
            ),
            AcpDataInput::Base64(b64) => {
                let decoded = base64_decode(b64)?;
                let cache_key = self
                    .cache
                    .put(&decoded, &acp.mime_type, Some(&acp.name))
                    .map_err(ArtifactStoreError::Io)?;
                (
                    ArtifactData::StoreRef {
                        store_id: cache_key,
                    },
                    Some(decoded),
                )
            }
            AcpDataInput::Url(url) => (
                ArtifactData::Url {
                    url: url.clone(),
                    expires_at: None,
                },
                None,
            ),
        };

        let size = raw_data.as_ref().map(|d| d.len() as u64).unwrap_or(0);
        let id = if let Some(ref d) = raw_data {
            MediaCache::content_key(d)
        } else {
            MediaCache::content_key(acp.name.as_bytes())
        };

        let mut art = ArtifactRef {
            id: id.clone(),
            name: acp.name.clone(),
            mime_type: acp.mime_type.clone(),
            size_bytes: acp.size_bytes.unwrap_or(size),
            data: data_variant,
            created_at: Utc::now(),
            owner_session: session_id.map(String::from),
            tags: vec!["acp".to_string()],
            accessed_from: vec![],
            ttl_secs: 0,
        };

        if let Some(ref raw) = raw_data {
            if matches!(art.data, ArtifactData::Text { .. }) {
                // For text, also cache it
                let cache_key = self
                    .cache
                    .put(raw, &art.mime_type, Some(&art.name))
                    .map_err(ArtifactStoreError::Io)?;
                art.id = cache_key.clone();
            }
        }

        let final_id = art.id.clone();
        let mut idx = self.index.write().await;
        idx.artifacts.insert(final_id.clone(), art);
        Ok(final_id)
    }

    /// Record that a channel accessed an artifact (cross-channel tracking).
    pub async fn record_access(&self, id: &str, channel_name: &str) -> bool {
        let mut idx = self.index.write().await;
        if let Some(art) = idx.artifacts.get_mut(id) {
            if !art.accessed_from.contains(&channel_name.to_string()) {
                art.accessed_from.push(channel_name.to_string());
            }
            true
        } else {
            false
        }
    }

    /// Get all artifacts accessed from multiple channels (truly cross-channel).
    pub async fn cross_channel_artifacts(&self) -> Vec<ArtifactRef> {
        let idx = self.index.read().await;
        idx.artifacts
            .values()
            .filter(|a| a.accessed_from.len() > 1)
            .cloned()
            .collect()
    }

    /// Format an artifact descriptor for LLM context injection.
    ///
    /// Returns a short XML-like snippet that can be prepended to the prompt
    /// so the agent knows what artifacts are available.
    pub fn format_for_context(artifacts: &[ArtifactRef]) -> String {
        if artifacts.is_empty() {
            return String::new();
        }
        let mut out = String::from("<available_artifacts>\n");
        for art in artifacts {
            out.push_str(&format!(
                "  <artifact id=\"{}\" name=\"{}\" mime=\"{}\" size=\"{}\" />\n",
                art.id, art.name, art.mime_type, art.size_bytes
            ));
        }
        out.push_str("</available_artifacts>\n");
        out
    }
}

/// Input type for ACP artifact ingestion (mirrors ACP Artifact without coupling to that crate).
#[derive(Debug, Clone)]
pub struct AcpArtifactInput {
    pub name: String,
    pub mime_type: String,
    pub data: AcpDataInput,
    pub size_bytes: Option<u64>,
}

/// How ACP artifact data arrives.
#[derive(Debug, Clone)]
pub enum AcpDataInput {
    Text(String),
    Base64(String),
    Url(String),
}

fn base64_decode(input: &str) -> Result<Vec<u8>, ArtifactStoreError> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(input)
        .map_err(|e| ArtifactStoreError::Serde(format!("base64 decode: {e}")))
}

// ---------------------------------------------------------------------------
// ArtifactStore trait implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl ArtifactStore for ArtifactPipeline {
    async fn put(
        &self,
        artifact: &ArtifactRef,
        data: Option<&[u8]>,
    ) -> Result<ArtifactId, ArtifactStoreError> {
        // Store binary data in cache if provided
        let id = if let Some(bytes) = data {
            let cache_key = self
                .cache
                .put(bytes, &artifact.mime_type, Some(&artifact.name))
                .map_err(ArtifactStoreError::Io)?;
            cache_key
        } else {
            artifact.id.clone()
        };

        let mut stored = artifact.clone();
        stored.id = id.clone();
        if data.is_some() {
            stored.data = ArtifactData::StoreRef {
                store_id: id.clone(),
            };
        }

        let mut idx = self.index.write().await;
        idx.artifacts.insert(id.clone(), stored);
        Ok(id)
    }

    async fn get(&self, id: &str) -> Result<Option<ArtifactRef>, ArtifactStoreError> {
        let idx = self.index.read().await;
        Ok(idx.artifacts.get(id).cloned())
    }

    async fn get_data(&self, id: &str) -> Result<Option<Vec<u8>>, ArtifactStoreError> {
        let idx = self.index.read().await;
        let art = match idx.artifacts.get(id) {
            Some(a) => a.clone(),
            None => return Ok(None),
        };
        drop(idx);

        match &art.data {
            ArtifactData::StoreRef { store_id } => {
                self.cache.get(store_id).map_err(ArtifactStoreError::Io)
            }
            ArtifactData::Inline { data } => Ok(Some(data.clone())),
            ArtifactData::Text { content } => Ok(Some(content.as_bytes().to_vec())),
            ArtifactData::Url { .. } => {
                // URL artifacts don't have stored data — caller must fetch
                Ok(None)
            }
        }
    }

    async fn delete(&self, id: &str) -> Result<bool, ArtifactStoreError> {
        let mut idx = self.index.write().await;
        if let Some(art) = idx.artifacts.remove(id) {
            if let ArtifactData::StoreRef { store_id } = &art.data {
                let _ = self.cache.remove(store_id);
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }

    async fn list_by_session(&self, session_id: &str) -> Result<Vec<ArtifactRef>, ArtifactStoreError> {
        let idx = self.index.read().await;
        let mut results: Vec<ArtifactRef> = idx
            .artifacts
            .values()
            .filter(|a| a.owner_session.as_deref() == Some(session_id))
            .cloned()
            .collect();
        results.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(results)
    }

    async fn list_by_tag(&self, tag: &str) -> Result<Vec<ArtifactRef>, ArtifactStoreError> {
        let idx = self.index.read().await;
        Ok(idx
            .artifacts
            .values()
            .filter(|a| a.tags.iter().any(|t| t == tag))
            .cloned()
            .collect())
    }

    async fn list_by_time_range(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<ArtifactRef>, ArtifactStoreError> {
        let idx = self.index.read().await;
        Ok(idx
            .artifacts
            .values()
            .filter(|a| a.created_at >= from && a.created_at <= to)
            .cloned()
            .collect())
    }

    async fn stats(&self) -> Result<ArtifactStoreStats, ArtifactStoreError> {
        let idx = self.index.read().await;
        let now = Utc::now();

        let mut sessions = std::collections::HashSet::new();
        let mut total_size = 0u64;
        let mut oldest_secs: Option<u64> = None;

        for art in idx.artifacts.values() {
            total_size += art.size_bytes;
            if let Some(ref s) = art.owner_session {
                sessions.insert(s.clone());
            }
            let age = (now - art.created_at).num_seconds().max(0) as u64;
            oldest_secs = Some(oldest_secs.map_or(age, |o: u64| o.max(age)));
        }

        Ok(ArtifactStoreStats {
            total_artifacts: idx.artifacts.len(),
            total_size_bytes: total_size,
            sessions_with_artifacts: sessions.len(),
            oldest_artifact_age_secs: oldest_secs,
        })
    }

    async fn evict_expired(&self) -> Result<usize, ArtifactStoreError> {
        let mut idx = self.index.write().await;
        let before = idx.artifacts.len();
        let expired_ids: Vec<ArtifactId> = idx
            .artifacts
            .values()
            .filter(|a| a.is_expired())
            .map(|a| a.id.clone())
            .collect();

        for id in &expired_ids {
            if let Some(art) = idx.artifacts.remove(id) {
                if let ArtifactData::StoreRef { store_id } = &art.data {
                    let _ = self.cache.remove(store_id);
                }
            }
        }

        Ok(before - idx.artifacts.len())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use clawdesk_types::message::MediaType;

    fn make_pipeline() -> (ArtifactPipeline, std::path::PathBuf) {
        let tmp = std::env::temp_dir().join(format!("clawdesk-art-test-{}", fastrand::u64(..)));
        let cache = Arc::new(MediaCache::new(tmp.clone(), 100).unwrap());
        (ArtifactPipeline::new(cache), tmp)
    }

    fn cleanup(path: &std::path::Path) {
        let _ = std::fs::remove_dir_all(path);
    }

    #[tokio::test]
    async fn test_ingest_media_with_data() {
        let (pipeline, _tmp) = make_pipeline();
        let media = MediaAttachment {
            media_type: MediaType::Image,
            url: None,
            data: Some(b"fake png data".to_vec()),
            mime_type: "image/png".to_string(),
            filename: Some("photo.png".to_string()),
            size_bytes: Some(13),
        };
        let id = pipeline
            .ingest_media(&media, Some("session-1"), Some("telegram"))
            .await
            .unwrap();
        assert!(!id.is_empty());

        // Verify retrieval
        let art = pipeline.get(&id).await.unwrap().unwrap();
        assert_eq!(art.mime_type, "image/png");
        assert_eq!(art.owner_session.as_deref(), Some("session-1"));
        assert!(art.accessed_from.contains(&"telegram".to_string()));

        // Verify data retrieval
        let data = pipeline.get_data(&id).await.unwrap().unwrap();
        assert_eq!(data, b"fake png data");
    }

    #[tokio::test]
    async fn test_ingest_media_url_only() {
        let (pipeline, _tmp) = make_pipeline();
        let media = MediaAttachment {
            media_type: MediaType::Document,
            url: Some("https://example.com/doc.pdf".to_string()),
            data: None,
            mime_type: "application/pdf".to_string(),
            filename: Some("doc.pdf".to_string()),
            size_bytes: None,
        };
        let id = pipeline
            .ingest_media(&media, None, None)
            .await
            .unwrap();
        let art = pipeline.get(&id).await.unwrap().unwrap();
        assert_eq!(art.mime_type, "application/pdf");
        // URL-only artifacts have no retrievable data
        let data = pipeline.get_data(&id).await.unwrap();
        assert!(data.is_none());
    }

    #[tokio::test]
    async fn test_ingest_acp_text() {
        let (pipeline, _tmp) = make_pipeline();
        let acp = AcpArtifactInput {
            name: "result.json".to_string(),
            mime_type: "application/json".to_string(),
            data: AcpDataInput::Text(r#"{"result": 42}"#.to_string()),
            size_bytes: None,
        };
        let id = pipeline.ingest_acp(&acp, Some("sess-a2a")).await.unwrap();
        let art = pipeline.get(&id).await.unwrap().unwrap();
        assert!(art.tags.contains(&"acp".to_string()));
        assert_eq!(art.owner_session.as_deref(), Some("sess-a2a"));
    }

    #[tokio::test]
    async fn test_ingest_acp_base64() {
        let (pipeline, _tmp) = make_pipeline();
        use base64::Engine;
        let raw = b"hello binary world";
        let encoded = base64::engine::general_purpose::STANDARD.encode(raw);
        let acp = AcpArtifactInput {
            name: "blob.bin".to_string(),
            mime_type: "application/octet-stream".to_string(),
            data: AcpDataInput::Base64(encoded),
            size_bytes: None,
        };
        let id = pipeline.ingest_acp(&acp, None).await.unwrap();
        let data = pipeline.get_data(&id).await.unwrap().unwrap();
        assert_eq!(data, raw);
    }

    #[tokio::test]
    async fn test_cross_channel_tracking() {
        let (pipeline, _tmp) = make_pipeline();
        let media = MediaAttachment {
            media_type: MediaType::Image,
            url: None,
            data: Some(b"cross channel image".to_vec()),
            mime_type: "image/jpeg".to_string(),
            filename: Some("shared.jpg".to_string()),
            size_bytes: Some(19),
        };
        let id = pipeline
            .ingest_media(&media, Some("s1"), Some("telegram"))
            .await
            .unwrap();

        // Same artifact accessed from slack
        pipeline.record_access(&id, "slack").await;

        let cross = pipeline.cross_channel_artifacts().await;
        assert_eq!(cross.len(), 1);
        assert!(cross[0].accessed_from.contains(&"telegram".to_string()));
        assert!(cross[0].accessed_from.contains(&"slack".to_string()));
    }

    #[tokio::test]
    async fn test_list_by_session() {
        let (pipeline, _tmp) = make_pipeline();
        for i in 0..3 {
            let media = MediaAttachment {
                media_type: MediaType::Document,
                url: None,
                data: Some(format!("data-{i}").into_bytes()),
                mime_type: "text/plain".to_string(),
                filename: Some(format!("file-{i}.txt")),
                size_bytes: None,
            };
            pipeline
                .ingest_media(&media, Some("session-x"), None)
                .await
                .unwrap();
        }
        let listed = pipeline.list_by_session("session-x").await.unwrap();
        assert_eq!(listed.len(), 3);
    }

    #[tokio::test]
    async fn test_delete() {
        let (pipeline, _tmp) = make_pipeline();
        let media = MediaAttachment {
            media_type: MediaType::Image,
            url: None,
            data: Some(b"delete me".to_vec()),
            mime_type: "image/gif".to_string(),
            filename: Some("tmp.gif".to_string()),
            size_bytes: Some(9),
        };
        let id = pipeline.ingest_media(&media, None, None).await.unwrap();
        assert!(pipeline.get(&id).await.unwrap().is_some());
        assert!(pipeline.delete(&id).await.unwrap());
        assert!(pipeline.get(&id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_evict_expired() {
        let (pipeline, _tmp) = make_pipeline();
        let media = MediaAttachment {
            media_type: MediaType::Audio,
            url: None,
            data: Some(b"audio data".to_vec()),
            mime_type: "audio/wav".to_string(),
            filename: Some("voice.wav".to_string()),
            size_bytes: Some(10),
        };
        let id = pipeline
            .ingest_media(&media, Some("s1"), None)
            .await
            .unwrap();

        // Mark as expired
        {
            let mut idx = pipeline.index.write().await;
            if let Some(art) = idx.artifacts.get_mut(&id) {
                art.ttl_secs = 1;
                art.created_at = Utc::now() - chrono::Duration::seconds(10);
            }
        }

        let evicted = pipeline.evict_expired().await.unwrap();
        assert_eq!(evicted, 1);
        assert!(pipeline.get(&id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_stats() {
        let (pipeline, _tmp) = make_pipeline();
        for i in 0..5 {
            let media = MediaAttachment {
                media_type: MediaType::Image,
                url: None,
                data: Some(format!("img-{i}").into_bytes()),
                mime_type: "image/png".to_string(),
                filename: Some(format!("img-{i}.png")),
                size_bytes: None,
            };
            pipeline
                .ingest_media(&media, Some("s1"), None)
                .await
                .unwrap();
        }
        let stats = pipeline.stats().await.unwrap();
        assert_eq!(stats.total_artifacts, 5);
        assert_eq!(stats.sessions_with_artifacts, 1);
        assert!(stats.total_size_bytes > 0);
    }

    #[tokio::test]
    async fn test_format_for_context() {
        let artifacts = vec![
            ArtifactRef::text("a1", "readme.md", "text/markdown", "# Hello"),
            ArtifactRef::url("a2", "photo.jpg", "image/jpeg", "https://img/1.jpg"),
        ];
        let ctx = ArtifactPipeline::format_for_context(&artifacts);
        assert!(ctx.contains("<available_artifacts>"));
        assert!(ctx.contains("readme.md"));
        assert!(ctx.contains("photo.jpg"));
    }

    #[test]
    fn test_format_for_context_empty() {
        let ctx = ArtifactPipeline::format_for_context(&[]);
        assert!(ctx.is_empty());
    }
}
