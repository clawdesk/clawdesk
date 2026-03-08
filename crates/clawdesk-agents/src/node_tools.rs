//! Node device + Talk Mode tool implementations for agents.
//!
//! ## Node Device Tools (dispatch to connected mobile nodes)
//! 1. `sms_send`         — Send SMS via connected mobile node
//! 2. `photos_latest`    — Get recent photos from device gallery
//! 3. `contacts_search`  — Search contacts by name/phone/email
//! 4. `contacts_add`     — Add a new contact
//! 5. `calendar_events`  — Get calendar events in a time range
//! 6. `calendar_add`     — Add a calendar event
//! 7. `motion_activity`  — Get current motion/activity data
//!
//! ## Talk Mode Tools
//! 8. `talk_activate`    — Activate Talk Mode conversation overlay
//! 9. `talk_deactivate`  — Deactivate Talk Mode
//! 10. `talk_status`     — Get Talk Mode phase and session stats

use crate::tools::{Tool, ToolCapability, ToolSchema};
use async_trait::async_trait;
use clawdesk_canvas::node_commands::{
    CalendarAddRequest, CalendarEventsQuery, ContactAddRequest, ContactSearchRequest,
    NodeCommandManager, NodeDeviceCommand, PhotosQuery, SmsSendRequest,
};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::RwLock;

// ═══════════════════════════════════════════════════════════════
// Tool 1: sms_send
// ═══════════════════════════════════════════════════════════════

pub struct SmsSendTool {
    mgr: Arc<RwLock<NodeCommandManager>>,
}

impl SmsSendTool {
    pub fn new(mgr: Arc<RwLock<NodeCommandManager>>) -> Self {
        Self { mgr }
    }
}

#[async_trait]
impl Tool for SmsSendTool {
    fn name(&self) -> &str {
        "sms_send"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "sms_send".into(),
            description: "Send an SMS message via the connected mobile device.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "to": { "type": "string", "description": "Recipient phone number (E.164)" },
                    "body": { "type": "string", "description": "Message text" }
                },
                "required": ["to", "body"],
                "additionalProperties": false
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Messaging]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let to = args["to"].as_str().ok_or("missing 'to'")?.to_string();
        let body = args["body"].as_str().ok_or("missing 'body'")?.to_string();
        let cmd = NodeDeviceCommand::SmsSend(SmsSendRequest {
            to,
            body,
            from: None,
        });
        let result = self.mgr.read().await.execute(cmd).await;
        serde_json::to_string(&result).map_err(|e| e.to_string())
    }
}

// ═══════════════════════════════════════════════════════════════
// Tool 2: photos_latest
// ═══════════════════════════════════════════════════════════════

pub struct PhotosLatestTool {
    mgr: Arc<RwLock<NodeCommandManager>>,
}

impl PhotosLatestTool {
    pub fn new(mgr: Arc<RwLock<NodeCommandManager>>) -> Self {
        Self { mgr }
    }
}

#[async_trait]
impl Tool for PhotosLatestTool {
    fn name(&self) -> &str {
        "photos_latest"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "photos_latest".into(),
            description: "Get recent photos from the connected device's gallery.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "limit": { "type": "integer", "description": "Max photos to return (default 20)", "default": 20 },
                    "album": { "type": "string", "description": "Filter by album name" },
                    "include_thumbnails": { "type": "boolean", "description": "Include base64 thumbnails", "default": false }
                },
                "additionalProperties": false
            }),
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let query = PhotosQuery {
            limit: args["limit"].as_u64().unwrap_or(20) as usize,
            offset: 0,
            album: args["album"].as_str().map(String::from),
            after: None,
            before: None,
            include_thumbnails: args["include_thumbnails"].as_bool().unwrap_or(false),
            thumbnail_max_width: 200,
        };
        let cmd = NodeDeviceCommand::PhotosLatest(query);
        let result = self.mgr.read().await.execute(cmd).await;
        serde_json::to_string(&result).map_err(|e| e.to_string())
    }
}

