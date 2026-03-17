import { useCallback, useMemo, useState } from "react";
import type { ChannelConfigField, ChannelTypeSpec } from "../types";

// ═══════════════════════════════════════════════════════════════
// Channel-specific setup journey definitions
// ═══════════════════════════════════════════════════════════════
// Each journey is a multi-step guided flow. Steps include:
//   - prerequisites: what the user needs before starting
//   - configure: the actual config fields
//   - verify: validation / confirmation
//
// Per-channel onboarding adapter pattern
// but rendered as a React step-by-step journey in the ClawDesk UI.
// ═══════════════════════════════════════════════════════════════

/** A single step in a channel setup journey. */
export interface JourneyStep {
  id: string;
  title: string;
  /** Markdown-flavoured description shown at the top of the step. */
  description: string;
  /** Which config field keys are relevant to this step (shown as inputs). */
  fields?: string[];
  /** A validation function — returns null if valid, or an error message. */
  validate?: (draft: Record<string, string>) => string | null;
  /** An informational note shown below the fields. */
  note?: string;
}

/** Full journey definition for a channel type. */
export interface ChannelJourney {
  channelType: string;
  steps: JourneyStep[];
}

// ── Per-channel journey definitions ──────────────────────────

const TELEGRAM_JOURNEY: ChannelJourney = {
  channelType: "Telegram",
  steps: [
    {
      id: "prereqs",
      title: "Create a Bot",
      description:
        "To connect Telegram, you need a **Bot Token** from @BotFather.\n\n" +
        "1. Open Telegram and search for **@BotFather**\n" +
        "2. Send `/newbot` and follow the prompts\n" +
        "3. Copy the API token you receive\n\n" +
        "Optionally, send `/setdescription` and `/setabouttext` to customize your bot's profile.",
      note: "The token looks like: 123456789:ABCdefGhIJKlmNOpQRsTUVwxyZ",
    },
    {
      id: "configure",
      title: "Configure",
      description: "Paste your bot token below.",
      fields: ["bot_token"],
      validate: (draft) => {
        const token = draft.bot_token?.trim();
        if (!token) return "Bot token is required";
        if (!/^\d+:[A-Za-z0-9_-]{35,}$/.test(token)) return "Invalid token format. Expected: 123456789:ABCdef...";
        return null;
      },
    },
    {
      id: "access",
      title: "Access Control",
      description:
        "Control who can interact with your bot.\n\n" +
        "Enter **Telegram numeric user IDs** (comma-separated) to restrict access, or `*` to allow everyone.\n\n" +
        "**To find your user ID:** Send a message to [@userinfobot](https://t.me/userinfobot) on Telegram.\n\n" +
        "**Security note:** Leaving this empty will **deny all users** by default.",
      fields: ["allowed_users", "mention_only"],
      validate: (draft) => {
        const users = draft.allowed_users?.trim();
        if (!users) return null;
        if (users === "*") return null;
        const items = users.split(",").map((s) => s.trim()).filter(Boolean);
        for (const item of items) {
          if (!/^\d+$/.test(item)) return `Invalid user ID: "${item}". Must be a numeric Telegram user ID`;
        }
        return null;
      },
      note: "Empty = deny all (safe default). Use * to allow everyone.",
    },
    {
      id: "verify",
      title: "Ready",
      description:
        "Your Telegram bot is ready to connect!\n\n" +
        "After finishing setup:\n" +
        "1. Open Telegram and find your bot by its username\n" +
        "2. Send it a message\n" +
        "3. ClawDesk will receive and respond automatically\n\n" +
        "**For groups:** Add the bot to a group and mention it with `@botname`.",
    },
  ],
};

