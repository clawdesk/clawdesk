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
// Modelled after openclaw's per-channel ChannelOnboardingAdapter
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
      title: "Prerequisites",
      description:
        "To connect Telegram, you need a **Bot Token** from @BotFather.\n\n" +
        "1. Open Telegram and search for **@BotFather**\n" +
        "2. Send `/newbot` and follow the prompts\n" +
        "3. Copy the API token you receive",
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
      id: "verify",
      title: "Ready",
      description:
        "Your Telegram bot is ready to connect. After finishing setup, send a message to your bot to test it.\n\n" +
        "**Tip:** Use `/setdescription` and `/setabouttext` with @BotFather to customize your bot's profile.",
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
        "3. Go to the **Bot** section and click **Add Bot**\n" +
        "4. Enable **Message Content Intent** under Privileged Gateway Intents\n" +
        "5. Copy the bot token",
      note: "You'll also need to invite the bot to your server using an OAuth2 URL with the `bot` scope.",
    },
    {
      id: "configure",
      title: "Configure",
      description: "Enter your Discord bot token.",
      fields: ["bot_token"],
      validate: (draft) => {
        const token = draft.bot_token?.trim();
        if (!token) return "Bot token is required";
        if (token.length < 50) return "Token seems too short";
        return null;
      },
    },
    {
      id: "verify",
      title: "Ready",
      description:
        "Discord bot is configured. After setup, invite the bot to your server and mention it or DM it to test.\n\n" +
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
        "3. Under **OAuth & Permissions**, add bot token scopes:\n" +
        "   - `chat:write`, `channels:read`, `groups:read`, `im:read`, `im:write`\n" +
        "4. Install the app to your workspace\n" +
        "5. Copy the **Bot User OAuth Token** (starts with `xoxb-`)",
    },
    {
      id: "configure",
      title: "Configure",
      description: "Enter your Slack bot token and optional signing secret.",
      fields: ["bot_token", "signing_secret"],
      validate: (draft) => {
        const token = draft.bot_token?.trim();
        if (!token) return "Bot token is required";
        if (!token.startsWith("xoxb-")) return "Slack bot tokens start with xoxb-";
        return null;
      },
    },
    {
      id: "verify",
      title: "Ready",
      description:
        "Slack bot is configured. After setup, invite the bot to a channel with `/invite @yourbot` and mention it to test.",
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
        "WhatsApp integration uses the WhatsApp Business API.\n\n" +
        "**You need:**\n" +
        "- A Meta Business account\n" +
        "- A WhatsApp Business API phone number\n" +
        "- Access token from the Meta developer portal\n\n" +
        "Visit [developers.facebook.com](https://developers.facebook.com/) to get started.",
    },
    {
      id: "configure",
      title: "Configure",
      description: "Enter your WhatsApp Business API credentials.",
      fields: ["phone_number_id", "access_token", "verify_token"],
      validate: (draft) => {
        if (!draft.access_token?.trim()) return "Access token is required";
        if (!draft.phone_number_id?.trim()) return "Phone number ID is required";
        return null;
      },
    },
    {
      id: "verify",
      title: "Ready",
      description: "WhatsApp channel is configured. You'll need to set up a webhook URL pointing to your ClawDesk instance.",
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
        "**For Gmail:** Enable \"Less secure apps\" or create an [App Password](https://myaccount.google.com/apppasswords).\n\n" +
        "**For Outlook:** Use your Microsoft account credentials or create an app password if 2FA is enabled.",
      note: "App passwords are recommended over your regular password.",
    },
    {
      id: "configure",
      title: "Credentials",
      description: "Enter your email account credentials.",
      fields: ["imap_host", "smtp_host", "email", "password"],
      validate: (draft) => {
        const email = draft.email?.trim();
        if (!email) return "Email address is required";
        if (!email.includes("@")) return "Enter a valid email address";
        if (!draft.password?.trim()) return "Password is required";
        return null;
      },
    },
    {
      id: "verify",
      title: "Ready",
      description:
        "Email channel is configured. ClawDesk will poll for new emails and respond automatically.\n\n" +
        "**Tip:** Consider using a dedicated email address for your AI agent.",
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

/** Registry of all channel journeys. */
const JOURNEY_REGISTRY: Record<string, ChannelJourney> = {
  Telegram: TELEGRAM_JOURNEY,
  Discord: DISCORD_JOURNEY,
  Slack: SLACK_JOURNEY,
  WhatsApp: WHATSAPP_JOURNEY,
  Email: EMAIL_JOURNEY,
  IMessage: IMESSAGE_JOURNEY,
  Irc: IRC_JOURNEY,
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
