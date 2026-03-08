//! Node device commands — SMS, Photos, Contacts, Calendar, Motion.
//!
//! Trait-based abstractions for mobile device capabilities.
//! Platform-specific implementations (Android/iOS) inject concrete providers.
//!
//! ## Commands
//! - `sms.send` — Send SMS message
//! - `photos.latest` — Get recent photos from gallery
//! - `contacts.search` / `contacts.add` — Contacts access
//! - `calendar.events` / `calendar.add` — Calendar access
//! - `motion.activity` / `motion.pedometer` — Motion sensors

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

// ═══════════════════════════════════════════════════════════════
// SMS
// ═══════════════════════════════════════════════════════════════

/// SMS send request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmsSendRequest {
    /// Recipient phone number (E.164 format).
    pub to: String,
    /// Message body.
    pub body: String,
    /// Optional sender ID override.
    pub from: Option<String>,
}

/// SMS send result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmsSendResult {
    pub sent: bool,
    pub message_id: Option<String>,
    pub error: Option<String>,
    pub timestamp: String,
}

/// SMS provider trait.
#[async_trait]
pub trait SmsProvider: Send + Sync {
    /// Send an SMS message.
    async fn send(&self, request: SmsSendRequest) -> Result<SmsSendResult, String>;
    /// Check if SMS sending is available.
    fn is_available(&self) -> bool;
}

// ═══════════════════════════════════════════════════════════════
// Photos
// ═══════════════════════════════════════════════════════════════

/// Photo metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhotoEntry {
    /// Unique identifier.
    pub id: String,
    /// File name.
    pub filename: String,
    /// MIME type (image/jpeg, image/png, etc.).
    pub mime_type: String,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// File size in bytes.
    pub size_bytes: u64,
    /// Creation timestamp (ISO 8601).
    pub created_at: String,
    /// Optional GPS coordinates.
    pub location: Option<PhotoLocation>,
    /// Optional album name.
    pub album: Option<String>,
}

/// Photo GPS location.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhotoLocation {
    pub latitude: f64,
    pub longitude: f64,
}

/// Encoded photo payload (for returning image data).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncodedPhoto {
    pub id: String,
    pub format: String,
    pub base64: String,
    pub width: u32,
    pub height: u32,
}

/// Photos query parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhotosQuery {
    /// Maximum number of photos to return.
    #[serde(default = "default_photo_limit")]
    pub limit: usize,
    /// Offset for pagination.
    #[serde(default)]
    pub offset: usize,
    /// Filter by album name.
    pub album: Option<String>,
    /// Filter: only after this date (ISO 8601).
    pub after: Option<String>,
    /// Filter: only before this date (ISO 8601).
    pub before: Option<String>,
    /// Whether to include base64 image data (thumbnails).
    #[serde(default)]
    pub include_thumbnails: bool,
    /// Max thumbnail width when include_thumbnails is true.
    #[serde(default = "default_thumb_width")]
    pub thumbnail_max_width: u32,
}

fn default_photo_limit() -> usize {
    20
}

fn default_thumb_width() -> u32 {
    200
}

/// Photos provider trait.
#[async_trait]
pub trait PhotosProvider: Send + Sync {
    /// Get recent photos from the device gallery.
    async fn latest(&self, query: PhotosQuery) -> Result<Vec<PhotoEntry>, String>;
    /// Get a specific photo by ID, optionally with base64 data.
    async fn get_photo(
        &self,
        id: &str,
        max_width: Option<u32>,
    ) -> Result<EncodedPhoto, String>;
    /// List available albums.
    async fn list_albums(&self) -> Result<Vec<String>, String>;
    /// Check if photo access is authorized.
    fn is_authorized(&self) -> bool;
}

// ═══════════════════════════════════════════════════════════════
// Contacts
// ═══════════════════════════════════════════════════════════════

/// Contact record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactRecord {
    pub id: String,
    pub given_name: String,
    pub family_name: String,
    pub display_name: String,
    pub phone_numbers: Vec<ContactPhone>,
    pub email_addresses: Vec<ContactEmail>,
    pub organization: Option<String>,
    pub job_title: Option<String>,
    pub note: Option<String>,
    pub birthday: Option<String>,
    /// Thumbnail base64 (small avatar).
    pub thumbnail: Option<String>,
}

/// Contact phone entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactPhone {
    pub label: String,
    pub number: String,
}

/// Contact email entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactEmail {
    pub label: String,
    pub address: String,
}

/// Contact search request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactSearchRequest {
    /// Search query (matches name, phone, email).
    pub query: String,
    /// Maximum results.
    #[serde(default = "default_contact_limit")]
    pub limit: usize,
}

