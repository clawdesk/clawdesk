//! Discord guild management and moderation actions.
//!
//! Provides agent-accessible methods for server administration:
//! - Guild: member info, role management, channel CRUD, emoji, events
//! - Moderation: timeout, kick, ban (with confirmation safety net)
//!
//! All actions go through the Discord REST API v10.

use reqwest::Client;
use serde::Serialize;
use tracing::{debug, info, warn};

const DISCORD_API_BASE: &str = "https://discord.com/api/v10";

/// Discord guild and moderation action dispatcher.
pub struct GuildActions {
    client: Client,
    bot_token: String,
}

/// Result of a moderation action that requires confirmation.
#[derive(Debug, Clone, Serialize)]
pub struct ModerationPending {
    pub action: String,
    pub target_user_id: String,
    pub guild_id: String,
    pub reason: Option<String>,
    pub expires_at_epoch_secs: u64,
}

impl GuildActions {
    pub fn new(client: Client, bot_token: String) -> Self {
        Self { client, bot_token }
    }

    fn auth_header(&self) -> String {
        format!("Bot {}", self.bot_token)
    }

    // ── Guild Information ────────────────────────────────────

    /// Get information about a guild member.
    pub async fn member_info(
        &self,
        guild_id: &str,
        user_id: &str,
    ) -> Result<serde_json::Value, String> {
        self.get(&format!(
            "{DISCORD_API_BASE}/guilds/{guild_id}/members/{user_id}"
        ))
        .await
    }

    /// List roles in a guild.
    pub async fn list_roles(&self, guild_id: &str) -> Result<serde_json::Value, String> {
        self.get(&format!("{DISCORD_API_BASE}/guilds/{guild_id}/roles")).await
    }

    /// Add a role to a member.
    pub async fn role_add(
        &self,
        guild_id: &str,
        user_id: &str,
        role_id: &str,
    ) -> Result<(), String> {
        self.put_empty(&format!(
            "{DISCORD_API_BASE}/guilds/{guild_id}/members/{user_id}/roles/{role_id}"
        ))
        .await
    }

    /// Remove a role from a member.
    pub async fn role_remove(
        &self,
        guild_id: &str,
        user_id: &str,
        role_id: &str,
    ) -> Result<(), String> {
        self.delete(&format!(
            "{DISCORD_API_BASE}/guilds/{guild_id}/members/{user_id}/roles/{role_id}"
        ))
        .await
    }

    // ── Channel Management ───────────────────────────────────

    /// List channels in a guild.
    pub async fn list_channels(&self, guild_id: &str) -> Result<serde_json::Value, String> {
        self.get(&format!("{DISCORD_API_BASE}/guilds/{guild_id}/channels")).await
    }

    /// Get channel info.
    pub async fn channel_info(&self, channel_id: &str) -> Result<serde_json::Value, String> {
        self.get(&format!("{DISCORD_API_BASE}/channels/{channel_id}")).await
    }

    /// Create a text channel.
    pub async fn create_channel(
        &self,
        guild_id: &str,
        name: &str,
        channel_type: u8, // 0 = text, 2 = voice, 4 = category
        parent_id: Option<&str>,
        topic: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        let mut body = serde_json::json!({
            "name": name,
            "type": channel_type,
        });
        if let Some(p) = parent_id {
            body["parent_id"] = serde_json::json!(p);
        }
        if let Some(t) = topic {
            body["topic"] = serde_json::json!(t);
        }

        self.post_json(
            &format!("{DISCORD_API_BASE}/guilds/{guild_id}/channels"),
            &body,
        )
        .await
    }

    /// Delete a channel.
    pub async fn delete_channel(&self, channel_id: &str) -> Result<(), String> {
        self.delete(&format!("{DISCORD_API_BASE}/channels/{channel_id}")).await
    }

    /// Create a category channel.
    pub async fn create_category(
        &self,
        guild_id: &str,
        name: &str,
    ) -> Result<serde_json::Value, String> {
        self.create_channel(guild_id, name, 4, None, None).await
    }

    // ── Threads ──────────────────────────────────────────────

