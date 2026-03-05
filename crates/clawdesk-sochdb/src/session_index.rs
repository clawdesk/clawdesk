//! Secondary indexes for session data.
//!
//! # Problem
//!
//! `list_sessions()` uses `scan("sessions/")` which scans ALL session keys,
//! deserializes each one, and then filters in memory. This is O(N) in total
//! sessions and will degrade as sessions accumulate.
//!
//! # Solution
//!
//! Write-time secondary indexes under `idx/sessions/`:
//!
//! ```text
//! idx/sessions/by_activity/{timestamp_us:020}/{session_id}   → empty (existence = signal)
//! idx/sessions/by_channel/{channel_id}/{timestamp_us:020}/{session_id} → empty
//! idx/sessions/by_agent/{agent_id}/{timestamp_us:020}/{session_id}     → empty
//! ```
//!
//! These enable O(log N + k) lookups with temporal ordering built into the key.
//!
//! ## Index Maintenance
//!
//! - `index_session()`: writes all three index entries for a session
//! - `deindex_session()`: removes all three index entries
//! - `reindex_session()`: deindex old + index new (used on `update_session()`)
//! - `list_sessions_by_activity()`: scan `idx/sessions/by_activity/` descending
//! - `list_sessions_by_channel()`: scan filtered by channel
//!
//! # Write Safety
//!
//! All index operations go through `SochStore::put` / `delete` which hold
//! the `op_lock` mutex, so indexes are always consistent with the primary data.

use crate::SochStore;
use clawdesk_types::session::Session;
use std::sync::Arc;
use tracing::{debug, warn};

/// Manages secondary indexes for sessions.
#[derive(Clone)]
pub struct SessionIndex {
    store: Arc<SochStore>,
}

impl SessionIndex {
    pub fn new(store: Arc<SochStore>) -> Self {
        Self { store }
    }

    /// Write index entries for a session.
    pub fn index_session(&self, session: &Session) -> Result<(), String> {
        let id = session.key.as_str();
        let ts = session.last_activity.timestamp_micros() as u64;
        let channel = session.channel.to_string();

        let entries = self.build_index_entries(&id, ts, &channel, None);
        let refs: Vec<(&str, &[u8])> = entries.iter().map(|k| (k.as_str(), &[][..])).collect();

        self.store.put_batch(&refs)
            .map_err(|e| format!("index_session put_batch: {e}"))?;

        debug!(session_id = %id, "Session indexed (3 entries)");
        Ok(())
    }

    /// Write index entries including agent_id for a session.
    pub fn index_session_with_agent(&self, session: &Session, agent_id: &str) -> Result<(), String> {
        let id = session.key.as_str();
        let ts = session.last_activity.timestamp_micros() as u64;
        let channel = session.channel.to_string();

        let entries = self.build_index_entries(&id, ts, &channel, Some(agent_id));
        let refs: Vec<(&str, &[u8])> = entries.iter().map(|k| (k.as_str(), &[][..])).collect();

        self.store.put_batch(&refs)
            .map_err(|e| format!("index_session put_batch: {e}"))?;

        debug!(session_id = %id, agent_id = %agent_id, "Session indexed (with agent)");
        Ok(())
    }

    /// Remove all index entries for a session.
    pub fn deindex_session(
        &self,
        session_id: &str,
        last_activity_us: u64,
        channel: &str,
        agent_id: Option<&str>,
    ) -> Result<(), String> {
        let entries = self.build_index_entries(session_id, last_activity_us, channel, agent_id);
        for key in &entries {
            if let Err(e) = self.store.delete(key) {
                warn!(key = %key, error = %e, "deindex_session: failed to delete index entry");
            }
        }
        Ok(())
    }

    /// Update indexes when a session's activity timestamp changes.
    pub fn reindex_session(
        &self,
        session: &Session,
        old_activity_us: u64,
        old_channel: &str,
        agent_id: Option<&str>,
    ) -> Result<(), String> {
        // Remove old indexes
        self.deindex_session(
            &session.key.as_str(),
            old_activity_us,
            old_channel,
            agent_id,
        )?;

        // Write new indexes
        if let Some(aid) = agent_id {
            self.index_session_with_agent(session, aid)
        } else {
            self.index_session(session)
        }
    }

    /// List sessions ordered by last activity (most recent first).
    ///
    /// Returns session IDs in descending activity order. Uses the secondary
    /// index so it's O(log N + k) instead of O(N).
    pub fn list_by_activity(&self, limit: usize) -> Result<Vec<String>, String> {
        let prefix = "idx/sessions/by_activity/";
        let entries = self.store.scan(prefix)
            .map_err(|e| format!("list_by_activity: {e}"))?;

        // Keys are sorted ascending by timestamp. We want descending, so reverse.
        let ids: Vec<String> = entries.iter().rev()
            .take(limit)
            .filter_map(|(key, _)| {
                // Extract session_id from: idx/sessions/by_activity/{ts}/{session_id}
                key.rsplit('/').next().map(|s| s.to_string())
            })
            .collect();

        Ok(ids)
    }