// ═══════════════════════════════════════════════════════════════
// Tool 3: contacts_search
// ═══════════════════════════════════════════════════════════════

pub struct ContactsSearchTool {
    mgr: Arc<RwLock<NodeCommandManager>>,
}

impl ContactsSearchTool {
    pub fn new(mgr: Arc<RwLock<NodeCommandManager>>) -> Self {
        Self { mgr }
    }
}

#[async_trait]
impl Tool for ContactsSearchTool {
    fn name(&self) -> &str {
        "contacts_search"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "contacts_search".into(),
            description: "Search contacts on the connected device by name, phone, or email."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search query" },
                    "limit": { "type": "integer", "description": "Max results (default 25)", "default": 25 }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let query = args["query"]
            .as_str()
            .ok_or("missing 'query'")?
            .to_string();
        let limit = args["limit"].as_u64().unwrap_or(25) as usize;
        let cmd =
            NodeDeviceCommand::ContactsSearch(ContactSearchRequest { query, limit });
        let result = self.mgr.read().await.execute(cmd).await;
        serde_json::to_string(&result).map_err(|e| e.to_string())
    }
}

// ═══════════════════════════════════════════════════════════════
// Tool 4: contacts_add
// ═══════════════════════════════════════════════════════════════

pub struct ContactsAddTool {
    mgr: Arc<RwLock<NodeCommandManager>>,
}

impl ContactsAddTool {
    pub fn new(mgr: Arc<RwLock<NodeCommandManager>>) -> Self {
        Self { mgr }
    }
}

#[async_trait]
impl Tool for ContactsAddTool {
    fn name(&self) -> &str {
        "contacts_add"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "contacts_add".into(),
            description: "Add a new contact on the connected device.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "given_name": { "type": "string" },
                    "family_name": { "type": "string" },
                    "phone": { "type": "string", "description": "Phone number" },
                    "email": { "type": "string", "description": "Email address" },
                    "organization": { "type": "string" }
                },
                "required": ["given_name", "family_name"],
                "additionalProperties": false
            }),
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        use clawdesk_canvas::node_commands::{ContactEmail, ContactPhone};

        let mut phones = vec![];
        if let Some(p) = args["phone"].as_str() {
            phones.push(ContactPhone {
                label: "mobile".into(),
                number: p.into(),
            });
        }
        let mut emails = vec![];
        if let Some(e) = args["email"].as_str() {
            emails.push(ContactEmail {
                label: "work".into(),
                address: e.into(),
            });
        }

        let req = ContactAddRequest {
            given_name: args["given_name"]
                .as_str()
                .ok_or("missing given_name")?
                .into(),
            family_name: args["family_name"]
                .as_str()
                .ok_or("missing family_name")?
                .into(),
            phone_numbers: phones,
            email_addresses: emails,
            organization: args["organization"].as_str().map(String::from),
            job_title: None,
            note: None,
        };
        let cmd = NodeDeviceCommand::ContactsAdd(req);
        let result = self.mgr.read().await.execute(cmd).await;
        serde_json::to_string(&result).map_err(|e| e.to_string())
    }
}

// ═══════════════════════════════════════════════════════════════
// Tool 5: calendar_events
// ═══════════════════════════════════════════════════════════════

pub struct CalendarEventsTool {
    mgr: Arc<RwLock<NodeCommandManager>>,
}

impl CalendarEventsTool {
    pub fn new(mgr: Arc<RwLock<NodeCommandManager>>) -> Self {
        Self { mgr }
    }
}

