# Skills & Plugins

ClawDesk uses two extension systems: **Skills** (composable agent capabilities) and **Plugins** (lifecycle hooks with sandbox isolation).

---

## Skills

A skill is a typed unit of agent capability consisting of a prompt fragment, tool bindings, parameter schema, and dependency declarations.

### Skill Structure

```yaml
# ~/.clawdesk/skills/my-skill/manifest.toml
[manifest]
id = "my-skill"
display_name = "My Custom Skill"
version = "1.0.0"
description = "Does something useful"
author = "you"

[trigger]
keywords = ["analyze", "review", "check"]
patterns = ["analyze\\s+\\w+", "review\\s+.*"]
always_active = false

[parameters]
style = { type = "string", default = "detailed", description = "Output style" }
max_items = { type = "integer", default = 10, description = "Maximum items to process" }

[dependencies]
requires = []  # Skill IDs this skill depends on
conflicts = [] # Skill IDs that conflict
```

The **prompt fragment** lives alongside the manifest:

```markdown
<!-- ~/.clawdesk/skills/my-skill/prompt.md -->
## My Custom Skill

When the user asks you to analyze or review something:
1. Break down the input into components
2. Evaluate each component against best practices
3. Provide a structured report with recommendations

Use the {{style}} output format, limiting to {{max_items}} items.
```

### Built-in Skills

ClawDesk ships with 15+ built-in skills:

| Skill | Description |
|-------|-------------|
| Code Review | Analyzes code for quality, bugs, and improvements |
| Writing Assistant | Helps with writing, editing, and tone adjustment |
| Research | Structured research with source tracking |
| Summarizer | Multi-level summarization |
| Data Analysis | Statistical analysis and visualization |
| Translator | Multi-language translation |
| Debugger | Step-by-step debugging assistance |
| Planner | Project planning and task breakdown |
| Educator | Teaching with examples and exercises |
| Designer | UI/UX design guidance |

### Skill Registry

`SkillRegistry` manages skill lifecycle:

```rust
// In-memory registry with O(1) lookup via FxHashMap
let registry = SkillRegistry::new();

// Register a skill
registry.register(skill)?;

// Get active skills for an agent
let active = registry.active_skills();

// Filter skills by agent binding
let agent_skills = registry.skills_for_agent(&agent_id);
```

### Skill Selection Pipeline

When a message arrives, skills are selected through a multi-stage pipeline:

```
User Message
    │
    ▼
┌─ Trigger Evaluation ──────────────────────┐
│  For each registered skill:                │
│  • Check keyword matches against message   │
│  • Check regex pattern matches             │
│  • Check always_active flag                │
│  • Score: relevance = match_count / total  │
│  Result: Vec<(Skill, relevance_score)>     │
└─────────────┬─────────────────────────────┘
              │
              ▼
┌─ Relevance Sort ──────────────────────────┐
│  Sort by: relevance × priority_weight     │
│  • Trigger-matched skills get 2× weight   │
│  • Higher relevance = higher placement    │
└─────────────┬─────────────────────────────┘
              │
              ▼
┌─ Token Budget Knapsack ───────────────────┐
│  Budget = 20% of context_limit            │
│  Greedy: pick highest-value skill that    │
│  fits remaining budget. Repeat.           │
│                                           │
│  Each skill's "cost" = token count of     │
│  its prompt fragment.                     │
│                                           │
│  Selected skills → SkillInjection         │
│  Excluded skills → logged with reason     │
└─────────────┬─────────────────────────────┘
              │
              ▼
┌─ Prompt Injection ────────────────────────┐
│  Selected skill fragments are appended    │
│  to the system prompt in priority order   │
│                                           │
│  System prompt = identity + skills +      │
│                  runtime + safety         │
└───────────────────────────────────────────┘
```

### Skill Orchestrator

`SkillOrchestrator` provides the `SkillProvider` trait implementation:

```rust
#[async_trait]
pub trait SkillProvider: Send + Sync {
    async fn select_skills(
        &self,
        user_message: &str,
        session_id: &str,
        channel_id: Option<&str>,
        turn: usize,
        skill_budget: usize,
    ) -> SkillInjection;
}
```

The `TurnContext` provides contextual information for trigger evaluation:
- `channel_id` — Which channel the message came from
- `message_keywords` — Extracted keywords from user message
- `current_time` — For time-based triggers
- `triggered_this_turn` — Skills already triggered (deduplication)

### Skill Verification

`SkillVerifier` assigns trust levels to skills:

| Trust Level | Description |
|-------------|-------------|
| `Builtin` | Ships with ClawDesk (highest trust) |
| `Verified` | Cryptographically signed by known author |
| `Community` | From community registry, user-approved |
| `Local` | User-created, filesystem only |
| `Unknown` | No verification (lowest trust) |

### Skill Lifecycle

```
Discover (filesystem/registry)
    → Load manifest + prompt
    → Resolve dependencies (topological sort)
    → Verify trust level
    → Register in SkillRegistry
    → Available for selection
```

### Environment Injection

Skills can declare environment variables. `EnvGuard` (RAII) manages injection:

```
Skill declares: env.API_KEY = "${CUSTOM_SERVICE_KEY}"
    │
    ▼
EnvResolver merges: OS env → Global config → Per-skill config
    │
    ▼
EnvGuard::apply() sets environment variables
    │
    ▼
Skill executes with injected env
    │
    ▼
EnvGuard::drop() restores original env (RAII cleanup)
```

### Skill Management Commands

