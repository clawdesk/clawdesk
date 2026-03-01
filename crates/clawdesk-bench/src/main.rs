//! ClawDesk Memory Benchmark Bridge
//!
//! JSON-line protocol over stdin/stdout for Python FFI benchmarking.
//! Wraps MemoryManager<SochMemoryBackend> with full hybrid search,
//! graph overlay, temporal decay, and MMR reranking.
//!
//! ## Protocol
//! Each line on stdin is a JSON command, each response is a JSON line on stdout.
//!
//! Commands:
//!   init       — Initialize SochDB + MemoryManager
//!   remember   — Store a single memory
//!   remember_batch — Store multiple memories
//!   recall     — Query memories with full hybrid pipeline
//!   forget     — Delete a memory by ID
//!   stats      — Get memory statistics
//!   reset      — Wipe database and reinitialize
//!   shutdown   — Graceful shutdown

use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;

use clawdesk_memory::{
    EmbeddingProvider, MemoryConfig, MemoryManager, MemorySource, OllamaEmbeddingProvider,
    SearchStrategy,
};
use clawdesk_sochdb::{SochMemoryBackend, SochStore};
use serde::{Deserialize, Serialize};
use tokio::runtime::Runtime;
use tracing::Level;

// ─── Protocol types ─────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
enum Command {
    Init {
        db_path: Option<String>,
        ollama_url: Option<String>,
        model: Option<String>,
        collection: Option<String>,
        max_results: Option<usize>,
        min_relevance: Option<f32>,
    },
    Remember {
        content: String,
        source: Option<String>,
        metadata: Option<serde_json::Value>,
    },
    RememberBatch {
        items: Vec<BatchItem>,
    },
    Recall {
        query: String,
        max_results: Option<usize>,
    },
    Forget {
        id: String,
    },
    Stats,
    Reset,
    Shutdown,
}

#[derive(Debug, Deserialize)]
struct BatchItem {
    content: String,
    source: Option<String>,
    metadata: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct Response {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ids: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    results: Option<Vec<RecallResult>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    deleted: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    total_memories: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    latency_ms: Option<u64>,
}

#[derive(Debug, Serialize)]
struct RecallResult {
    id: String,
    score: f32,
    content: Option<String>,
    metadata: serde_json::Value,
}

impl Response {
    fn ok() -> Self {
        Self {
            ok: true,
            id: None,
            ids: None,
            results: None,
            deleted: None,
            total_memories: None,
            error: None,
            message: None,
            latency_ms: None,
        }
    }

    fn err(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(msg.into()),
            ..Self::ok()
        }
    }
}

// ─── State ──────────────────────────────────────────────────────────────────

struct BenchState {
    #[allow(dead_code)]
    store: Arc<SochStore>,
    memory: Arc<MemoryManager<SochMemoryBackend>>,
    db_path: PathBuf,
    ollama_url: String,
    model: String,
    collection: String,
    max_results: usize,
    min_relevance: f32,
}

fn parse_source(s: &Option<String>) -> MemorySource {
    match s.as_deref() {
        Some("conversation") => MemorySource::Conversation,
        Some("document") => MemorySource::Document,
        Some("user_saved") => MemorySource::UserSaved,
        Some("plugin") => MemorySource::Plugin,
        Some("system") => MemorySource::System,
        _ => MemorySource::Document,
    }
}

fn init_memory(
    db_path: &PathBuf,
    ollama_url: &str,
    model: &str,
    collection: &str,
    max_results: usize,
    min_relevance: f32,
) -> Result<(Arc<SochStore>, Arc<MemoryManager<SochMemoryBackend>>), String> {
    std::fs::create_dir_all(db_path).map_err(|e| format!("mkdir: {e}"))?;

    let store = Arc::new(SochStore::open(db_path).map_err(|e| format!("SochStore::open: {e}"))?);
    let backend = Arc::new(SochMemoryBackend::new(store.clone()));

    let embedding: Arc<dyn EmbeddingProvider> = Arc::new(OllamaEmbeddingProvider::new(
        Some(model.to_string()),
        Some(ollama_url.to_string()),
    ));

    let config = MemoryConfig {
        collection_name: collection.to_string(),
        search_strategy: SearchStrategy::Hybrid,
        auto_embed: true,
        max_results,
        min_relevance,
        ..MemoryConfig::default()
    };

    let memory = Arc::new(MemoryManager::new(backend, embedding, config));
    Ok((store, memory))
}

// ─── Main loop ──────────────────────────────────────────────────────────────

