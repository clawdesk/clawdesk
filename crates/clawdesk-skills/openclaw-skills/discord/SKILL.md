---
name: discord
description: "Send messages to Discord via message_send(channel='discord', content='...')."
metadata: { "openclaw": { "emoji": "🎮", "requires": { "config": ["channels.discord.token"] } } }
allowed-tools: ["message_send"]
---

# Discord Messaging

Send messages to Discord using the `message_send` tool. Routing is automatic — **never ask for channel IDs, guild IDs, or user IDs**.

## How to Send

Just call `message_send` with `channel: "discord"` and `content: "<your message>"`. The system handles routing automatically.

```json
{ "channel": "discord", "content": "Hello from the agent!" }
```

That's it. No IDs needed. The `to` parameter defaults to the active Discord channel.

## Examples

Send a simple message:
```json
{ "channel": "discord", "content": "Hello!" }
```

Send a longer message:
```json
{ "channel": "discord", "content": "Here's the weekly report:\n\n• Task A: done\n• Task B: in progress\n• Task C: blocked" }
```

## Rules

- **NEVER ask the user for Discord channel IDs, guild IDs, or user IDs.**
- **NEVER refuse to send because you don't have an ID.** Just call `message_send`.
- Always set `channel: "discord"`.
- Omit the `to` parameter — it defaults to the correct destination automatically.
- Only set `to` if the user explicitly provides a numeric Discord channel ID.
- Mention users as `<@USER_ID>` only if the user provides the ID.

## Writing Style (Discord)

- Short, conversational, low ceremony.
- Avoid Markdown tables — Discord renders them poorly.
- Use bullet points and short paragraphs.
- Keep messages under 2000 characters (Discord limit).
