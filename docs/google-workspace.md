# Google Workspace Integration

ClawDesk ships with built-in Google Workspace support via the [gws CLI](https://github.com/googleworkspace/cli) (Apache-2.0, by Google LLC).

## What's Included

- **41 agent skills** covering Drive, Gmail, Calendar, Sheets, Docs, Slides, Tasks, People, Chat, Forms, Keep, Meet, Admin Reports, and multi-service workflows
- **gws binary** ships as a Tauri sidecar (built from source, 11MB)
- **OAuth integration** via ClawDesk's Extensions UI

## Quick Setup

### Option 1: Use `gws auth setup` (Easiest — requires gcloud)

```bash
# From terminal (or ClawDesk's built-in shell):
gws auth setup       # Creates GCP project, enables APIs, logs you in
gws auth status      # Verify authentication
```

### Option 2: Manual OAuth (No gcloud required)

1. Go to [Google Cloud Console](https://console.cloud.google.com/apis/credentials)
2. Create an OAuth 2.0 Client ID (Desktop app type)
3. Download the client secret JSON
4. In ClawDesk **Extensions** page → click **google-workspace** → **Configure**
5. Paste your Client ID and Client Secret
6. Click **OAuth** to authenticate

### Option 3: Direct Token

```bash
export GOOGLE_WORKSPACE_CLI_TOKEN="ya29.your-access-token"
```

## Shared Client ID

All Google integrations (Gmail, Drive, Calendar, Google Workspace) use the **same `GOOGLE_CLIENT_ID`**. Configure it once, and all Google services share the credential.

| Extension | Client ID Field | Scopes |
|-----------|----------------|--------|
| google-workspace | `GOOGLE_CLIENT_ID` | drive, gmail, calendar, sheets, docs |
| gmail | `GOOGLE_CLIENT_ID` | gmail.modify |
| google-drive | `GOOGLE_CLIENT_ID` | drive |
| google-calendar | `GOOGLE_CLIENT_ID` | calendar |

## What Agents Can Do

Once authenticated, agents can:

### Gmail
```
"Check my unread emails"
"Send an email to alice@company.com about the quarterly report"
"Triage my inbox — archive promotions and label important ones"
"Reply to Bob's last email with the meeting notes"
```

### Google Drive
```
"Find my Q4 report in Drive"
"Upload this file to the shared folder"
"Share the document with the team"
"Search Drive for spreadsheets modified this week"
```

### Google Calendar
```
"What's on my calendar today?"
"Schedule a team standup at 9am tomorrow"
"Find free time slots for a meeting with Alice and Bob"
"Cancel the 3pm meeting"
```

### Google Sheets
```
"Read the sales data from the Q4 spreadsheet"
"Append these new rows to the tracking sheet"
"Create a new spreadsheet for project tracking"
```

### Multi-Service Workflows
```
"Convert that email from the client into a task"
"Prepare materials for my next meeting"
"Give me a weekly digest of my emails, events, and tasks"
"Send a team announcement in Chat and schedule a follow-up event"
```

## Scope Management

For personal @gmail.com accounts (unverified OAuth apps), use limited scopes:

```bash
gws auth login -s drive,gmail,sheets,calendar
```

For Google Workspace accounts (verified apps), use the full scope set:

```bash
gws auth login  # All 85+ scopes
```

## Credential Security

- Credentials encrypted at rest with **AES-256-GCM**
- Encryption key stored in **OS keyring** (macOS Keychain, Windows Credential Manager, Linux Secret Service)
- File fallback: `~/.config/gws/.encryption_key` with `0o600` permissions
- Tokens auto-refresh when expired

## Building from Source

The gws binary is built alongside ClawDesk:

```bash
# Build gws for current platform
./scripts/build-gws.sh

# Build for release
./scripts/build-gws.sh --release

# Download from GitHub Releases instead of building
./scripts/download-gws.sh --download
```

## Attribution

The `gws` CLI is maintained by Google LLC under the Apache License 2.0.
- Repository: https://github.com/googleworkspace/cli
- Author: Justin Poehnelt
- License: Apache-2.0

ClawDesk ships the unmodified gws binary alongside its own distribution.
The GWS-LICENSE file is included with every ClawDesk installation.