fn default_contact_limit() -> usize {
    25
}

/// New contact to add.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactAddRequest {
    pub given_name: String,
    pub family_name: String,
    pub phone_numbers: Vec<ContactPhone>,
    pub email_addresses: Vec<ContactEmail>,
    pub organization: Option<String>,
    pub job_title: Option<String>,
    pub note: Option<String>,
}

/// Contacts provider trait.
#[async_trait]
pub trait ContactsProvider: Send + Sync {
    /// Search contacts by name/phone/email.
    async fn search(&self, request: ContactSearchRequest) -> Result<Vec<ContactRecord>, String>;
    /// Add a new contact.
    async fn add(&self, contact: ContactAddRequest) -> Result<ContactRecord, String>;
    /// Get a contact by ID.
    async fn get(&self, id: &str) -> Result<Option<ContactRecord>, String>;
    /// Check if contacts access is authorized.
    fn is_authorized(&self) -> bool;
}

// ═══════════════════════════════════════════════════════════════
// Calendar
// ═══════════════════════════════════════════════════════════════

/// Calendar event record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalendarEvent {
    pub id: String,
    pub title: String,
    pub notes: Option<String>,
    pub location: Option<String>,
    /// ISO 8601 start time.
    pub start_time: String,
    /// ISO 8601 end time.
    pub end_time: String,
    pub all_day: bool,
    pub calendar_name: String,
    pub calendar_color: Option<String>,
    pub recurrence: Option<String>,
    pub attendees: Vec<CalendarAttendee>,
    pub url: Option<String>,
}

/// Calendar attendee.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalendarAttendee {
    pub name: Option<String>,
    pub email: String,
    pub status: AttendeeStatus,
}

/// Attendee response status.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttendeeStatus {
    Pending,
    Accepted,
    Declined,
    Tentative,
}

/// Calendar events query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalendarEventsQuery {
    /// Start of time range (ISO 8601).
    pub from: String,
    /// End of time range (ISO 8601).
    pub to: String,
    /// Filter by calendar name.
    pub calendar: Option<String>,
    /// Maximum events to return.
    #[serde(default = "default_calendar_limit")]
    pub limit: usize,
}

fn default_calendar_limit() -> usize {
    50
}

/// Request to add a calendar event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalendarAddRequest {
    pub title: String,
    pub notes: Option<String>,
    pub location: Option<String>,
    pub start_time: String,
    pub end_time: String,
    #[serde(default)]
    pub all_day: bool,
    /// Target calendar name (uses default if omitted).
    pub calendar: Option<String>,
    pub attendees: Option<Vec<String>>,
    pub url: Option<String>,
}

/// Calendar info.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalendarInfo {
    pub name: String,
    pub color: Option<String>,
    pub is_default: bool,
    pub is_writable: bool,
}

/// Calendar provider trait.
#[async_trait]
pub trait CalendarProvider: Send + Sync {
    /// Get events in a time range.
    async fn events(&self, query: CalendarEventsQuery) -> Result<Vec<CalendarEvent>, String>;
    /// Add a new event.
    async fn add(&self, event: CalendarAddRequest) -> Result<CalendarEvent, String>;
    /// List available calendars.
    async fn list_calendars(&self) -> Result<Vec<CalendarInfo>, String>;
    /// Check if calendar access is authorized.
    fn is_authorized(&self) -> bool;
}

// ═══════════════════════════════════════════════════════════════
// Motion
// ═══════════════════════════════════════════════════════════════

/// Motion activity type.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MotionActivityType {
    Stationary,
    Walking,
    Running,
    Cycling,
    Automotive,
    Unknown,
}

/// Motion activity data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MotionActivity {
    pub activity: MotionActivityType,
    pub confidence: f32,
    pub timestamp: String,
    /// Steps since last query (if pedometer available).
    pub steps: Option<u64>,
    /// Distance in meters (if pedometer available).
    pub distance_m: Option<f64>,
    /// Floors ascended (if available).
    pub floors_ascended: Option<u32>,
    /// Floors descended (if available).
    pub floors_descended: Option<u32>,
}

/// Pedometer data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PedometerData {
    pub steps: u64,
    pub distance_m: f64,
    pub floors_ascended: u32,
    pub floors_descended: u32,
    /// Start of measurement interval (ISO 8601).
    pub from: String,
    /// End of measurement interval (ISO 8601).
    pub to: String,
    /// Current cadence (steps per second).
    pub cadence: Option<f64>,
    /// Average pace (seconds per meter).
    pub pace: Option<f64>,
}