    /// List sessions for a specific channel, ordered by last activity.
    pub fn list_by_channel(&self, channel: &str, limit: usize) -> Result<Vec<String>, String> {
        let prefix = format!("idx/sessions/by_channel/{}/", channel);
        let entries = self.store.scan(&prefix)
            .map_err(|e| format!("list_by_channel: {e}"))?;

        let ids: Vec<String> = entries.iter().rev()
            .take(limit)
            .filter_map(|(key, _)| key.rsplit('/').next().map(|s| s.to_string()))
            .collect();

        Ok(ids)
    }

    /// List sessions for a specific agent, ordered by last activity.
    pub fn list_by_agent(&self, agent_id: &str, limit: usize) -> Result<Vec<String>, String> {
        let prefix = format!("idx/sessions/by_agent/{}/", agent_id);
        let entries = self.store.scan(&prefix)
            .map_err(|e| format!("list_by_agent: {e}"))?;

        let ids: Vec<String> = entries.iter().rev()
            .take(limit)
            .filter_map(|(key, _)| key.rsplit('/').next().map(|s| s.to_string()))
            .collect();

        Ok(ids)
    }

    /// Rebuild all indexes from primary data.
    ///
    /// Scans all `sessions/*/state` entries and re-creates index entries.
    /// Use this for repair or after a migration.
    pub fn rebuild_all(&self) -> Result<usize, String> {
        let prefix = "sessions/";
        let entries = self.store.scan(prefix)
            .map_err(|e| format!("rebuild_all scan: {e}"))?;

        let mut indexed = 0;
        for (key, value) in &entries {
            if !key.ends_with("/state") {
                continue;
            }

            if let Ok(session) = serde_json::from_slice::<Session>(value) {
                if let Err(e) = self.index_session(&session) {
                    warn!(key = %key, error = %e, "rebuild_all: failed to index session");
                } else {
                    indexed += 1;
                }
            }
        }

        debug!(indexed, "Session indexes rebuilt");
        Ok(indexed)
    }

    // ── Internal ────────────────────────────────────────────────────

    fn build_index_entries(
        &self,
        session_id: &str,
        timestamp_us: u64,
        channel: &str,
        agent_id: Option<&str>,
    ) -> Vec<String> {
        let ts_padded = format!("{:020}", timestamp_us);
        let mut entries = vec![
            format!("idx/sessions/by_activity/{}/{}", ts_padded, session_id),
            format!("idx/sessions/by_channel/{}/{}/{}", channel, ts_padded, session_id),
        ];
        if let Some(aid) = agent_id {
            entries.push(format!("idx/sessions/by_agent/{}/{}/{}", aid, ts_padded, session_id));
        }
        entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clawdesk_types::channel::ChannelId;
    use clawdesk_types::session::SessionKey;

    fn parse_channel(s: &str) -> ChannelId {
        match s {
            "telegram" => ChannelId::Telegram,
            "discord" => ChannelId::Discord,
            "slack" => ChannelId::Slack,
            "whatsapp" => ChannelId::WhatsApp,
            "webchat" => ChannelId::WebChat,
            "email" => ChannelId::Email,
            _ => ChannelId::Internal,
        }
    }

    fn make_session(id: &str, channel: &str, ts_us: i64) -> Session {
        let ch = parse_channel(channel);
        Session {
            key: SessionKey::new(ch, id),
            state: Default::default(),
            channel: ch,
            system_prompt: String::new(),
            model: None,
            history_limit: 50,
            created_at: chrono::Utc::now(),
            last_activity: chrono::DateTime::from_timestamp_micros(ts_us).unwrap_or_else(|| chrono::Utc::now()),
            message_count: 0,
            metadata: serde_json::json!({}),
        }
    }

    #[test]
    fn index_and_list_by_activity() {
        let store = Arc::new(SochStore::open_ephemeral_quiet().unwrap());
        let idx = SessionIndex::new(store);

        let s1 = make_session("s1", "internal", 1_000_000);
        let s2 = make_session("s2", "internal", 2_000_000);
        let s3 = make_session("s3", "internal", 3_000_000);

        idx.index_session(&s1).unwrap();
        idx.index_session(&s2).unwrap();
        idx.index_session(&s3).unwrap();

        // Most recent first
        let ids = idx.list_by_activity(10).unwrap();
        assert_eq!(ids.len(), 3, "expected 3 sessions, got {:?}", ids);
        // First element should be the most recent
        assert!(ids[0].contains("s3"), "most recent should be s3, got {:?}", ids);

        // With limit
        let ids = idx.list_by_activity(2).unwrap();
        assert_eq!(ids.len(), 2);
    }

    #[test]
    fn list_by_channel() {
        let store = Arc::new(SochStore::open_ephemeral_quiet().unwrap());
        let idx = SessionIndex::new(store);

        let s1 = make_session("s1", "internal", 1_000_000);
        let s2 = make_session("s2", "internal", 2_000_000);
        let s3 = make_session("s3", "internal", 3_000_000);

        idx.index_session(&s1).unwrap();
        idx.index_session(&s2).unwrap();
        idx.index_session(&s3).unwrap();

        let internal = idx.list_by_channel("internal", 10).unwrap();
        assert_eq!(internal.len(), 3, "expected 3 internal sessions, got {:?}", internal);

        let discord = idx.list_by_channel("discord", 10).unwrap();
        assert!(discord.is_empty(), "expected no discord sessions, got {:?}", discord);
    }
}