#[async_trait]
impl Tool for CalendarEventsTool {
    fn name(&self) -> &str {
        "calendar_events"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "calendar_events".into(),
            description: "Get calendar events from the connected device within a time range."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "from": { "type": "string", "description": "Start of range (ISO 8601)" },
                    "to": { "type": "string", "description": "End of range (ISO 8601)" },
                    "calendar": { "type": "string", "description": "Filter by calendar name" },
                    "limit": { "type": "integer", "description": "Max events (default 50)", "default": 50 }
                },
                "required": ["from", "to"],
                "additionalProperties": false
            }),
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let from = args["from"].as_str().ok_or("missing 'from'")?.to_string();
        let to = args["to"].as_str().ok_or("missing 'to'")?.to_string();
        let query = CalendarEventsQuery {
            from,
            to,
            calendar: args["calendar"].as_str().map(String::from),
            limit: args["limit"].as_u64().unwrap_or(50) as usize,
        };
        let cmd = NodeDeviceCommand::CalendarEvents(query);
        let result = self.mgr.read().await.execute(cmd).await;
        serde_json::to_string(&result).map_err(|e| e.to_string())
    }
}

// ═══════════════════════════════════════════════════════════════
// Tool 6: calendar_add
// ═══════════════════════════════════════════════════════════════

pub struct CalendarAddTool {
    mgr: Arc<RwLock<NodeCommandManager>>,
}

impl CalendarAddTool {
    pub fn new(mgr: Arc<RwLock<NodeCommandManager>>) -> Self {
        Self { mgr }
    }
}

#[async_trait]
impl Tool for CalendarAddTool {
    fn name(&self) -> &str {
        "calendar_add"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "calendar_add".into(),
            description: "Add a new calendar event on the connected device.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "title": { "type": "string", "description": "Event title" },
                    "start_time": { "type": "string", "description": "Start time (ISO 8601)" },
                    "end_time": { "type": "string", "description": "End time (ISO 8601)" },
                    "location": { "type": "string" },
                    "notes": { "type": "string" },
                    "all_day": { "type": "boolean", "default": false },
                    "calendar": { "type": "string", "description": "Calendar name" }
                },
                "required": ["title", "start_time", "end_time"],
                "additionalProperties": false
            }),
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let req = CalendarAddRequest {
            title: args["title"]
                .as_str()
                .ok_or("missing 'title'")?
                .to_string(),
            start_time: args["start_time"]
                .as_str()
                .ok_or("missing 'start_time'")?
                .to_string(),
            end_time: args["end_time"]
                .as_str()
                .ok_or("missing 'end_time'")?
                .to_string(),
            location: args["location"].as_str().map(String::from),
            notes: args["notes"].as_str().map(String::from),
            all_day: args["all_day"].as_bool().unwrap_or(false),
            calendar: args["calendar"].as_str().map(String::from),
            attendees: None,
            url: None,
        };
        let cmd = NodeDeviceCommand::CalendarAdd(req);
        let result = self.mgr.read().await.execute(cmd).await;
        serde_json::to_string(&result).map_err(|e| e.to_string())
    }
}

// ═══════════════════════════════════════════════════════════════
// Tool 7: motion_activity
// ═══════════════════════════════════════════════════════════════

pub struct MotionActivityTool {
    mgr: Arc<RwLock<NodeCommandManager>>,
}

impl MotionActivityTool {
    pub fn new(mgr: Arc<RwLock<NodeCommandManager>>) -> Self {
        Self { mgr }
    }
}

#[async_trait]
impl Tool for MotionActivityTool {
    fn name(&self) -> &str {
        "motion_activity"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "motion_activity".into(),
            description:
                "Get current motion activity data from the connected device (steps, activity type)."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        }
    }

    async fn execute(&self, _args: serde_json::Value) -> Result<String, String> {
        let cmd = NodeDeviceCommand::MotionActivity;
        let result = self.mgr.read().await.execute(cmd).await;
        serde_json::to_string(&result).map_err(|e| e.to_string())
    }
}

// ═══════════════════════════════════════════════════════════════
// Talk Mode tools (independent of NodeCommandManager — use TalkModeController)
// ═══════════════════════════════════════════════════════════════

