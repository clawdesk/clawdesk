# Voice & TTS

ClawDesk supports multi-provider speech synthesis via the `clawdesk-voice` crate.

## Providers

| Provider | Quality | Latency | Cost | API Key Required |
|----------|---------|---------|------|------------------|
| ElevenLabs | High | Medium | $$ | Yes |
| OpenAI TTS | Good | Low | $ | Yes |
| Edge TTS | Fair | Low | Free | No |

## Configuration

```yaml
voice:
  default_provider: elevenlabs
  default_voice: rachel
  format: mp3
  chunk_samples: 4800  # 200ms at 24kHz

  elevenlabs:
    api_key: ${ELEVENLABS_API_KEY}
    stability: 0.5       # 0.0-1.0
    similarity_boost: 0.75
    speed: 1.0           # 0.5-2.0

  openai:
    api_key: ${OPENAI_API_KEY}
    voice: alloy          # alloy, echo, fable, onyx, nova, shimmer

  edge_tts:
    voice: en-US-GuyNeural
```

## Provider Selection

The engine selects the best available provider using weighted scoring:

```
score = α·Quality - β·Latency - γ·Cost
```

Default weights: α=0.5, β=0.3, γ=0.2. If the selected provider fails, the next-best is tried automatically.

## Compile-Time Parameter Safety

Parameters like ElevenLabs `stability` are validated at construction time via Rust newtypes:

```rust
let stability = Stability::new(0.5)?;  // Ok — within [0.0, 1.0]
let bad = Stability::new(1.5);         // Err — out of range
```

## VoiceWake

VoiceWake enables hands-free operation by listening for trigger phrases. Configuration is in `clawdesk-infra` (the `voice_wake` module):

```yaml
voice_wake:
  enabled: true
  wake_phrases: ["hey llama", "computer"]
  target_agent: default
  silence_timeout_secs: 3
```

## Voice Calls

The `VoiceCallPlugin` provides a state machine for voice call integration:

```
Idle → Ringing → Active → OnHold → Ended
```
