//! Key encoding for the thread store.
//!
//! All keys use `/`-delimited string paths with zero-padded numeric fields
//! so that SochDB prefix scans yield lexicographically sorted results.

use uuid::Uuid;

// ── Key prefixes ────────────────────────────────────────────────────────────

pub const THREAD_PREFIX: &str = "threads";
pub const MSG_PREFIX: &str = "msgs";
pub const ATTACHMENT_PREFIX: &str = "attachments";
pub const IDX_AGENT_PREFIX: &str = "idx/agent";
pub const IDX_THREAD_AGENT_PREFIX: &str = "idx/thread_agent";
pub const META_PREFIX: &str = "meta";

// ── Thread keys ─────────────────────────────────────────────────────────────

/// Primary thread metadata key.
/// Format: `threads/{thread_id:032x}`
#[inline]
pub fn thread_key(thread_id: u128) -> String {
    format!("{}/{:032x}", THREAD_PREFIX, thread_id)
}

/// Scan prefix for all threads.
#[inline]
pub fn all_threads_prefix() -> &'static str {
    "threads/"
}

// ── Message keys ────────────────────────────────────────────────────────────

/// Primary message key inside a thread namespace.
/// Format: `msgs/{thread_id:032x}/{timestamp_us:020}/{msg_id:032x}`
///
/// The zero-padded timestamp ensures that a prefix scan over
/// `msgs/{thread_id}/` returns messages in chronological order.
#[inline]
pub fn message_key(thread_id: u128, timestamp_us: u64, msg_id: u128) -> String {
    format!(
        "{}/{:032x}/{:020}/{:032x}",
        MSG_PREFIX, thread_id, timestamp_us, msg_id,
    )
}

/// Scan prefix for all messages in a thread (the thread's "namespace").
/// Format: `msgs/{thread_id:032x}/`
#[inline]
pub fn thread_messages_prefix(thread_id: u128) -> String {
    format!("{}/{:032x}/", MSG_PREFIX, thread_id)
}

/// Range-scan boundaries for messages within a time window.
/// Returns `(start_key, end_key)` suitable for `scan_range()`.
pub fn thread_messages_time_range(
    thread_id: u128,
    from_us: u64,
    to_us: u64,
) -> (String, String) {
    let start = format!("{}/{:032x}/{:020}/", MSG_PREFIX, thread_id, from_us);
    let end = format!("{}/{:032x}/{:020}/", MSG_PREFIX, thread_id, to_us);
    (start, end)
}

// ── Attachment keys ─────────────────────────────────────────────────────────

/// Attachment blob key (optional large payload stored separately from the
/// message, same pattern as agentreplay `payloads/{edge_id}`).
#[inline]
pub fn attachment_key(msg_id: u128) -> String {
    format!("{}/{:032x}", ATTACHMENT_PREFIX, msg_id)
}

// ── Secondary index keys ────────────────────────────────────────────────────

/// Agent → threads index (sorted by `updated_at` descending via zero-padded ts).
/// Format: `idx/agent/{agent_id}/{updated_us:020}/{thread_id:032x}`
///
/// Value: empty (`&[]`) — existence is the signal.
#[inline]
pub fn idx_agent_thread(agent_id: &str, updated_us: u64, thread_id: u128) -> String {
    format!(
        "{}/{}/{:020}/{:032x}",
        IDX_AGENT_PREFIX, agent_id, updated_us, thread_id,
    )
}

/// Scan prefix for all threads belonging to an agent.
#[inline]
pub fn idx_agent_prefix(agent_id: &str) -> String {
    format!("{}/{}/", IDX_AGENT_PREFIX, agent_id)
}

/// Reverse index: thread → agent (for fast agent lookup from a thread).
/// Format: `idx/thread_agent/{thread_id:032x}`
/// Value: agent_id string bytes.
#[inline]
pub fn idx_thread_agent(thread_id: u128) -> String {
    format!("{}/{:032x}", IDX_THREAD_AGENT_PREFIX, thread_id)
}

// ── Metadata keys ───────────────────────────────────────────────────────────

pub const META_THREAD_COUNT: &str = "meta/thread_count";
pub const META_MSG_COUNT: &str = "meta/msg_count";

// ── Utilities ───────────────────────────────────────────────────────────────

/// Convert a UUID string to a u128 for key encoding.
pub fn uuid_to_u128(id: &str) -> Option<u128> {
    Uuid::parse_str(id).ok().map(|u| u.as_u128())
}

/// Convert a u128 back to a UUID string.
pub fn u128_to_uuid(id: u128) -> String {
    Uuid::from_u128(id).to_string()
}

/// Generate a new random u128 id.
pub fn new_id() -> u128 {
    Uuid::new_v4().as_u128()
}

/// Current timestamp in microseconds since Unix epoch.
pub fn now_us() -> u64 {
    chrono::Utc::now().timestamp_micros() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_key_roundtrip() {
        let id: u128 = 0xdeadbeef_cafebabe_12345678_9abcdef0;
        let key = thread_key(id);
        assert!(key.starts_with("threads/"));
        assert_eq!(key.len(), "threads/".len() + 32);
    }

    #[test]
    fn message_keys_are_ordered() {
        let tid: u128 = 1;
        let k1 = message_key(tid, 1000, 1);
        let k2 = message_key(tid, 2000, 2);
        assert!(k1 < k2, "earlier timestamp must sort first");
    }

    #[test]
    fn uuid_roundtrip() {
        let id = new_id();
        let s = u128_to_uuid(id);
        assert_eq!(uuid_to_u128(&s), Some(id));
    }
}