    /// List active threads in a channel.
    pub async fn list_threads(&self, channel_id: &str) -> Result<serde_json::Value, String> {
        self.get(&format!(
            "{DISCORD_API_BASE}/channels/{channel_id}/threads/active"
        ))
        .await
    }

    // ── Messages ─────────────────────────────────────────────

    /// Edit a message.
    pub async fn edit_message(
        &self,
        channel_id: &str,
        message_id: &str,
        content: &str,
    ) -> Result<serde_json::Value, String> {
        self.patch_json(
            &format!("{DISCORD_API_BASE}/channels/{channel_id}/messages/{message_id}"),
            &serde_json::json!({ "content": content }),
        )
        .await
    }

    /// Delete a message.
    pub async fn delete_message(
        &self,
        channel_id: &str,
        message_id: &str,
    ) -> Result<(), String> {
        self.delete(&format!(
            "{DISCORD_API_BASE}/channels/{channel_id}/messages/{message_id}"
        ))
        .await
    }

    /// Pin a message.
    pub async fn pin_message(
        &self,
        channel_id: &str,
        message_id: &str,
    ) -> Result<(), String> {
        self.put_empty(&format!(
            "{DISCORD_API_BASE}/channels/{channel_id}/pins/{message_id}"
        ))
        .await
    }

    /// Unpin a message.
    pub async fn unpin_message(
        &self,
        channel_id: &str,
        message_id: &str,
    ) -> Result<(), String> {
        self.delete(&format!(
            "{DISCORD_API_BASE}/channels/{channel_id}/pins/{message_id}"
        ))
        .await
    }

    /// List pinned messages.
    pub async fn list_pins(&self, channel_id: &str) -> Result<serde_json::Value, String> {
        self.get(&format!("{DISCORD_API_BASE}/channels/{channel_id}/pins")).await
    }

    /// Fetch messages from a channel (up to 100).
    pub async fn fetch_messages(
        &self,
        channel_id: &str,
        limit: u8,
        before: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        let mut url = format!(
            "{DISCORD_API_BASE}/channels/{channel_id}/messages?limit={}",
            limit.min(100)
        );
        if let Some(b) = before {
            url.push_str(&format!("&before={b}"));
        }
        self.get(&url).await
    }

    // ── Emoji ────────────────────────────────────────────────

    /// List custom emojis in a guild.
    pub async fn list_emojis(&self, guild_id: &str) -> Result<serde_json::Value, String> {
        self.get(&format!("{DISCORD_API_BASE}/guilds/{guild_id}/emojis")).await
    }

    // ── Polls ────────────────────────────────────────────────

    /// Create a poll (Discord polls are message-based with poll object).
    pub async fn create_poll(
        &self,
        channel_id: &str,
        question: &str,
        answers: Vec<String>,
        duration_hours: u32,
        allow_multiselect: bool,
    ) -> Result<serde_json::Value, String> {
        let answers_json: Vec<serde_json::Value> = answers
            .into_iter()
            .map(|a| serde_json::json!({ "poll_media": { "text": a } }))
            .collect();

        let body = serde_json::json!({
            "poll": {
                "question": { "text": question },
                "answers": answers_json,
                "duration": duration_hours,
                "allow_multiselect": allow_multiselect,
            }
        });

        self.post_json(
            &format!("{DISCORD_API_BASE}/channels/{channel_id}/messages"),
            &body,
        )
        .await
    }

    // ── Moderation ───────────────────────────────────────────

    /// Timeout a member (mute). Duration in seconds, max 28 days.
    pub async fn timeout_member(
        &self,
        guild_id: &str,
        user_id: &str,
        duration_secs: u64,
        reason: Option<&str>,
    ) -> Result<(), String> {
        let until = chrono::Utc::now()
            + chrono::Duration::seconds(duration_secs.min(28 * 24 * 3600) as i64);
        let body = serde_json::json!({
            "communication_disabled_until": until.to_rfc3339(),
        });

        let url = format!("{DISCORD_API_BASE}/guilds/{guild_id}/members/{user_id}");
        let mut req = self.client.patch(&url)
            .header("Authorization", self.auth_header())
            .json(&body);
        if let Some(r) = reason {
            req = req.header("X-Audit-Log-Reason", r);
        }

        let resp = req.send().await.map_err(|e| format!("timeout: {e}"))?;
        if !resp.status().is_success() {
            let err = resp.text().await.unwrap_or_default();
            return Err(format!("timeout failed: {err}"));
        }
        info!(guild_id, user_id, duration_secs, "member timed out");
        Ok(())
    }