// Note: TalkModeController is in clawdesk-media, which may not be a dependency
// of clawdesk-agents. We define these tools to work via a simple trait bridge.

/// Trait for Talk Mode operations accessible from agent tools.
#[async_trait]
pub trait TalkModeBridge: Send + Sync {
    async fn activate(&self, source: &str) -> Result<String, String>;
    async fn deactivate(&self) -> Result<(), String>;
    async fn phase(&self) -> String;
    async fn session_stats(&self) -> Result<String, String>;
}

pub struct TalkActivateTool {
    bridge: Arc<dyn TalkModeBridge>,
}

impl TalkActivateTool {
    pub fn new(bridge: Arc<dyn TalkModeBridge>) -> Self {
        Self { bridge }
    }
}

#[async_trait]
impl Tool for TalkActivateTool {
    fn name(&self) -> &str {
        "talk_activate"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "talk_activate".into(),
            description: "Activate Talk Mode — start a voice conversation overlay with the user."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "source": {
                        "type": "string",
                        "enum": ["programmatic", "wake_phrase", "push_to_talk", "ui_button"],
                        "default": "programmatic",
                        "description": "How Talk Mode was triggered"
                    }
                },
                "additionalProperties": false
            }),
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let source = args["source"].as_str().unwrap_or("programmatic");
        self.bridge.activate(source).await
    }
}

pub struct TalkDeactivateTool {
    bridge: Arc<dyn TalkModeBridge>,
}

impl TalkDeactivateTool {
    pub fn new(bridge: Arc<dyn TalkModeBridge>) -> Self {
        Self { bridge }
    }
}

#[async_trait]
impl Tool for TalkDeactivateTool {
    fn name(&self) -> &str {
        "talk_deactivate"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "talk_deactivate".into(),
            description: "Deactivate Talk Mode — stop the voice conversation.".into(),
            parameters: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        }
    }

    async fn execute(&self, _args: serde_json::Value) -> Result<String, String> {
        self.bridge.deactivate().await?;
        Ok(r#"{"ok": true}"#.to_string())
    }
}

pub struct TalkStatusTool {
    bridge: Arc<dyn TalkModeBridge>,
}

impl TalkStatusTool {
    pub fn new(bridge: Arc<dyn TalkModeBridge>) -> Self {
        Self { bridge }
    }
}

#[async_trait]
impl Tool for TalkStatusTool {
    fn name(&self) -> &str {
        "talk_status"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "talk_status".into(),
            description: "Get Talk Mode status — current phase and session statistics.".into(),
            parameters: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        }
    }

    async fn execute(&self, _args: serde_json::Value) -> Result<String, String> {
        let phase = self.bridge.phase().await;
        let stats = self.bridge.session_stats().await.unwrap_or_default();
        Ok(json!({ "phase": phase, "stats": stats }).to_string())
    }
}

// ═══════════════════════════════════════════════════════════════
// Factory functions
// ═══════════════════════════════════════════════════════════════

/// Create all node device tools for an agent.
pub fn create_node_device_tools(
    mgr: Arc<RwLock<NodeCommandManager>>,
) -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(SmsSendTool::new(mgr.clone())),
        Box::new(PhotosLatestTool::new(mgr.clone())),
        Box::new(ContactsSearchTool::new(mgr.clone())),
        Box::new(ContactsAddTool::new(mgr.clone())),
        Box::new(CalendarEventsTool::new(mgr.clone())),
        Box::new(CalendarAddTool::new(mgr.clone())),
        Box::new(MotionActivityTool::new(mgr)),
    ]
}

/// Create all Talk Mode tools for an agent.
pub fn create_talk_mode_tools(
    bridge: Arc<dyn TalkModeBridge>,
) -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(TalkActivateTool::new(bridge.clone())),
        Box::new(TalkDeactivateTool::new(bridge.clone())),
        Box::new(TalkStatusTool::new(bridge)),
    ]
}
