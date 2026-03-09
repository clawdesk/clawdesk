//! Unified streaming abstraction for LLM provider responses.
//!
//! `ProviderStream` wraps a `tokio::sync::mpsc::Receiver<StreamChunk>` with
//! bounded-channel backpressure and provides a typed `Stream` interface for
//! consumers.
//!
//! ## Backpressure
//!
//! The bounded channel (default capacity: 256) provides natural backpressure:
//! if the consumer is slow, the producer blocks at the channel send until a
//! slot opens. This prevents unbounded memory growth during streaming.
//!
//! ## Usage
//!
//! ```ignore
//! let (stream, tx) = ProviderStream::new(256);
//! // Pass `tx` to the provider's stream() method
//! // Consume `stream` via async iteration
//! while let Some(chunk) = stream.next().await {
//!     // process chunk
//! }
//! let stats = stream.stats();
//! ```

use crate::StreamChunk;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// Default channel buffer size — 256 chunks provides ~100ms of buffer at
/// typical streaming rates (2-3k chunks/sec) while bounding memory to ~64KB.
pub const DEFAULT_BUFFER_SIZE: usize = 256;

/// Unified streaming wrapper over bounded mpsc channel.
///
/// Implements manual async iteration via `next()`. Tracks cumulative
/// statistics (chunks, bytes, timing) for observability.
pub struct ProviderStream {
    rx: mpsc::Receiver<StreamChunk>,
    stats: StreamStats,
    started_at: Instant,
    done: bool,
}

/// Sender half — pass this to the provider's `stream()` method.
pub type StreamSender = mpsc::Sender<StreamChunk>;

/// Cumulative statistics for a completed stream.
#[derive(Debug, Clone, Default)]
pub struct StreamStats {
    /// Total chunks received (including the final done=true chunk).
    pub chunks: usize,
    /// Total content bytes across all deltas.
    pub content_bytes: usize,
    /// Total reasoning bytes across all reasoning deltas.
    pub reasoning_bytes: usize,
    /// Time from stream creation to last chunk.
    pub wall_time: Duration,
    /// Time from stream creation to first non-empty delta.
    pub time_to_first_token: Option<Duration>,
    /// Tool calls extracted from the final chunk.
    pub tool_call_count: usize,
}

impl ProviderStream {
    /// Create a new stream with the given channel buffer capacity.
    ///
    /// Returns `(stream, sender)`. Pass the sender to the provider.
    pub fn new(buffer: usize) -> (Self, StreamSender) {
        let (tx, rx) = mpsc::channel(buffer);
        let stream = Self {
            rx,
            stats: StreamStats::default(),
            started_at: Instant::now(),
            done: false,
        };
        (stream, tx)
    }

    /// Create a stream with the default buffer size (256).
    pub fn buffered() -> (Self, StreamSender) {
        Self::new(DEFAULT_BUFFER_SIZE)
    }

    /// Receive the next chunk, or `None` if the stream is complete.
    pub async fn next(&mut self) -> Option<StreamChunk> {
        if self.done {
            return None;
        }

        match self.rx.recv().await {
            Some(chunk) => {
                self.stats.chunks += 1;
                self.stats.content_bytes += chunk.delta.len();
                self.stats.reasoning_bytes += chunk.reasoning_delta.len();

                // Track time to first non-empty token
                if self.stats.time_to_first_token.is_none() && !chunk.delta.is_empty() {
                    self.stats.time_to_first_token = Some(self.started_at.elapsed());
                }

                if chunk.done {
                    self.done = true;
                    self.stats.wall_time = self.started_at.elapsed();
                    self.stats.tool_call_count = chunk.tool_calls.len();
                }

                Some(chunk)
            }
            None => {
                // Channel closed without a done=true chunk — producer dropped.
                self.done = true;
                self.stats.wall_time = self.started_at.elapsed();
                None
            }
        }
    }

