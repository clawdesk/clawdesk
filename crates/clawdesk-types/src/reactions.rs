//! GAP 5 — Reactions & Rich Messages
//!
//! Defines rich message types used across ClawDesk's multi-channel architecture.
//! This module provides a unified representation for reactions, polls, cards,
//! carousels, quick replies, buttons, location shares, and contact shares
//! that can be mapped to and from channel-specific formats.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Reactions
// ---------------------------------------------------------------------------

/// An emoji or custom reaction attached to a message.
///
/// Reactions are normalized across channels — native emoji are stored as their
/// Unicode representation while channel-specific custom emoji carry an optional
/// URL so the UI can render them without a round-trip to the origin platform.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Reaction {
    /// Unicode emoji string (e.g. "👍") or a short-code identifier for custom emoji.
    pub emoji: String,
    /// The user who reacted.
    pub user_id: String,
    /// The message this reaction is attached to.
    pub message_id: String,
    /// When the reaction was created (or removed).
    pub timestamp: DateTime<Utc>,
    /// Optional URL for a custom (non-Unicode) emoji image.
    pub custom_emoji_url: Option<String>,
}

/// Lifecycle event for a reaction — either newly added or removed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ReactionEvent {
    /// A reaction was added to a message.
    Added(Reaction),
    /// A previously-existing reaction was removed from a message.
    Removed(Reaction),
}

// ---------------------------------------------------------------------------
// Rich content envelope
// ---------------------------------------------------------------------------

/// A rich-content payload that can accompany (or replace) a plain-text message.
///
/// Each variant wraps a channel-agnostic content struct that channel adapters
/// translate into the platform's native format on send, and back into this
/// representation on receive.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "data")]
pub enum RichContent {
    /// An interactive poll.
    Poll(PollContent),
    /// A single rich card (hero image + actions).
    Card(CardContent),
    /// A horizontally-scrollable set of cards.
    Carousel(CarouselContent),
    /// A set of quick-reply chips shown below a prompt.
    QuickReplies(QuickReplySet),
    /// One or more action buttons attached to a text block.
    Button(ButtonContent),
    /// A geographic location share.
    LocationShare(LocationContent),
    /// A contact / vCard share.
    ContactShare(ContactContent),
}

// ---------------------------------------------------------------------------
// Poll
// ---------------------------------------------------------------------------

/// Content for an interactive poll message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PollContent {
    /// The poll question.
    pub question: String,
    /// Available answer options.
    pub options: Vec<PollOption>,
    /// Whether voters may select more than one option.
    pub allows_multiple: bool,
    /// Whether individual votes are hidden from other participants.
    pub anonymous: bool,
    /// Optional deadline after which voting closes automatically.
    pub close_at: Option<DateTime<Utc>>,
}

/// A single option within a [`PollContent`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PollOption {
    /// Unique identifier for this option within the poll.
    pub id: String,
    /// Human-readable label.
    pub text: String,
    /// Current number of votes.
    pub voter_count: u32,
}

// ---------------------------------------------------------------------------
// Card / Carousel
// ---------------------------------------------------------------------------

/// A rich card with an optional hero image, text body, and action buttons.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CardContent {
    /// Card title (always required).
    pub title: String,
    /// Optional subtitle rendered below the title.
    pub subtitle: Option<String>,
    /// Optional hero / thumbnail image URL.
    pub image_url: Option<String>,
    /// Optional longer body text.
    pub body: Option<String>,
    /// Zero or more interactive actions attached to the card.
    pub actions: Vec<CardAction>,
}

/// An interactive element on a [`CardContent`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "action_type")]
pub enum CardAction {
    /// A tappable button that may open a URL or post a payload back to the bot.
    Button {
        /// Button label text.
        label: String,
        /// Optional URL to open on tap.
        url: Option<String>,
        /// Optional postback payload delivered to the bot.
        payload: Option<String>,
    },
    /// A simple hyperlink.
    Link {
        /// Link display text.
        label: String,
        /// Destination URL.
        url: String,
    },
}

/// A horizontally-scrollable carousel of [`CardContent`] items.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CarouselContent {
    /// Ordered list of cards in the carousel.
    pub cards: Vec<CardContent>,
}

// ---------------------------------------------------------------------------
// Quick replies
// ---------------------------------------------------------------------------

/// A prompt followed by a set of quick-reply chips the user can tap.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QuickReplySet {
    /// The prompt text displayed above the quick-reply chips.
    pub prompt: String,
    /// Available quick-reply options.
    pub replies: Vec<QuickReply>,
}