fn handle_command(
    cmd: Command,
    state: &mut Option<BenchState>,
    rt: &Runtime,
) -> Option<Response> {
    let resp = match cmd {
        Command::Init {
            db_path,
            ollama_url,
            model,
            collection,
            max_results,
            min_relevance,
        } => {
            // Drop existing state first to release SochDB lock
            if state.is_some() {
                let _ = state.take();
            }

            let path = PathBuf::from(
                db_path.unwrap_or_else(|| "/tmp/clawdesk_bench_sochdb".to_string()),
            );
            let url = ollama_url.unwrap_or_else(|| "http://localhost:11434".to_string());
            let mdl = model.unwrap_or_else(|| "nomic-embed-text".to_string());
            let col = collection.unwrap_or_else(|| "bench_memories".to_string());
            let mr = max_results.unwrap_or(10);
            let mrel = min_relevance.unwrap_or(0.1);

            match init_memory(&path, &url, &mdl, &col, mr, mrel) {
                Ok((store, memory)) => {
                    *state = Some(BenchState {
                        store,
                        memory,
                        db_path: path,
                        ollama_url: url,
                        model: mdl,
                        collection: col,
                        max_results: mr,
                        min_relevance: mrel,
                    });
                    let mut r = Response::ok();
                    r.message = Some("initialized".to_string());
                    r
                }
                Err(e) => Response::err(e),
            }
        }

        Command::Remember {
            content,
            source,
            metadata,
        } => {
            let st = match state.as_ref() {
                Some(s) => s,
                None => return Some(Response::err("not initialized — send init first")),
            };
            let src = parse_source(&source);
            let meta = metadata.unwrap_or(serde_json::json!({}));
            let t0 = std::time::Instant::now();
            match rt.block_on(st.memory.remember(&content, src, meta)) {
                Ok(id) => {
                    let mut r = Response::ok();
                    r.id = Some(id);
                    r.latency_ms = Some(t0.elapsed().as_millis() as u64);
                    r
                }
                Err(e) => Response::err(e),
            }
        }

        Command::RememberBatch { items } => {
            let st = match state.as_ref() {
                Some(s) => s,
                None => return Some(Response::err("not initialized — send init first")),
            };
            let batch: Vec<(String, MemorySource, serde_json::Value)> = items
                .into_iter()
                .map(|i| {
                    (
                        i.content,
                        parse_source(&i.source),
                        i.metadata.unwrap_or(serde_json::json!({})),
                    )
                })
                .collect();
            let t0 = std::time::Instant::now();
            match rt.block_on(st.memory.remember_batch(batch)) {
                Ok(ids) => {
                    let mut r = Response::ok();
                    r.ids = Some(ids);
                    r.latency_ms = Some(t0.elapsed().as_millis() as u64);
                    r
                }
                Err(e) => Response::err(e),
            }
        }

        Command::Recall { query, max_results } => {
            let st = match state.as_ref() {
                Some(s) => s,
                None => return Some(Response::err("not initialized — send init first")),
            };
            let t0 = std::time::Instant::now();
            match rt.block_on(st.memory.recall(&query, max_results)) {
                Ok(results) => {
                    let mut r = Response::ok();
                    r.results = Some(
                        results
                            .into_iter()
                            .map(|vr| RecallResult {
                                id: vr.id,
                                score: vr.score,
                                content: vr.content,
                                metadata: vr.metadata,
                            })
                            .collect(),
                    );
                    r.latency_ms = Some(t0.elapsed().as_millis() as u64);
                    r
                }
                Err(e) => Response::err(e),
            }
        }

        Command::Forget { id } => {
            let st = match state.as_ref() {
                Some(s) => s,
                None => return Some(Response::err("not initialized — send init first")),
            };
            match rt.block_on(st.memory.forget(&id)) {
                Ok(deleted) => {
                    let mut r = Response::ok();
                    r.deleted = Some(deleted);
                    r
                }
                Err(e) => Response::err(e),
            }
        }

        Command::Stats => {
            let st = match state.as_ref() {
                Some(s) => s,
                None => return Some(Response::err("not initialized — send init first")),
            };
            // No stats() method on MemoryManager — do a wildcard recall to estimate count
            match rt.block_on(st.memory.recall("*", Some(1000))) {
                Ok(results) => {
                    let mut r = Response::ok();
                    r.total_memories = Some(results.len());
                    r
                }
                Err(_) => {
                    let mut r = Response::ok();
                    r.total_memories = Some(0);
                    r
                }
            }
        }

        Command::Reset => {
            // Drop existing state, remove db, reinit
            if let Some(st) = state.take() {
                let path = st.db_path.clone();
                let url = st.ollama_url.clone();
                let mdl = st.model.clone();
                let col = st.collection.clone();
                let mr = st.max_results;
                let mrel = st.min_relevance;
                drop(st);

                // Remove old DB
                let _ = std::fs::remove_dir_all(&path);

                match init_memory(&path, &url, &mdl, &col, mr, mrel) {
                    Ok((store, memory)) => {
                        *state = Some(BenchState {
                            store,
                            memory,
                            db_path: path,
                            ollama_url: url,
                            model: mdl,
                            collection: col,
                            max_results: mr,
                            min_relevance: mrel,
                        });
                        let mut r = Response::ok();
                        r.message = Some("reset complete".to_string());
                        r
                    }
                    Err(e) => Response::err(e),
                }
            } else {
                Response::err("not initialized — nothing to reset")
            }
        }

        Command::Shutdown => {
            return None; // Signal to break
        }
    };
    Some(resp)
}

fn main() {
    tracing_subscriber::fmt()
        .with_max_level(Level::WARN)
        .with_writer(io::stderr)
        .init();

    let rt = Runtime::new().expect("tokio runtime");
    let mut state: Option<BenchState> = None;

    let stdin = io::stdin();
    let mut stdout = io::stdout();

    // Signal ready
    let ready = serde_json::json!({"ready": true, "version": "0.1.0"});
    writeln!(stdout, "{}", ready).ok();
    stdout.flush().ok();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };

        if line.trim().is_empty() {
            continue;
        }

        let cmd: Command = match serde_json::from_str(&line) {
            Ok(c) => c,
            Err(e) => {
                let resp = Response::err(format!("parse error: {e}"));
                writeln!(stdout, "{}", serde_json::to_string(&resp).unwrap()).ok();
                stdout.flush().ok();
                continue;
            }
        };

        match handle_command(cmd, &mut state, &rt) {
            Some(resp) => {
                writeln!(stdout, "{}", serde_json::to_string(&resp).unwrap()).ok();
                stdout.flush().ok();
            }
            None => {
                // Shutdown
                let r = Response::ok();
                writeln!(stdout, "{}", serde_json::to_string(&r).unwrap()).ok();
                stdout.flush().ok();
                break;
            }
        }
    }
}