const DISCORD_JOURNEY: ChannelJourney = {
  channelType: "Discord",
  steps: [
    {
      id: "prereqs",
      title: "Create a Discord App",
      description:
        "1. Go to the [Discord Developer Portal](https://discord.com/developers/applications)\n" +
        "2. Click **New Application** and give it a name\n" +
        "3. Go to **General Information** — copy the **Application ID**\n" +
        "4. Go to the **Bot** section and click **Reset Token** to get a bot token\n" +
        "5. Under **Privileged Gateway Intents**, enable:\n" +
        "   - **Message Content Intent** *(required)*\n" +
        "   - **Server Members Intent** *(recommended)*",
      note: "Message Content Intent is required — without it, the bot cannot read message text.",
    },
    {
      id: "configure",
      title: "Configure",
      description: "Enter your Discord bot token and application ID.",
      fields: ["bot_token", "application_id"],
      validate: (draft) => {
        const token = draft.bot_token?.trim();
        if (!token) return "Bot token is required";
        if (token.length < 50) return "Token seems too short. Discord tokens are typically 70+ characters";
        const appId = draft.application_id?.trim();
        if (!appId) return "Application ID is required";
        if (!/^\d{17,20}$/.test(appId)) return "Application ID must be a 17-20 digit number (from Developer Portal → General Information)";
        return null;
      },
    },
    {
      id: "invite",
      title: "Invite Bot",
      description:
        "Invite your bot to a Discord server:\n\n" +
        "1. Go to **OAuth2 → URL Generator** in the Developer Portal\n" +
        "2. Select scopes: `bot`\n" +
        "3. Select permissions: `Send Messages`, `Read Message History`, `View Channels`\n" +
        "4. Copy the generated URL and open it in your browser\n" +
        "5. Choose the server and authorize\n\n" +
        "Optionally restrict the bot to a specific server by entering its Guild ID.",
      fields: ["guild_id"],
      validate: (draft) => {
        const guildId = draft.guild_id?.trim();
        if (!guildId) return null;
        if (!/^\d{17,20}$/.test(guildId)) return "Guild ID must be a 17-20 digit number. Right-click the server → Copy Server ID (enable Developer Mode in Settings → Advanced)";
        return null;
      },
      note: "Right-click a server → Copy Server ID (requires Developer Mode: Settings → Advanced → Developer Mode)",
    },
    {
      id: "access",
      title: "Access Control",
      description:
        "Control who can interact with the bot.\n\n" +
        "Enter **Discord user IDs** (comma-separated) to restrict access, or `*` to allow everyone.\n\n" +
        "**To find a user ID:** Enable Developer Mode (Settings → Advanced), then right-click a user → Copy User ID.\n\n" +
        "**Security note:** Leaving this empty will **deny all users** by default.",
      fields: ["allowed_users", "mention_only"],
      validate: (draft) => {
        const users = draft.allowed_users?.trim();
        if (!users) return null;
        if (users === "*") return null;
        const items = users.split(",").map((s) => s.trim()).filter(Boolean);
        for (const item of items) {
          if (!/^\d{17,20}$/.test(item)) return `Invalid user ID: "${item}". Must be a 17-20 digit Discord user ID`;
        }
        return null;
      },
      note: "Empty = deny all (safe default). Use * to allow everyone.",
    },
    {
      id: "verify",
      title: "Ready",
      description:
        "Discord bot is configured!\n\n" +
        "After finishing setup:\n" +
        "1. Check the bot appears online in your server\n" +
        "2. Mention the bot with `@BotName` in a channel or send it a DM\n" +
        "3. ClawDesk will receive and respond automatically\n\n" +
        "**Required intents:** Message Content, Guild Messages, Direct Messages.",
    },
  ],
};