/// Accelerometer sample.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccelerometerSample {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub timestamp: String,
}

/// Motion provider trait.
#[async_trait]
pub trait MotionProvider: Send + Sync {
    /// Get current motion activity.
    async fn activity(&self) -> Result<MotionActivity, String>;
    /// Get pedometer data for a time range.
    async fn pedometer(&self, from: &str, to: &str) -> Result<PedometerData, String>;
    /// Get recent accelerometer samples.
    async fn accelerometer(&self, duration_ms: u64) -> Result<Vec<AccelerometerSample>, String>;
    /// Check if motion sensing is available.
    fn is_available(&self) -> bool;
}

// ═══════════════════════════════════════════════════════════════
// Node command dispatcher
// ═══════════════════════════════════════════════════════════════

/// All node device commands that can be dispatched.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum NodeDeviceCommand {
    /// Send an SMS message.
    SmsSend(SmsSendRequest),
    /// Get recent photos.
    PhotosLatest(PhotosQuery),
    /// Get a specific photo with data.
    PhotosGet {
        id: String,
        max_width: Option<u32>,
    },
    /// Search contacts.
    ContactsSearch(ContactSearchRequest),
    /// Add a contact.
    ContactsAdd(ContactAddRequest),
    /// Get calendar events.
    CalendarEvents(CalendarEventsQuery),
    /// Add calendar event.
    CalendarAdd(CalendarAddRequest),
    /// Get current motion activity.
    MotionActivity,
    /// Get pedometer data.
    MotionPedometer {
        from: String,
        to: String,
    },
}

/// Result of a node device command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeCommandResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl NodeCommandResult {
    pub fn success(data: serde_json::Value) -> Self {
        Self {
            ok: true,
            data: Some(data),
            error: None,
        }
    }

    pub fn error(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            data: None,
            error: Some(msg.into()),
        }
    }
}

/// Node device command manager — dispatches to platform providers.
pub struct NodeCommandManager {
    pub sms: Option<Box<dyn SmsProvider>>,
    pub photos: Option<Box<dyn PhotosProvider>>,
    pub contacts: Option<Box<dyn ContactsProvider>>,
    pub calendar: Option<Box<dyn CalendarProvider>>,
    pub motion: Option<Box<dyn MotionProvider>>,
}

impl NodeCommandManager {
    pub fn new() -> Self {
        Self {
            sms: None,
            photos: None,
            contacts: None,
            calendar: None,
            motion: None,
        }
    }

    /// Execute a node device command.
    pub async fn execute(&self, cmd: NodeDeviceCommand) -> NodeCommandResult {
        match cmd {
            NodeDeviceCommand::SmsSend(req) => match &self.sms {
                Some(p) => match p.send(req).await {
                    Ok(r) => NodeCommandResult::success(serde_json::to_value(r).unwrap()),
                    Err(e) => NodeCommandResult::error(e),
                },
                None => NodeCommandResult::error("SMS provider not available"),
            },

            NodeDeviceCommand::PhotosLatest(q) => match &self.photos {
                Some(p) => match p.latest(q).await {
                    Ok(photos) => NodeCommandResult::success(serde_json::to_value(photos).unwrap()),
                    Err(e) => NodeCommandResult::error(e),
                },
                None => NodeCommandResult::error("photos provider not available"),
            },

            NodeDeviceCommand::PhotosGet { id, max_width } => match &self.photos {
                Some(p) => match p.get_photo(&id, max_width).await {
                    Ok(photo) => NodeCommandResult::success(serde_json::to_value(photo).unwrap()),
                    Err(e) => NodeCommandResult::error(e),
                },
                None => NodeCommandResult::error("photos provider not available"),
            },

            NodeDeviceCommand::ContactsSearch(req) => match &self.contacts {
                Some(p) => match p.search(req).await {
                    Ok(contacts) => {
                        NodeCommandResult::success(serde_json::to_value(contacts).unwrap())
                    }
                    Err(e) => NodeCommandResult::error(e),
                },
                None => NodeCommandResult::error("contacts provider not available"),
            },

            NodeDeviceCommand::ContactsAdd(req) => match &self.contacts {
                Some(p) => match p.add(req).await {
                    Ok(contact) => {
                        NodeCommandResult::success(serde_json::to_value(contact).unwrap())
                    }
                    Err(e) => NodeCommandResult::error(e),
                },
                None => NodeCommandResult::error("contacts provider not available"),
            },

            NodeDeviceCommand::CalendarEvents(q) => match &self.calendar {
                Some(p) => match p.events(q).await {
                    Ok(events) => NodeCommandResult::success(serde_json::to_value(events).unwrap()),
                    Err(e) => NodeCommandResult::error(e),
                },
                None => NodeCommandResult::error("calendar provider not available"),
            },

            NodeDeviceCommand::CalendarAdd(req) => match &self.calendar {
                Some(p) => match p.add(req).await {
                    Ok(event) => NodeCommandResult::success(serde_json::to_value(event).unwrap()),
                    Err(e) => NodeCommandResult::error(e),
                },
                None => NodeCommandResult::error("calendar provider not available"),
            },

            NodeDeviceCommand::MotionActivity => match &self.motion {
                Some(p) => match p.activity().await {
                    Ok(act) => NodeCommandResult::success(serde_json::to_value(act).unwrap()),
                    Err(e) => NodeCommandResult::error(e),
                },
                None => NodeCommandResult::error("motion provider not available"),
            },

            NodeDeviceCommand::MotionPedometer { from, to } => match &self.motion {
                Some(p) => match p.pedometer(&from, &to).await {
                    Ok(data) => NodeCommandResult::success(serde_json::to_value(data).unwrap()),
                    Err(e) => NodeCommandResult::error(e),
                },
                None => NodeCommandResult::error("motion provider not available"),
            },
        }
    }