/// A single quick-reply chip.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QuickReply {
    /// Display label on the chip.
    pub label: String,
    /// Postback payload sent when the chip is tapped.
    pub payload: String,
    /// Optional small image rendered alongside the label.
    pub image_url: Option<String>,
}

// ---------------------------------------------------------------------------
// Buttons
// ---------------------------------------------------------------------------

/// A text block with one or more attached action buttons.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ButtonContent {
    /// Message text displayed above the buttons.
    pub text: String,
    /// The buttons themselves.
    pub buttons: Vec<ButtonItem>,
}

/// A single button in a [`ButtonContent`] block.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ButtonItem {
    /// Button label text.
    pub label: String,
    /// Visual style hint for the rendering layer.
    pub style: ButtonStyle,
    /// Optional URL to open on click.
    pub url: Option<String>,
    /// Optional postback payload.
    pub payload: Option<String>,
}

/// Visual style hint for a [`ButtonItem`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ButtonStyle {
    /// Default / primary emphasis.
    Primary,
    /// Reduced emphasis.
    Secondary,
    /// Destructive / warning action.
    Danger,
    /// Styled as a hyperlink rather than a button.
    Link,
}

// ---------------------------------------------------------------------------
// Location & Contact
// ---------------------------------------------------------------------------

/// A geographic location share.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LocationContent {
    /// Latitude in decimal degrees.
    pub latitude: f64,
    /// Longitude in decimal degrees.
    pub longitude: f64,
    /// Optional human-readable place name.
    pub label: Option<String>,
    /// Optional street address.
    pub address: Option<String>,
}

/// A contact / vCard share.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContactContent {
    /// Contact display name.
    pub name: String,
    /// Optional phone number.
    pub phone: Option<String>,
    /// Optional email address.
    pub email: Option<String>,
}

// ---------------------------------------------------------------------------
// Outbound message wrapper
// ---------------------------------------------------------------------------