const SLACK_JOURNEY: ChannelJourney = {
  channelType: "Slack",
  steps: [
    {
      id: "prereqs",
      title: "Create a Slack App",
      description:
        "1. Go to [api.slack.com/apps](https://api.slack.com/apps) and click **Create New App**\n" +
        "2. Choose **From scratch**, pick a name and workspace\n" +
        "3. Go to **Settings → Socket Mode** and **enable** it\n" +
        "4. Generate an **App-Level Token** with `connections:write` scope — copy it (starts with `xapp-`)",
      note: "Socket Mode is recommended — it avoids public URL requirements.",
    },
    {
      id: "scopes",
      title: "Bot Scopes & Install",
      description:
        "Add required bot token scopes under **OAuth & Permissions → Bot Token Scopes**:\n\n" +
        "- `chat:write` — Send messages\n" +
        "- `channels:read`, `channels:history` — Read public channels\n" +
        "- `groups:history` — Read private channels\n" +
        "- `im:history`, `im:read`, `im:write` — Direct messages\n" +
        "- `app_mentions:read` — Respond to @mentions\n" +
        "- `users:read` — Resolve user info\n\n" +
        "Then click **Install to Workspace** and copy the **Bot User OAuth Token** (`xoxb-...`).",
    },
    {
      id: "events",
      title: "Event Subscriptions",
      description:
        "Go to **Event Subscriptions** and subscribe to these bot events:\n\n" +
        "- `app_mention` — Bot is @mentioned\n" +
        "- `message.channels` — Messages in public channels\n" +
        "- `message.groups` — Messages in private channels\n" +
        "- `message.im` — Direct messages\n\n" +
        "Also enable the **Messages Tab** under **App Home** to allow DMs.",
    },
    {
      id: "configure",
      title: "Configure",
      description: "Enter your Slack tokens.",
      fields: ["bot_token", "app_token"],
      validate: (draft) => {
        const token = draft.bot_token?.trim();
        if (!token) return "Bot token is required";
        if (!token.startsWith("xoxb-")) return "Bot token must start with xoxb-";
        const appToken = draft.app_token?.trim();
        if (!appToken) return "App token is required for Socket Mode";
        if (!appToken.startsWith("xapp-")) return "App token must start with xapp-";
        return null;
      },
    },
    {
      id: "access",
      title: "Access Control",
      description:
        "Optionally restrict which users or channels the bot responds to.\n\n" +
        "**Channel ID:** Restrict to a single channel (right-click channel header → View channel details → copy ID from bottom).\n\n" +
        "**Allowed Users:** Enter Slack user IDs (comma-separated) or `*` for everyone.\n\n" +
        "**Security note:** Leaving allowed users empty will **deny all** by default.",
      fields: ["channel_id", "allowed_users"],
      validate: (draft) => {
        const chId = draft.channel_id?.trim();
        if (chId && !/^[A-Z0-9]{9,}$/.test(chId)) return "Channel ID should be an uppercase alphanumeric string like C01ABCDEFGH";
        return null;
      },
    },
    {
      id: "verify",
      title: "Ready",
      description:
        "Slack bot is configured!\n\n" +
        "After finishing setup:\n" +
        "1. Invite the bot to a channel: `/invite @yourbot`\n" +
        "2. Mention the bot: `@yourbot hello`\n" +
        "3. Or send a direct message to the bot\n\n" +
        "**Tip:** Check the **App Home → Messages Tab** is enabled for DMs to work.",
    },
  ],
};

const WHATSAPP_JOURNEY: ChannelJourney = {
  channelType: "WhatsApp",
  steps: [
    {
      id: "prereqs",
      title: "Prerequisites",
      description:
        "WhatsApp integration uses the **WhatsApp Business Cloud API**.\n\n" +
        "**You need:**\n" +
        "1. A [Meta Business account](https://business.facebook.com/)\n" +
        "2. A [Meta Developer account](https://developers.facebook.com/)\n" +
        "3. Create a new App → choose **Business** type\n" +
        "4. Add **WhatsApp** product to the app\n" +
        "5. Go to **WhatsApp → API Setup**\n\n" +
        "You'll get a temporary access token and test phone number to start.",
      note: "Meta provides a free test number and 1,000 free conversations/month.",
    },
    {
      id: "configure",
      title: "API Credentials",
      description: "Enter your WhatsApp Business API credentials from the Meta Developer Dashboard.",
      fields: ["phone_number_id", "access_token", "verify_token"],
      validate: (draft) => {
        if (!draft.phone_number_id?.trim()) return "Phone Number ID is required (found in WhatsApp → API Setup)";
        if (!/^\d+$/.test(draft.phone_number_id.trim())) return "Phone Number ID must be numeric";
        if (!draft.access_token?.trim()) return "Access token is required";
        return null;
      },
    },
    {
      id: "security",
      title: "Security & Access",
      description:
        "Configure webhook security and who can message the bot.\n\n" +
        "**App Secret:** Found in App Settings → Basic → App Secret. Used to verify webhook signatures.\n\n" +
        "**Allowed Numbers:** Enter phone numbers in E.164 format (e.g., `+1234567890`), or `*` for everyone.\n\n" +
        "**Security note:** Leaving allowed numbers empty will **deny all** by default.",
      fields: ["app_secret", "allowed_numbers"],
      validate: (draft) => {
        const nums = draft.allowed_numbers?.trim();
        if (!nums) return null;
        if (nums === "*") return null;
        const items = nums.split(",").map((s) => s.trim()).filter(Boolean);
        for (const item of items) {
          if (!/^\+\d{7,15}$/.test(item)) return `Invalid number: "${item}". Use E.164 format: +1234567890`;
        }
        return null;
      },
      note: "Empty = deny all. Phone numbers must include country code (e.g., +1 for US).",
    },
    {
      id: "webhook",
      title: "Webhook Setup",
      description:
        "To receive messages, configure a webhook in the Meta Developer Dashboard:\n\n" +
        "1. Go to **WhatsApp → Configuration → Webhook**\n" +
        "2. Set **Callback URL** to your ClawDesk instance URL + `/webhook/whatsapp`\n" +
        "3. Set **Verify Token** to the same value entered above\n" +
        "4. Subscribe to the `messages` webhook field\n\n" +
        "**For local development:** Use a tunnel like ngrok or Cloudflare Tunnel to expose your local instance.",
    },
    {
      id: "verify",
      title: "Ready",
      description:
        "WhatsApp channel is configured!\n\n" +
        "After finishing setup:\n" +
        "1. Send a message to the WhatsApp number from an allowed contact\n" +
        "2. ClawDesk will receive it via webhook and respond\n\n" +
        "**Note:** For production, generate a permanent access token (System User token) instead of the temporary one.",
    },
  ],
};