    /// Kick a member from the guild.
    pub async fn kick_member(
        &self,
        guild_id: &str,
        user_id: &str,
        reason: Option<&str>,
    ) -> Result<(), String> {
        let url = format!("{DISCORD_API_BASE}/guilds/{guild_id}/members/{user_id}");
        let mut req = self.client.delete(&url)
            .header("Authorization", self.auth_header());
        if let Some(r) = reason {
            req = req.header("X-Audit-Log-Reason", r);
        }

        let resp = req.send().await.map_err(|e| format!("kick: {e}"))?;
        if !resp.status().is_success() {
            let err = resp.text().await.unwrap_or_default();
            return Err(format!("kick failed: {err}"));
        }
        warn!(guild_id, user_id, "member kicked");
        Ok(())
    }

    /// Ban a member from the guild.
    pub async fn ban_member(
        &self,
        guild_id: &str,
        user_id: &str,
        delete_message_days: u8,
        reason: Option<&str>,
    ) -> Result<(), String> {
        let url = format!("{DISCORD_API_BASE}/guilds/{guild_id}/bans/{user_id}");
        let body = serde_json::json!({
            "delete_message_seconds": (delete_message_days.min(7) as u64) * 86400,
        });

        let mut req = self.client.put(&url)
            .header("Authorization", self.auth_header())
            .json(&body);
        if let Some(r) = reason {
            req = req.header("X-Audit-Log-Reason", r);
        }

        let resp = req.send().await.map_err(|e| format!("ban: {e}"))?;
        if !resp.status().is_success() {
            let err = resp.text().await.unwrap_or_default();
            return Err(format!("ban failed: {err}"));
        }
        warn!(guild_id, user_id, "member banned");
        Ok(())
    }

    // ── Presence ─────────────────────────────────────────────

    // Note: Bot presence is set via Gateway (opcode 3), not REST API.
    // This would need integration with the WebSocket connection in mod.rs.

    // ── HTTP helpers ─────────────────────────────────────────

    async fn get(&self, url: &str) -> Result<serde_json::Value, String> {
        let resp = self.client
            .get(url)
            .header("Authorization", self.auth_header())
            .send()
            .await
            .map_err(|e| format!("Discord GET {url}: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            return Err(format!("Discord GET ({status}): {err}"));
        }
        resp.json().await.map_err(|e| format!("parse: {e}"))
    }

    async fn post_json(
        &self,
        url: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let resp = self.client
            .post(url)
            .header("Authorization", self.auth_header())
            .json(body)
            .send()
            .await
            .map_err(|e| format!("Discord POST {url}: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            return Err(format!("Discord POST ({status}): {err}"));
        }
        resp.json().await.map_err(|e| format!("parse: {e}"))
    }

    async fn patch_json(
        &self,
        url: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let resp = self.client
            .patch(url)
            .header("Authorization", self.auth_header())
            .json(body)
            .send()
            .await
            .map_err(|e| format!("Discord PATCH {url}: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            return Err(format!("Discord PATCH ({status}): {err}"));
        }
        resp.json().await.map_err(|e| format!("parse: {e}"))
    }

    async fn put_empty(&self, url: &str) -> Result<(), String> {
        let resp = self.client
            .put(url)
            .header("Authorization", self.auth_header())
            .header("Content-Length", "0")
            .send()
            .await
            .map_err(|e| format!("Discord PUT {url}: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            return Err(format!("Discord PUT ({status}): {err}"));
        }
        Ok(())
    }

    async fn delete(&self, url: &str) -> Result<(), String> {
        let resp = self.client
            .delete(url)
            .header("Authorization", self.auth_header())
            .send()
            .await
            .map_err(|e| format!("Discord DELETE {url}: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            return Err(format!("Discord DELETE ({status}): {err}"));
        }
        Ok(())
    }
}
