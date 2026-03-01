"""
ClawDesk Memory FFI Bridge

Manages a subprocess running the clawdesk-bench Rust binary,
communicating via JSON-line protocol over stdin/stdout.

Usage:
    bridge = ClawDeskBridge()
    bridge.init(ollama_url="http://localhost:11434")
    mid = bridge.remember("The capital of France is Paris")
    results = bridge.recall("What is the capital of France?")
    bridge.shutdown()
"""

import json
import os
import subprocess
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Optional


@dataclass
class RecallResult:
    """A single memory recall result."""
    id: str
    score: float
    content: Optional[str]
    metadata: dict


@dataclass
class BridgeConfig:
    """Configuration for the FFI bridge."""
    bench_binary: str = ""
    db_path: str = "/tmp/clawdesk_bench_sochdb"
    ollama_url: str = "http://localhost:11434"
    model: str = "nomic-embed-text"
    collection: str = "bench_memories"
    max_results: int = 10
    min_relevance: float = 0.1

    def __post_init__(self):
        if not self.bench_binary:
            # Auto-detect: look for debug build relative to this file
            bench_dir = Path(__file__).parent.parent
            candidates = [
                bench_dir / "target" / "debug" / "clawdesk-bench",
                bench_dir / "target" / "release" / "clawdesk-bench",
            ]
            for c in candidates:
                if c.exists():
                    self.bench_binary = str(c)
                    break
            if not self.bench_binary:
                raise FileNotFoundError(
                    f"clawdesk-bench binary not found. Build with: "
                    f"cargo build -p clawdesk-bench\n"
                    f"Searched: {[str(c) for c in candidates]}"
                )


class ClawDeskBridge:
    """Python FFI bridge to ClawDesk's Rust memory system."""

    def __init__(self, config: Optional[BridgeConfig] = None):
        self.config = config or BridgeConfig()
        self.process: Optional[subprocess.Popen] = None
        self._started = False
        self._latencies: list[float] = []

    def start(self):
        """Start the Rust bridge subprocess."""
        if self._started:
            return

        self.process = subprocess.Popen(
            [self.config.bench_binary],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,  # line-buffered
        )

        # Wait for ready signal
        ready_line = self.process.stdout.readline()
        ready = json.loads(ready_line)
        if not ready.get("ready"):
            raise RuntimeError(f"Bridge failed to start: {ready_line}")
        self._started = True
        print(f"[bridge] Started clawdesk-bench v{ready.get('version', '?')}", file=sys.stderr)

    def _send(self, cmd: dict) -> dict:
        """Send a command and receive response."""
        if not self._started:
            self.start()

        line = json.dumps(cmd, separators=(",", ":"))
        self.process.stdin.write(line + "\n")
        self.process.stdin.flush()

        resp_line = self.process.stdout.readline()
        if not resp_line:
            raise RuntimeError("Bridge process died — no response")

        resp = json.loads(resp_line)
        if not resp.get("ok", False) and "error" in resp:
            raise RuntimeError(f"Bridge error: {resp['error']}")

        if "latency_ms" in resp:
            self._latencies.append(resp["latency_ms"])

        return resp

    def init(
        self,
        db_path: Optional[str] = None,
        ollama_url: Optional[str] = None,
        model: Optional[str] = None,
        collection: Optional[str] = None,
        max_results: Optional[int] = None,
        min_relevance: Optional[float] = None,
    ) -> dict:
        """Initialize SochDB + MemoryManager."""
        cmd = {"cmd": "init"}
        if db_path:
            cmd["db_path"] = db_path
        else:
            cmd["db_path"] = self.config.db_path
        if ollama_url:
            cmd["ollama_url"] = ollama_url
        else:
            cmd["ollama_url"] = self.config.ollama_url
        if model:
            cmd["model"] = model
        else:
            cmd["model"] = self.config.model
        if collection:
            cmd["collection"] = collection
        else:
            cmd["collection"] = self.config.collection
        if max_results is not None:
            cmd["max_results"] = max_results
        else:
            cmd["max_results"] = self.config.max_results
        if min_relevance is not None:
            cmd["min_relevance"] = min_relevance
        else:
            cmd["min_relevance"] = self.config.min_relevance
        return self._send(cmd)

    def remember(
        self,
        content: str,
        source: str = "document",
        metadata: Optional[dict] = None,
    ) -> str:
        """Store a memory. Returns memory ID."""
        cmd = {
            "cmd": "remember",
            "content": content,
            "source": source,
            "metadata": metadata or {},
        }
        resp = self._send(cmd)
        return resp["id"]

    def remember_batch(
        self,
        items: list[tuple[str, str, dict]],
    ) -> list[str]:
        """Store multiple memories. Returns list of memory IDs.

        items: list of (content, source, metadata) tuples
        """
        cmd = {
            "cmd": "remember_batch",
            "items": [
                {"content": c, "source": s, "metadata": m}
                for c, s, m in items
            ],
        }
        resp = self._send(cmd)
        return resp["ids"]

    def recall(
        self,
        query: str,
        max_results: Optional[int] = None,
    ) -> list[RecallResult]:
        """Recall relevant memories for a query."""
        cmd = {"cmd": "recall", "query": query}
        if max_results is not None:
            cmd["max_results"] = max_results
        resp = self._send(cmd)
        return [
            RecallResult(
                id=r["id"],
                score=r["score"],
                content=r.get("content"),
                metadata=r.get("metadata", {}),
            )
            for r in resp.get("results", [])
        ]

    def forget(self, memory_id: str) -> bool:
        """Delete a memory by ID."""
        resp = self._send({"cmd": "forget", "id": memory_id})
        return resp.get("deleted", False)

    def stats(self) -> dict:
        """Get memory statistics."""
        return self._send({"cmd": "stats"})

    def reset(self) -> dict:
        """Wipe database and reinitialize."""
        return self._send({"cmd": "reset"})

    def shutdown(self):
        """Graceful shutdown."""
        if self._started and self.process:
            try:
                self._send({"cmd": "shutdown"})
            except Exception:
                pass
            self.process.terminate()
            self.process.wait(timeout=5)
            self._started = False

    @property
    def avg_latency_ms(self) -> float:
        """Average operation latency in milliseconds."""
        if not self._latencies:
            return 0.0
        return sum(self._latencies) / len(self._latencies)

    @property
    def p99_latency_ms(self) -> float:
        """P99 operation latency in milliseconds."""
        if not self._latencies:
            return 0.0
        sorted_l = sorted(self._latencies)
        idx = int(len(sorted_l) * 0.99)
        return sorted_l[min(idx, len(sorted_l) - 1)]

    def __enter__(self):
        self.start()
        return self

    def __exit__(self, *args):
        self.shutdown()

    def __del__(self):
        try:
            self.shutdown()
        except Exception:
            pass