const EMAIL_JOURNEY: ChannelJourney = {
  channelType: "Email",
  steps: [
    {
      id: "prereqs",
      title: "Prerequisites",
      description:
        "To connect email, you need IMAP/SMTP credentials.\n\n" +
        "**Gmail:**\n" +
        "1. Enable 2-Step Verification on your Google account\n" +
        "2. Create an [App Password](https://myaccount.google.com/apppasswords)\n" +
        "3. Use `imap.gmail.com` / `smtp.gmail.com`\n\n" +
        "**Outlook/Microsoft 365:**\n" +
        "1. Use `outlook.office365.com` for both IMAP and SMTP\n" +
        "2. Create an app password if 2FA is enabled\n\n" +
        "**Other providers:** Check your email provider's IMAP/SMTP settings.",
      note: "Always use app passwords — never use your main password.",
    },
    {
      id: "servers",
      title: "Server Settings",
      description: "Enter your IMAP (incoming) and SMTP (outgoing) server details.",
      fields: ["imap_host", "imap_port", "smtp_host", "smtp_port"],
      validate: (draft) => {
        if (!draft.imap_host?.trim()) return "IMAP host is required";
        if (!draft.smtp_host?.trim()) return "SMTP host is required";
        const imapPort = draft.imap_port?.trim();
        if (imapPort) {
          const n = parseInt(imapPort, 10);
          if (isNaN(n) || n < 1 || n > 65535) return "IMAP port must be 1-65535";
        }
        const smtpPort = draft.smtp_port?.trim();
        if (smtpPort) {
          const n = parseInt(smtpPort, 10);
          if (isNaN(n) || n < 1 || n > 65535) return "SMTP port must be 1-65535";
        }
        return null;
      },
      note: "Default ports: IMAP 993 (TLS), SMTP 465 (TLS). All connections use TLS.",
    },
    {
      id: "credentials",
      title: "Credentials",
      description: "Enter your email account credentials and the folder to monitor.",
      fields: ["email", "password", "imap_folder"],
      validate: (draft) => {
        const email = draft.email?.trim();
        if (!email) return "Email address is required";
        if (!/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email)) return "Enter a valid email address";
        if (!draft.password?.trim()) return "Password is required (use an app password)";
        return null;
      },
      note: "The email address is used for both login and as the From address when sending replies.",
    },
    {
      id: "access",
      title: "Sender Allowlist",
      description:
        "Control which senders the agent responds to.\n\n" +
        "Enter email addresses or domain wildcards (comma-separated), or `*` for everyone.\n\n" +
        "Examples: `user@example.com`, `*@company.com`, `*`\n\n" +
        "**Security note:** Leaving this empty will **deny all senders** by default.",
      fields: ["allowed_senders"],
      validate: (draft) => {
        const senders = draft.allowed_senders?.trim();
        if (!senders) return null;
        if (senders === "*") return null;
        const items = senders.split(",").map((s) => s.trim()).filter(Boolean);
        for (const item of items) {
          if (item.startsWith("*@")) continue;
          if (/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(item)) continue;
          return `Invalid sender: "${item}". Use email (user@example.com) or domain wildcard (*@company.com)`;
        }
        return null;
      },
      note: "Empty = deny all. Use * to allow everyone. Domain wildcard: *@company.com",
    },
    {
      id: "verify",
      title: "Ready",
      description:
        "Email channel is configured!\n\n" +
        "ClawDesk uses **IMAP IDLE** for near-instant push notifications — no polling delay.\n\n" +
        "After finishing setup:\n" +
        "1. Send an email to the configured address from an allowed sender\n" +
        "2. ClawDesk will detect it instantly and reply\n\n" +
        "**Tip:** Use a dedicated email address for your AI agent to keep things organized.",
    },
  ],
};