| Command | Description |
|---------|-------------|
| `list_skills` | List all registered skills |
| `activate_skill` | Activate a skill for use |
| `deactivate_skill` | Deactivate a skill |
| `register_skill` | Register a new skill from filesystem |
| `delete_skill` | Remove a skill |
| `get_skill_detail` | Get full skill information |
| `validate_skill` | Validate a skill manifest |

### Advanced Skill Features

#### Federated Registry
`FederatedRegistry` enables skill discovery from multiple sources:
- **Local filesystem** — `~/.clawdesk/skills/`
- **Content-addressed** — SHA256-based deduplication
- **Remote registries** — Fetch skills from external sources
- **Priority ordering** — Local overrides remote

#### Promotion Pipeline
`PromotionPipeline` manages skill lifecycle stages:
```
Draft → Review → Testing → Staging → Production
```
With rollback support at each stage.

#### Hot Reload
`SkillWatcher` monitors the skills directory for changes:
- Filesystem notifications trigger re-load
- Modified skills are re-validated and re-registered
- No restart required

---

## Plugins

Plugins hook into ClawDesk's lifecycle phases to intercept, modify, or extend behavior.

### Plugin Manifest

```rust
pub struct PluginManifest {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub dependencies: Vec<String>,  // Other plugin names
    pub capabilities: Vec<String>,  // Required capabilities
}
```

### Plugin Trait

```rust
#[async_trait]
pub trait ClawDeskPlugin: Send + Sync {
    fn manifest(&self) -> &PluginManifest;
    async fn on_load(&mut self, ctx: &PluginContext) -> Result<(), PluginSdkError>;
    async fn on_unload(&mut self) -> Result<(), PluginSdkError>;
    async fn on_event(&mut self, event: PluginEvent) -> PluginResponse;
}
```

### Hook System

Plugins register hooks that fire at specific lifecycle phases:

#### Phases

| Phase | When | Can Mutate |
|-------|------|------------|
| `Boot` | System startup | config |
| `SessionStart` | New chat session begins | session config |
| `BeforeAgentStart` | Before agent run begins | model, prompt, tools |
| `MessageReceive` | Inbound message arrives | message, routing |
| `BeforeLlmCall` | Before sending to LLM | prompt, parameters |
| `AfterLlmCall` | After LLM responds | response content |
| `AfterToolCall` | After tool execution | tool result |
| `BeforeCompaction` | Before context compaction | compaction strategy |
| `AfterCompaction` | After compaction finishes | compacted history |
| `MessageSend` | Before outbound delivery | response, formatting |
| `SessionEnd` | Chat session closing | session summary |
| `Shutdown` | System shutting down | cleanup |

#### Hook Implementation

```rust
pub struct MyHook;

#[async_trait]
impl Hook for MyHook {
    fn name(&self) -> &str { "my-hook" }
    
    fn phases(&self) -> Vec<Phase> {
        vec![Phase::BeforeAgentStart, Phase::MessageSend]
    }
    
    fn priority(&self) -> Priority { 50 }  // Lower = runs first
    
    async fn execute(&self, ctx: HookContext) -> HookResult {
        // Inspect context
        let model = ctx.data.get("model");
        
        // Set typed overrides
        let ctx = ctx
            .override_model("claude-haiku")
            .append_to_prompt("Keep it concise.")
            .suppress_tools(vec!["shell_exec".into()]);
        
        HookResult::Continue(ctx)
        // Or: HookResult::ShortCircuit(ctx) to stop chain
        // Or: HookResult::Error("reason".into()) to log + continue
    }
}
```

#### Hook Overrides (Typed)

`HookOverrides` provides typed mutation fields instead of untyped JSON:

| Override | Type | Effect |
|----------|------|--------|
| `model` | `Option<String>` | Switch LLM model |
| `system_prompt_prepend` | `Option<String>` | Prepend to system prompt |
| `system_prompt_append` | `Option<String>` | Append to system prompt |
| `inject_tools` | `Vec<String>` | Activate additional tools |
| `suppress_tools` | `Vec<String>` | Remove tools from available set |
| `max_tool_rounds` | `Option<usize>` | Override tool round limit |
| `response_prepend` | `Option<String>` | Prepend to outgoing response |
| `response_append` | `Option<String>` | Append to outgoing response |

Overrides from multiple hooks in a chain are merged:
- Scalar fields: last hook wins
- List fields: concatenated

#### Chain of Responsibility

Hooks execute in priority order (lowest first). Each hook can:
- **Continue** — Pass modified context to next hook
- **Short-circuit** — Cancel remaining hooks (and potentially the operation)
- **Error** — Log the error and continue the chain

### Plugin Host

`PluginHost` manages the plugin lifecycle:

```
Discover plugins (filesystem scan)
    │
    ▼
Load manifests + validate
    │
    ▼
Resolve dependencies (topological sort via DependencyResolver)
    │
    ▼
Activate in dependency order
    │
    ▼
Register hooks with HookManager
    │
    ▼
Plugins active — hooks fire on lifecycle events
```

### Plugin Sandbox

`PluginSandbox` enforces resource limits:

| Resource | Limit |
|----------|-------|
| Memory | Configurable per plugin |
| CPU time | Per-execution timeout |
| Network | Can be restricted |
| Filesystem | Confined to plugin directory |

### Plugin Management Commands

| Command | Description |
|---------|-------------|
| `list_plugins` | List all discovered plugins |
| `get_plugin_info` | Get detailed plugin information |
| `enable_plugin` | Activate a plugin |
| `disable_plugin` | Deactivate a plugin |

### Plugin Registry

`PluginRegistry` tracks plugin state:
- Discovery status (found, loaded, activated, error)
- Hook registrations
- Resource usage
- Health status