    /// Check if the stream has completed (received done=true or channel closed).
    pub fn is_done(&self) -> bool {
        self.done
    }

    /// Get cumulative stream statistics.
    pub fn stats(&self) -> &StreamStats {
        &self.stats
    }

    /// Collect all remaining chunks into a single concatenated string.
    ///
    /// Useful for non-streaming consumers that want to await the full response.
    pub async fn collect_content(&mut self) -> String {
        let mut buf = String::new();
        while let Some(chunk) = self.next().await {
            buf.push_str(&chunk.delta);
        }
        buf
    }
}

/// Wrapper that implements `futures::Stream` for interop with stream combinators.
pub struct ProviderStreamPoll {
    inner: ProviderStream,
}

impl ProviderStreamPoll {
    pub fn new(stream: ProviderStream) -> Self {
        Self { inner: stream }
    }

    pub fn stats(&self) -> &StreamStats {
        self.inner.stats()
    }
}

impl futures::Stream for ProviderStreamPoll {
    type Item = StreamChunk;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.inner.done {
            return Poll::Ready(None);
        }
        match self.inner.rx.poll_recv(cx) {
            Poll::Ready(Some(chunk)) => {
                self.inner.stats.chunks += 1;
                self.inner.stats.content_bytes += chunk.delta.len();
                self.inner.stats.reasoning_bytes += chunk.reasoning_delta.len();

                if self.inner.stats.time_to_first_token.is_none() && !chunk.delta.is_empty() {
                    self.inner.stats.time_to_first_token = Some(self.inner.started_at.elapsed());
                }

                if chunk.done {
                    self.inner.done = true;
                    self.inner.stats.wall_time = self.inner.started_at.elapsed();
                    self.inner.stats.tool_call_count = chunk.tool_calls.len();
                }

                Poll::Ready(Some(chunk))
            }
            Poll::Ready(None) => {
                self.inner.done = true;
                self.inner.stats.wall_time = self.inner.started_at.elapsed();
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FinishReason, TokenUsage};

    fn make_chunk(delta: &str, done: bool) -> StreamChunk {
        StreamChunk {
            delta: delta.to_string(),
            reasoning_delta: String::new(),
            done,
            finish_reason: if done { Some(FinishReason::Stop) } else { None },
            usage: if done {
                Some(TokenUsage::new(10, 20))
            } else {
                None
            },
            tool_calls: Vec::new(),
        }
    }

    #[tokio::test]
    async fn basic_streaming() {
        let (mut stream, tx) = ProviderStream::buffered();

        tokio::spawn(async move {
            tx.send(make_chunk("Hello", false)).await.ok();
            tx.send(make_chunk(" world", false)).await.ok();
            tx.send(make_chunk("!", true)).await.ok();
        });

        let mut content = String::new();
        while let Some(chunk) = stream.next().await {
            content.push_str(&chunk.delta);
        }

        assert_eq!(content, "Hello world!");
        assert_eq!(stream.stats().chunks, 3);
        assert_eq!(stream.stats().content_bytes, 12);
        assert!(stream.stats().time_to_first_token.is_some());
    }

    #[tokio::test]
    async fn collect_content() {
        let (mut stream, tx) = ProviderStream::new(16);

        tokio::spawn(async move {
            tx.send(make_chunk("foo", false)).await.ok();
            tx.send(make_chunk("bar", true)).await.ok();
        });

        let content = stream.collect_content().await;
        assert_eq!(content, "foobar");
        assert!(stream.is_done());
    }

    #[tokio::test]
    async fn producer_drop_completes_stream() {
        let (mut stream, tx) = ProviderStream::new(4);

        tokio::spawn(async move {
            tx.send(make_chunk("partial", false)).await.ok();
            drop(tx); // Drop without done=true
        });

        let mut chunks = 0;
        while stream.next().await.is_some() {
            chunks += 1;
        }
        assert_eq!(chunks, 1);
        assert!(stream.is_done());
    }
}