const IMESSAGE_JOURNEY: ChannelJourney = {
  channelType: "IMessage",
  steps: [
    {
      id: "prereqs",
      title: "macOS Setup Required",
      description:
        "iMessage integration requires **macOS** and uses AppleScript + the Messages database.\n\n" +
        "**Before proceeding, ensure:**\n" +
        "1. You're running this on a **Mac** with Messages.app signed in\n" +
        "2. ClawDesk has **Full Disk Access** in System Settings → Privacy & Security\n" +
        "3. ClawDesk has **Automation** permission for Messages.app\n\n" +
        "Without Full Disk Access, ClawDesk cannot read the Messages database (`~/Library/Messages/chat.db`).",
      note: "System Settings → Privacy & Security → Full Disk Access → Add ClawDesk",
    },
    {
      id: "security",
      title: "Security & Contacts",
      description:
        "For security, you can restrict which contacts the agent will respond to.\n\n" +
        "- Leave blank to respond to **everyone** (wildcard `*`)\n" +
        "- Or enter specific phone numbers / email addresses\n\n" +
        "Phone numbers must include country code (e.g., `+1234567890`).\n" +
        "Email addresses must be valid Apple ID emails.",
      fields: ["allowed_contacts"],
      validate: (draft) => {
        const contacts = draft.allowed_contacts?.trim();
        if (!contacts) return null; // Empty = wildcard, which is fine
        const items = contacts.split(",").map((s) => s.trim()).filter(Boolean);
        for (const item of items) {
          if (item === "*") continue;
          if (item.startsWith("+") && /^\+\d{7,15}$/.test(item)) continue;
          if (/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(item)) continue;
          return `Invalid contact: "${item}". Use phone (+1234567890) or email (user@example.com)`;
        }
        return null;
      },
    },
    {
      id: "polling",
      title: "Polling Settings",
      description:
        "ClawDesk polls the Messages database for new messages at a configurable interval.\n\n" +
        "Lower values mean faster response times but slightly higher CPU usage.",
      fields: ["poll_interval_secs"],
      validate: (draft) => {
        const val = draft.poll_interval_secs?.trim();
        if (!val) return null; // Uses default of 3
        const n = parseInt(val, 10);
        if (isNaN(n) || n < 1) return "Poll interval must be at least 1 second";
        if (n > 60) return "Poll interval cannot exceed 60 seconds";
        return null;
      },
      note: "Default: 3 seconds. Recommended: 2-5 seconds.",
    },
    {
      id: "verify",
      title: "Ready",
      description:
        "iMessage channel is configured! After setup:\n\n" +
        "1. Send a message to this Mac from an allowed contact\n" +
        "2. ClawDesk will detect it within the poll interval\n" +
        "3. The agent will reply via AppleScript through Messages.app\n\n" +
        "**Note:** First-time macOS permissions may require you to approve a dialog.",
    },
  ],
};

