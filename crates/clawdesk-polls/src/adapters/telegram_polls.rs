//! Telegram poll adapter — native poll widget via Bot API sendPoll.

use crate::engine::PollResult;
use serde::Serialize;

/// Telegram poll constraints.
pub const TELEGRAM_MIN_DURATION_SECS: u64 = 5;
pub const TELEGRAM_MAX_DURATION_SECS: u64 = 600;
pub const TELEGRAM_MAX_OPTIONS: usize = 10;

/// Convert an abstract PollResult to Telegram API parameters.
#[derive(Debug, Clone, Serialize)]
pub struct TelegramPollParams {
    pub chat_id: String,
    pub question: String,
    pub options: Vec<String>,
    pub is_anonymous: bool,
    pub allows_multiple_answers: bool,
    pub open_period: Option<u64>,
}

pub fn to_telegram_poll(poll: &PollResult, chat_id: &str) -> Result<TelegramPollParams, String> {
    if poll.options.len() > TELEGRAM_MAX_OPTIONS {
        return Err(format!("Telegram supports max {} options, got {}", TELEGRAM_MAX_OPTIONS, poll.options.len()));
    }

    let duration_secs = poll.duration.as_secs();
    let open_period = if duration_secs >= TELEGRAM_MIN_DURATION_SECS && duration_secs <= TELEGRAM_MAX_DURATION_SECS {
        Some(duration_secs)
    } else if duration_secs > TELEGRAM_MAX_DURATION_SECS {
        Some(TELEGRAM_MAX_DURATION_SECS)
    } else {
        None
    };

    Ok(TelegramPollParams {
        chat_id: chat_id.to_string(),
        question: poll.question.clone(),
        options: poll.options.clone(),
        is_anonymous: poll.anonymous,
        allows_multiple_answers: poll.max_selections > 1,
        open_period,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn test_poll() -> PollResult {
        PollResult {
            id: "test".into(),
            question: "Best lang?".into(),
            options: vec!["Rust".into(), "Go".into(), "Zig".into()],
            max_selections: 1,
            duration: Duration::from_secs(300),
            anonymous: true,
        }
    }

    #[test]
    fn valid_telegram_poll() {
        let params = to_telegram_poll(&test_poll(), "12345").unwrap();
        assert_eq!(params.question, "Best lang?");
        assert_eq!(params.open_period, Some(300));
        assert!(!params.allows_multiple_answers);
    }

    #[test]
    fn too_many_options() {
        let mut poll = test_poll();
        poll.options = (0..15).map(|i| format!("Option {i}")).collect();
        assert!(to_telegram_poll(&poll, "12345").is_err());
    }
}