/// A rich outbound message combining plain text, optional rich content, and
/// any reactions that should be rendered alongside the message.
///
/// Channel adapters consume this struct when delivering messages and map the
/// rich payload to the target platform's native format.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RichOutboundMessage {
    /// Plain-text (or Markdown) message body.
    pub body: String,
    /// Optional rich content payload.
    pub rich: Option<RichContent>,
    /// Reactions attached to this message.
    pub reactions: Vec<Reaction>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    /// Helper: serialize to JSON and back, asserting equality.
    fn roundtrip<T>(value: &T)
    where
        T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug,
    {
        let json = serde_json::to_string(value).expect("serialize");
        let back: T = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(*value, back);
    }

    #[test]
    fn reaction_roundtrip() {
        let r = Reaction {
            emoji: "👍".into(),
            user_id: "u-1".into(),
            message_id: "m-42".into(),
            timestamp: Utc::now(),
            custom_emoji_url: None,
        };
        roundtrip(&r);
    }

    #[test]
    fn reaction_with_custom_emoji_roundtrip() {
        let r = Reaction {
            emoji: ":partyparrot:".into(),
            user_id: "u-2".into(),
            message_id: "m-99".into(),
            timestamp: Utc::now(),
            custom_emoji_url: Some("https://cdn.example.com/partyparrot.gif".into()),
        };
        roundtrip(&r);
    }

    #[test]
    fn reaction_event_roundtrip() {
        let added = ReactionEvent::Added(Reaction {
            emoji: "🎉".into(),
            user_id: "u-3".into(),
            message_id: "m-7".into(),
            timestamp: Utc::now(),
            custom_emoji_url: None,
        });
        roundtrip(&added);

        let removed = ReactionEvent::Removed(Reaction {
            emoji: "😢".into(),
            user_id: "u-4".into(),
            message_id: "m-8".into(),
            timestamp: Utc::now(),
            custom_emoji_url: None,
        });
        roundtrip(&removed);
    }

    #[test]
    fn poll_content_roundtrip() {
        let poll = RichContent::Poll(PollContent {
            question: "Favorite language?".into(),
            options: vec![
                PollOption { id: "1".into(), text: "Rust".into(), voter_count: 42 },
                PollOption { id: "2".into(), text: "TypeScript".into(), voter_count: 17 },
            ],
            allows_multiple: false,
            anonymous: true,
            close_at: Some(Utc::now()),
        });
        roundtrip(&poll);
    }

    #[test]
    fn card_content_roundtrip() {
        let card = RichContent::Card(CardContent {
            title: "Welcome".into(),
            subtitle: Some("Getting started".into()),
            image_url: Some("https://example.com/hero.png".into()),
            body: Some("Hello and welcome to ClawDesk!".into()),
            actions: vec![
                CardAction::Button {
                    label: "Open".into(),
                    url: Some("https://example.com".into()),
                    payload: None,
                },
                CardAction::Link {
                    label: "Docs".into(),
                    url: "https://docs.example.com".into(),
                },
            ],
        });
        roundtrip(&card);
    }

    #[test]
    fn carousel_content_roundtrip() {
        let carousel = RichContent::Carousel(CarouselContent {
            cards: vec![
                CardContent {
                    title: "Card A".into(),
                    subtitle: None,
                    image_url: None,
                    body: None,
                    actions: vec![],
                },
                CardContent {
                    title: "Card B".into(),
                    subtitle: Some("Sub B".into()),
                    image_url: None,
                    body: Some("Body B".into()),
                    actions: vec![CardAction::Link {
                        label: "Go".into(),
                        url: "https://b.example.com".into(),
                    }],
                },
            ],
        });
        roundtrip(&carousel);
    }

    #[test]
    fn quick_replies_roundtrip() {
        let qr = RichContent::QuickReplies(QuickReplySet {
            prompt: "Pick one:".into(),
            replies: vec![
                QuickReply { label: "Yes".into(), payload: "yes".into(), image_url: None },
                QuickReply {
                    label: "No".into(),
                    payload: "no".into(),
                    image_url: Some("https://example.com/no.png".into()),
                },
            ],
        });
        roundtrip(&qr);
    }

    #[test]
    fn button_content_roundtrip() {
        let btns = RichContent::Button(ButtonContent {
            text: "Choose an action".into(),
            buttons: vec![
                ButtonItem {
                    label: "Confirm".into(),
                    style: ButtonStyle::Primary,
                    url: None,
                    payload: Some("confirm".into()),
                },
                ButtonItem {
                    label: "Cancel".into(),
                    style: ButtonStyle::Danger,
                    url: None,
                    payload: Some("cancel".into()),
                },
                ButtonItem {
                    label: "Learn more".into(),
                    style: ButtonStyle::Link,
                    url: Some("https://example.com/info".into()),
                    payload: None,
                },
            ],
        });
        roundtrip(&btns);
    }

    #[test]
    fn location_content_roundtrip() {
        let loc = RichContent::LocationShare(LocationContent {
            latitude: 37.7749,
            longitude: -122.4194,
            label: Some("San Francisco".into()),
            address: Some("1 Market St, San Francisco, CA".into()),
        });
        roundtrip(&loc);
    }

    #[test]
    fn contact_content_roundtrip() {
        let contact = RichContent::ContactShare(ContactContent {
            name: "Ada Lovelace".into(),
            phone: Some("+1-555-0100".into()),
            email: Some("ada@example.com".into()),
        });
        roundtrip(&contact);
    }

    #[test]
    fn rich_outbound_message_roundtrip() {
        let msg = RichOutboundMessage {
            body: "Check out this poll!".into(),
            rich: Some(RichContent::Poll(PollContent {
                question: "Ship it?".into(),
                options: vec![
                    PollOption { id: "y".into(), text: "Yes".into(), voter_count: 0 },
                    PollOption { id: "n".into(), text: "No".into(), voter_count: 0 },
                ],
                allows_multiple: false,
                anonymous: false,
                close_at: None,
            })),
            reactions: vec![Reaction {
                emoji: "🚀".into(),
                user_id: "u-ship".into(),
                message_id: "m-poll".into(),
                timestamp: Utc::now(),
                custom_emoji_url: None,
            }],
        };
        roundtrip(&msg);
    }

    #[test]
    fn rich_outbound_message_text_only_roundtrip() {
        let msg = RichOutboundMessage {
            body: "Plain text, no rich content.".into(),
            rich: None,
            reactions: vec![],
        };
        roundtrip(&msg);
    }

    #[test]
    fn button_style_variants_roundtrip() {
        for style in [
            ButtonStyle::Primary,
            ButtonStyle::Secondary,
            ButtonStyle::Danger,
            ButtonStyle::Link,
        ] {
            roundtrip(&style);
        }
    }
}