const IRC_JOURNEY: ChannelJourney = {
  channelType: "Irc",
  steps: [
    {
      id: "server",
      title: "Server Connection",
      description:
        "Connect to an IRC server over TLS.\n\n" +
        "**Popular networks:**\n" +
        "- `irc.libera.chat` — Libera.Chat (open source projects)\n" +
        "- `irc.oftc.net` — OFTC (Debian, Tor)\n" +
        "- `irc.rizon.net` — Rizon\n" +
        "- `irc.undernet.org` — Undernet",
      fields: ["server", "port", "nickname"],
      validate: (draft) => {
        if (!draft.server?.trim()) return "Server hostname is required";
        if (!draft.nickname?.trim()) return "Nickname is required";
        const nick = draft.nickname.trim();
        if (!/^[A-Za-z_][A-Za-z0-9_\-\[\]\\^{}|`]{0,15}$/.test(nick))
          return "Invalid IRC nickname. Must start with a letter/underscore, max 16 chars.";
        const port = draft.port?.trim();
        if (port) {
          const n = parseInt(port, 10);
          if (isNaN(n) || n < 1 || n > 65535) return "Port must be 1-65535";
        }
        return null;
      },
      note: "Default port: 6697 (TLS). All connections use TLS encryption.",
    },
    {
      id: "channels",
      title: "Channels to Join",
      description:
        "Enter the IRC channels your bot should join.\n\n" +
        "Channel names start with `#`. Separate multiple channels with commas.",
      fields: ["channels"],
      validate: (draft) => {
        const ch = draft.channels?.trim();
        if (!ch) return null; // No channels = DM only
        const items = ch.split(",").map((s) => s.trim()).filter(Boolean);
        for (const item of items) {
          if (!item.startsWith("#")) return `Channel "${item}" must start with #`;
          if (item.length < 2) return "Channel name is too short";
          if (/\s/.test(item)) return `Channel "${item}" must not contain spaces`;
        }
        return null;
      },
      note: "Leave blank to only accept direct messages.",
    },
    {
      id: "auth",
      title: "Authentication (Optional)",
      description:
        "If the server requires authentication, configure it here.\n\n" +
        "**SASL PLAIN** is the recommended method for modern IRC networks.\n" +
        "**NickServ** is a fallback for networks that don't support SASL.\n\n" +
        "Leave both blank if the server allows unauthenticated connections.",
      fields: ["sasl_password", "nickserv_password"],
      validate: () => null,
      note: "Only one authentication method is needed. SASL is preferred.",
    },
    {
      id: "verify",
      title: "Ready",
      description:
        "IRC bot is configured! After setup, it will:\n\n" +
        "1. Connect to the server over TLS\n" +
        "2. Authenticate (if configured)\n" +
        "3. Join the specified channels\n" +
        "4. Respond to messages mentioning the bot's nickname\n\n" +
        "**Tip:** You can also DM the bot directly on IRC.",
    },
  ],
};

const SIGNAL_JOURNEY: ChannelJourney = {
  channelType: "Signal",
  steps: [
    {
      id: "config",
      title: "Signal Connection",
      description:
        "Connect to Signal via signal-cli's JSON-RPC interface.\n\n" +
        "**Prerequisites:**\n" +
        "1. Install [signal-cli](https://github.com/AsamK/signal-cli)\n" +
        "2. Register/link a phone number\n" +
        "3. Start the JSON-RPC daemon: `signal-cli -u +1234567890 jsonRpc`",
      fields: ["phone_number", "rpc_endpoint"],
      validate: (draft) => {
        if (!draft.phone_number?.trim()) return "Phone number is required (e.g., +1234567890)";
        return null;
      },
      note: "Default RPC endpoint: http://localhost:8080/api/v1/rpc",
    },
  ],
};

const MATRIX_JOURNEY: ChannelJourney = {
  channelType: "Matrix",
  steps: [
    {
      id: "config",
      title: "Matrix Homeserver",
      description:
        "Connect to a Matrix homeserver (Element, Synapse, Conduit, etc.).\n\n" +
        "Create a bot user on your homeserver and generate an access token.",
      fields: ["homeserver_url", "user_id", "access_token"],
      validate: (draft) => {
        if (!draft.homeserver_url?.trim()) return "Homeserver URL is required (e.g., https://matrix.org)";
        if (!draft.user_id?.trim()) return "User ID is required (e.g., @bot:matrix.org)";
        if (!draft.access_token?.trim()) return "Access token is required";
        return null;
      },
    },
  ],
};

const TEAMS_JOURNEY: ChannelJourney = {
  channelType: "Teams",
  steps: [
    {
      id: "config",
      title: "Microsoft Teams Bot",
      description:
        "Connect via Azure Bot Framework.\n\n" +
        "**Setup:**\n" +
        "1. Register a bot in the [Azure Portal](https://portal.azure.com)\n" +
        "2. Get the App ID and Secret from the bot registration\n" +
        "3. Configure the messaging endpoint to point to your ClawDesk instance",
      fields: ["app_id", "app_secret", "tenant_id"],
      validate: (draft) => {
        if (!draft.app_id?.trim()) return "App ID is required";
        if (!draft.app_secret?.trim()) return "App Secret is required";
        return null;
      },
    },
  ],
};

const MASTODON_JOURNEY: ChannelJourney = {
  channelType: "Mastodon",
  steps: [
    {
      id: "config",
      title: "Mastodon Instance",
      description:
        "Connect to a Mastodon/Fediverse instance.\n\n" +
        "**Setup:**\n" +
        "1. Go to your instance Settings → Development → New Application\n" +
        "2. Grant `read` and `write` scopes\n" +
        "3. Copy the access token",
      fields: ["instance_url", "access_token"],
      validate: (draft) => {
        if (!draft.instance_url?.trim()) return "Instance URL is required (e.g., https://mastodon.social)";
        if (!draft.access_token?.trim()) return "Access token is required";
        return null;
      },
    },
  ],
};

const WEBHOOK_JOURNEY: ChannelJourney = {
  channelType: "Webhook",
  steps: [
    {
      id: "config",
      title: "Webhook Endpoint",
      description:
        "Configure a generic webhook for custom integrations.\n\n" +
        "Inbound: POST JSON `{\"text\": \"...\", \"sender\": \"...\"}` to ClawDesk.\n" +
        "Outbound: ClawDesk POSTs responses to your callback URL.",
      fields: ["callback_url", "shared_secret", "listen_port"],
      validate: (draft) => {
        if (!draft.callback_url?.trim()) return "Callback URL is required";
        return null;
      },
      note: "Default listen port: 9090. Set a shared secret for HMAC validation.",
    },
  ],
};

/** Registry of all channel journeys. */
const JOURNEY_REGISTRY: Record<string, ChannelJourney> = {
  Telegram: TELEGRAM_JOURNEY,
  Discord: DISCORD_JOURNEY,
  Slack: SLACK_JOURNEY,
  WhatsApp: WHATSAPP_JOURNEY,
  Email: EMAIL_JOURNEY,
  IMessage: IMESSAGE_JOURNEY,
  Irc: IRC_JOURNEY,
  Signal: SIGNAL_JOURNEY,
  Matrix: MATRIX_JOURNEY,
  Teams: TEAMS_JOURNEY,
  Mastodon: MASTODON_JOURNEY,
  Webhook: WEBHOOK_JOURNEY,
};

/** Get the journey for a channel type, or null if generic. */
export function getJourney(channelType: string): ChannelJourney | null {
  return JOURNEY_REGISTRY[channelType] ?? null;
}

// ═══════════════════════════════════════════════════════════════
// Channel Setup Journey UI Component
// ═══════════════════════════════════════════════════════════════

interface ChannelSetupJourneyProps {
  /** The channel type spec with config fields. */
  spec: ChannelTypeSpec;
  /** Initial values for config fields. */
  initialValues?: Record<string, string>;
  /** Called when the user completes the journey. */
  onComplete: (config: Record<string, string>) => void;
  /** Called when the user cancels. */
  onCancel: () => void;
}

/**
 * Renders a guided multi-step setup flow for a channel type.
 *
 * If a journey is registered for the channel type, it shows step-by-step
 * guidance with prerequisites, field groups, and validation.
 *
 * If no journey exists, falls back to a simple single-page form with all fields.
 */
export function ChannelSetupJourney({ spec, initialValues, onComplete, onCancel }: ChannelSetupJourneyProps) {
  const journey = useMemo(() => getJourney(spec.id), [spec.id]);
  const [currentStep, setCurrentStep] = useState(0);
  const [draft, setDraft] = useState<Record<string, string>>({ ...(initialValues ?? {}) });
  const [error, setError] = useState<string | null>(null);

  const steps = journey?.steps ?? [];
  const totalSteps = steps.length;
  const step = steps[currentStep];

  const updateField = useCallback((key: string, value: string) => {
    setDraft((prev) => ({ ...prev, [key]: value }));
    setError(null);
  }, []);

  const handleNext = useCallback(() => {
    if (!step) return;
    if (step.validate) {
      const err = step.validate(draft);
      if (err) {
        setError(err);
        return;
      }
    }
    setError(null);
    if (currentStep < totalSteps - 1) {
      setCurrentStep((s) => s + 1);
    } else {
      onComplete(draft);
    }
  }, [step, draft, currentStep, totalSteps, onComplete]);

  const handleBack = useCallback(() => {
    setError(null);
    if (currentStep > 0) {
      setCurrentStep((s) => s - 1);
    } else {
      onCancel();
    }
  }, [currentStep, onCancel]);

  // ── Fallback: simple form (no journey defined) ──
  if (!journey || totalSteps === 0) {
    return (
      <div className="channel-journey">
        <button className="btn ghost" onClick={onCancel} style={{ alignSelf: "flex-start", marginBottom: "0.5rem" }}>
          ← Back to channels
        </button>
        <h3>{spec.icon} Configure {spec.label}</h3>
        <p className="onboarding-hint">{spec.blurb}</p>

        {spec.configFields.length === 0 ? (
          <p className="onboarding-hint">This channel has no configuration — it's ready to use.</p>
        ) : (
          spec.configFields.map((field) => (
            <ConfigField key={field.key} field={field} value={draft[field.key] ?? ""} onChange={(v) => updateField(field.key, v)} />
          ))
        )}

        <div className="row-actions" style={{ justifyContent: "flex-end", marginTop: "0.75rem" }}>
          <button className="btn ghost" onClick={onCancel}>Cancel</button>
          <button className="btn primary" onClick={() => onComplete(draft)}>
            {spec.configFields.length === 0 ? "Add" : "Save & Add"}
          </button>
        </div>
      </div>
    );
  }

  // ── Journey: step-by-step guided flow ──
  const isLastStep = currentStep === totalSteps - 1;
  const stepFields = step?.fields
    ? spec.configFields.filter((f) => step.fields!.includes(f.key))
    : [];

  return (
    <div className="channel-journey">
      {/* Back navigation */}
      <button className="btn ghost" onClick={handleBack} style={{ alignSelf: "flex-start", marginBottom: "0.5rem" }}>
        ← {currentStep === 0 ? "Back to channels" : "Back"}
      </button>

      {/* Channel header */}
      <div className="channel-journey-header">
        <span className="channel-journey-icon">{spec.icon}</span>
        <div>
          <h3 style={{ margin: 0 }}>{spec.label} Setup</h3>
          <span className="row-sub">{spec.blurb}</span>
        </div>
      </div>

      {/* Step progress indicator */}
      <div className="wizard-steps channel-journey-steps">
        {steps.map((s, i) => (
          <span
            key={s.id}
            className={`${i === currentStep ? "active" : ""} ${i < currentStep ? "done" : ""}`}
            title={s.title}
          >
            {i + 1}. {s.title}
          </span>
        ))}
      </div>

      {/* Step content */}
      <section className="section-card channel-journey-content">
        <h4 className="channel-journey-step-title">{step.title}</h4>

        {/* Description with simple markdown rendering */}
        <div className="channel-journey-description">
          {step.description.split("\n").map((line, i) => {
            if (!line.trim()) return <br key={i} />;
            // Bold text
            const rendered = line.replace(/\*\*(.+?)\*\*/g, "<strong>$1</strong>");
            // Code inline
            const withCode = rendered.replace(/`(.+?)`/g, "<code>$1</code>");
            // Links
            const withLinks = withCode.replace(
              /\[(.+?)\]\((.+?)\)/g,
              '<a href="$2" target="_blank" rel="noopener noreferrer">$1</a>'
            );
            return <p key={i} dangerouslySetInnerHTML={{ __html: withLinks }} />;
          })}
        </div>

        {/* Config fields for this step */}
        {stepFields.length > 0 && (
          <div className="channel-journey-fields">
            {stepFields.map((field) => (
              <ConfigField
                key={field.key}
                field={field}
                value={draft[field.key] ?? ""}
                onChange={(v) => updateField(field.key, v)}
              />
            ))}
          </div>
        )}

        {/* Validation error */}
        {error && (
          <div className="channel-journey-error">
            <span className="text-error">⚠ {error}</span>
          </div>
        )}

        {/* Step note */}
        {step.note && (
          <div className="channel-journey-note">
            <span className="row-sub">ⓘ {step.note}</span>
          </div>
        )}
      </section>

      {/* Navigation */}
      <div className="row-actions" style={{ justifyContent: "flex-end", marginTop: "0.75rem" }}>
        <button className="btn ghost" onClick={handleBack}>
          {currentStep === 0 ? "Cancel" : "Back"}
        </button>
        <button className="btn primary" onClick={handleNext}>
          {isLastStep ? "Finish Setup" : "Continue"}
        </button>
      </div>
    </div>
  );
}

// ── Reusable config field renderer ──
function ConfigField({
  field,
  value,
  onChange,
}: {
  field: ChannelConfigField;
  value: string;
  onChange: (value: string) => void;
}) {
  return (
    <label className="field-label">
      {field.label} {field.required && <span style={{ color: "var(--danger)" }}>*</span>}
      {field.type === "select" ? (
        <select value={value} onChange={(e) => onChange(e.target.value)}>
          <option value="">Select...</option>
          {(field.options ?? []).map((opt) => (
            <option key={opt}>{opt}</option>
          ))}
        </select>
      ) : (
        <input
          type={field.type === "password" ? "password" : "text"}
          value={value}
          placeholder={field.placeholder ?? ""}
          onChange={(e) => onChange(e.target.value)}
        />
      )}
      {field.help && <span className="row-sub" style={{ display: "block", marginTop: 2 }}>{field.help}</span>}
    </label>
  );
}
