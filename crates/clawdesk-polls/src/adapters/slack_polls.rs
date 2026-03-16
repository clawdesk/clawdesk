//! Slack poll adapter — Block Kit-based polls.

use crate::engine::PollResult;
use serde_json::json;

/// Build a Slack Block Kit message for a poll.
pub fn to_slack_blocks(poll: &PollResult) -> serde_json::Value {
    let mut blocks = vec![
        json!({
            "type": "header",
            "text": { "type": "plain_text", "text": format!("📊 {}", poll.question) }
        }),
    ];

    for (i, option) in poll.options.iter().enumerate() {
        blocks.push(json!({
            "type": "section",
            "text": { "type": "mrkdwn", "text": format!("*{}*", option) },
            "accessory": {
                "type": "button",
                "text": { "type": "plain_text", "text": "Vote" },
                "action_id": format!("poll_vote_{}", i),
                "value": format!("{}:{}", poll.id, i)
            }
        }));
    }

    blocks.push(json!({
        "type": "context",
        "elements": [{ "type": "mrkdwn", "text": format!("Closes in {} min", poll.duration.as_secs() / 60) }]
    }));

    json!({ "blocks": blocks })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn slack_blocks_structure() {
        let poll = PollResult {
            id: "p1".into(), question: "Team outing?".into(),
            options: vec!["Bowling".into(), "Laser tag".into(), "Escape room".into()],
            max_selections: 1, duration: Duration::from_secs(3600), anonymous: false,
        };
        let blocks = to_slack_blocks(&poll);
        let blocks_arr = blocks.get("blocks").unwrap().as_array().unwrap();
        assert!(blocks_arr.len() >= 4); // header + 3 options + context
    }
}
