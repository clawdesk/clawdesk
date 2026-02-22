import { useState, useRef, useCallback, useEffect } from "react";
import { Icon } from "./Icon";
import { startVoiceRecording, stopVoiceRecording, cancelVoiceRecording } from "../api";

/**
 * VoiceInput — microphone button that records audio via native cpal
 * (Rust-side), then sends the WAV to local Whisper for transcription.
 *
 * All audio capture happens in Rust because Tauri's WebView doesn't
 * expose `navigator.mediaDevices`. The frontend only invokes
 * start/stop/cancel commands via Tauri IPC.
 *
 * Props:
 *  - onTranscription(text): called with the final transcribed text
 *  - disabled: disable the button (e.g. while sending a message)
 */

type VoiceState = "idle" | "recording" | "transcribing" | "error";

interface VoiceInputProps {
  onTranscription: (text: string) => void;
  disabled?: boolean;
}

export function VoiceInput({ onTranscription, disabled }: VoiceInputProps) {
  const [state, setState] = useState<VoiceState>("idle");
  const [elapsed, setElapsed] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const timerRef = useRef<ReturnType<typeof setInterval> | null>(null);

  // Cleanup on unmount
  useEffect(() => {
    return () => {
      if (timerRef.current) clearInterval(timerRef.current);
      // Cancel any in-progress recording
      cancelVoiceRecording().catch(() => {});
    };
  }, []);

  const startRecording = useCallback(async () => {
    try {
      setError(null);
      const resp = await startVoiceRecording();
      if (!resp.success) {
        setError(resp.error || "Failed to start recording");
        setState("error");
        setTimeout(() => setState("idle"), 3000);
        return;
      }

      setState("recording");
      setElapsed(0);

      // Elapsed timer
      timerRef.current = setInterval(() => {
        setElapsed((prev) => prev + 1);
      }, 1000);
    } catch (err: any) {
      console.error("[VoiceInput] start recording error:", err);
      setError(err?.message || String(err));
      setState("error");
      setTimeout(() => setState("idle"), 3000);
    }
  }, []);

  const stopRecording = useCallback(async () => {
    if (timerRef.current) {
      clearInterval(timerRef.current);
      timerRef.current = null;
    }

    try {
      setState("transcribing");
      const result = await stopVoiceRecording();

      if (result.text.trim()) {
        onTranscription(result.text.trim());
      } else {
        setError("No speech detected");
        setState("error");
        setTimeout(() => setState("idle"), 2000);
        return;
      }
      setState("idle");
    } catch (err: any) {
      console.error("[VoiceInput] stop/transcribe error:", err);
      setError(err?.message || String(err));
      setState("error");
      setTimeout(() => setState("idle"), 3000);
    }
  }, [onTranscription]);

  const handleClick = useCallback(() => {
    if (state === "recording") {
      stopRecording();
    } else if (state === "idle" || state === "error") {
      startRecording();
    }
    // Do nothing while transcribing
  }, [state, startRecording, stopRecording]);

  const formatTime = (secs: number) => {
    const m = Math.floor(secs / 60);
    const s = secs % 60;
    return `${m}:${s.toString().padStart(2, "0")}`;
  };

  // ── Render ──

  const buttonTitle =
    state === "recording"
      ? "Stop recording"
      : state === "transcribing"
      ? "Transcribing..."
      : error
      ? error
      : "Voice input (Whisper)";

  return (
    <div className="voice-input-wrap" style={{ display: "inline-flex", alignItems: "center", gap: 4 }}>
      {state === "recording" && (
        <span className="voice-elapsed" style={{
          fontSize: 11,
          color: "var(--error, #e55)",
          fontVariantNumeric: "tabular-nums",
          animation: "voice-pulse 1s ease-in-out infinite",
        }}>
          {formatTime(elapsed)}
        </span>
      )}
      <button
        className={`btn ghost voice-input-btn ${state === "recording" ? "recording" : ""}`}
        onClick={handleClick}
        disabled={disabled || state === "transcribing"}
        title={buttonTitle}
        style={{
          padding: "4px 8px",
          color: state === "recording"
            ? "var(--error, #e55)"
            : state === "transcribing"
            ? "var(--text-softer, #888)"
            : "var(--text-soft)",
          position: "relative",
        }}
      >
        {state === "transcribing" ? (
          <Icon name="loader" className="spin" />
        ) : state === "recording" ? (
          <span style={{
            display: "inline-block",
            width: 12,
            height: 12,
            borderRadius: 3,
            backgroundColor: "var(--error, #e55)",
          }} />
        ) : (
          <Icon name="mic" />
        )}
      </button>
    </div>
  );
}
