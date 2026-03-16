//! Discord poll adapter — reaction-based voting.

use crate::engine::PollResult;
use serde::Serialize;

/// Discord uses reactions for polls. Map options → emoji reactions.
const REACTION_EMOJIS: &[&str] = &["1️⃣", "2️⃣", "3️⃣", "4️⃣", "5️⃣", "6️⃣", "7️⃣", "8️⃣", "9️⃣", "🔟"];

/// Discord poll message format.
#[derive(Debug, Clone, Serialize)]
pub struct DiscordPollEmbed {
    pub title: String,
    pub description: String,
    pub reactions: Vec<String>,
    pub footer: String,
}

pub fn to_discord_poll(poll: &PollResult) -> DiscordPollEmbed {
    let mut description = String::new();
    let reactions: Vec<String> = poll.options.iter().enumerate().map(|(i, opt)| {
        let emoji = REACTION_EMOJIS.get(i).unwrap_or(&"▪️");
        description.push_str(&format!("{emoji} {opt}\n"));
        emoji.to_string()
    }).collect();

    let multi = if poll.max_selections > 1 {
        format!(" (select up to {})", poll.max_selections)
    } else {
        String::new()
    };

    DiscordPollEmbed {
        title: format!("📊 {}", poll.question),
        description,
        reactions,
        footer: format!("React to vote{multi} · Closes in {} min", poll.duration.as_secs() / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn discord_poll_format() {
        let poll = PollResult {
            id: "t".into(), question: "Lunch?".into(),
            options: vec!["Pizza".into(), "Sushi".into()],
            max_selections: 1, duration: Duration::from_secs(600), anonymous: false,
        };
        let embed = to_discord_poll(&poll);
        assert!(embed.description.contains("Pizza"));
        assert_eq!(embed.reactions.len(), 2);
    }
}