    /// Get list of available commands based on configured providers.
    pub fn available_commands(&self) -> Vec<&'static str> {
        let mut cmds = Vec::new();
        if self.sms.is_some() {
            cmds.push("sms.send");
        }
        if self.photos.is_some() {
            cmds.extend(&["photos.latest", "photos.get"]);
        }
        if self.contacts.is_some() {
            cmds.extend(&["contacts.search", "contacts.add"]);
        }
        if self.calendar.is_some() {
            cmds.extend(&["calendar.events", "calendar.add"]);
        }
        if self.motion.is_some() {
            cmds.extend(&["motion.activity", "motion.pedometer"]);
        }
        cmds
    }
}

impl Default for NodeCommandManager {
    fn default() -> Self {
        Self::new()
    }
}

// ═══════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_command_manager_defaults() {
        let mgr = NodeCommandManager::new();
        assert!(mgr.available_commands().is_empty());
    }

    #[test]
    fn node_command_result_success() {
        let r = NodeCommandResult::success(serde_json::json!({"sent": true}));
        assert!(r.ok);
        assert!(r.error.is_none());
    }

    #[test]
    fn node_command_result_error() {
        let r = NodeCommandResult::error("not available");
        assert!(!r.ok);
        assert_eq!(r.error, Some("not available".into()));
    }

    #[tokio::test]
    async fn no_provider_returns_error() {
        let mgr = NodeCommandManager::new();
        let result = mgr
            .execute(NodeDeviceCommand::SmsSend(SmsSendRequest {
                to: "+1234567890".into(),
                body: "test".into(),
                from: None,
            }))
            .await;
        assert!(!result.ok);
        assert!(result.error.unwrap().contains("not available"));
    }

    #[test]
    fn sms_send_request_serialization() {
        let req = SmsSendRequest {
            to: "+1234567890".into(),
            body: "Hello world".into(),
            from: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("+1234567890"));
    }

    #[test]
    fn calendar_event_serialization() {
        let event = CalendarEvent {
            id: "ev-1".into(),
            title: "Team standup".into(),
            notes: Some("Daily sync".into()),
            location: None,
            start_time: "2026-03-05T09:00:00Z".into(),
            end_time: "2026-03-05T09:30:00Z".into(),
            all_day: false,
            calendar_name: "Work".into(),
            calendar_color: Some("#4285f4".into()),
            recurrence: Some("FREQ=DAILY".into()),
            attendees: vec![CalendarAttendee {
                name: Some("Alice".into()),
                email: "alice@example.com".into(),
                status: AttendeeStatus::Accepted,
            }],
            url: None,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["title"], "Team standup");
    }

    #[test]
    fn contact_record_serialization() {
        let contact = ContactRecord {
            id: "c-1".into(),
            given_name: "John".into(),
            family_name: "Doe".into(),
            display_name: "John Doe".into(),
            phone_numbers: vec![ContactPhone {
                label: "mobile".into(),
                number: "+1234567890".into(),
            }],
            email_addresses: vec![ContactEmail {
                label: "work".into(),
                address: "john@example.com".into(),
            }],
            organization: Some("Acme Corp".into()),
            job_title: None,
            note: None,
            birthday: None,
            thumbnail: None,
        };
        let json = serde_json::to_value(&contact).unwrap();
        assert_eq!(json["display_name"], "John Doe");
    }
}
