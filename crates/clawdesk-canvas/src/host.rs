//! Canvas host — HTTP server for agent canvas content.
//!
//! Serves:
//! - `/`                              — health check
//! - `/__clawdesk__/a2ui/`           — A2UI SPA runtime
//! - `/__clawdesk__/cap/{token}/...` — capability-scoped file serving
//! - `/__clawdesk__/ws`              — WebSocket for live A2UI updates

use crate::capability::CapabilityStore;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State,
    },
    http::{header, StatusCode},
    response::{Html, IntoResponse, Json, Response},
    routing::get,
    Router,
};
use std::net::SocketAddr;
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

// ═══════════════════════════════════════════════════════════════
// Server state
// ═══════════════════════════════════════════════════════════════

/// Server state shared across all routes.
#[derive(Clone)]
pub struct ServerState {
    pub capabilities: CapabilityStore,
    pub a2ui_tx: broadcast::Sender<String>,
}

/// Canvas host server.
pub struct CanvasHostServer {
    state: ServerState,
    bind_addr: SocketAddr,
}

impl CanvasHostServer {
    /// Create a new canvas host server.
    pub fn new(capabilities: CapabilityStore, bind_addr: SocketAddr) -> Self {
        let (a2ui_tx, _) = broadcast::channel(256);
        Self {
            state: ServerState {
                capabilities,
                a2ui_tx,
            },
            bind_addr,
        }
    }

    /// Get the broadcast sender for pushing A2UI state updates.
    pub fn a2ui_sender(&self) -> broadcast::Sender<String> {
        self.state.a2ui_tx.clone()
    }

    /// Build the Axum router.
    fn build_router(&self) -> Router {
        Router::new()
            .route("/", get(health))
            .route("/__clawdesk__/a2ui/", get(a2ui_spa))
            .route("/__clawdesk__/ws", get(ws_handler))
            .route(
                "/__clawdesk__/cap/{token}/*rest",
                get(cap_serve),
            )
            .with_state(self.state.clone())
    }

    /// Start the server (runs forever).
    pub async fn start(self) -> Result<(), String> {
        let router = self.build_router();
        let listener = tokio::net::TcpListener::bind(self.bind_addr)
            .await
            .map_err(|e| format!("bind failed: {e}"))?;

        info!(addr = %self.bind_addr, "canvas host server listening");

        axum::serve(listener, router)
            .await
            .map_err(|e| format!("serve failed: {e}"))
    }

    /// Start the server in the background, returning the actual bound address.
    pub async fn start_background(self) -> Result<(SocketAddr, broadcast::Sender<String>), String> {
        let router = self.build_router();
        let listener = tokio::net::TcpListener::bind(self.bind_addr)
            .await
            .map_err(|e| format!("bind failed: {e}"))?;

        let local_addr = listener
            .local_addr()
            .map_err(|e| format!("local_addr: {e}"))?;

        let tx = self.state.a2ui_tx.clone();

        info!(addr = %local_addr, "canvas host server listening (background)");

        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, router).await {
                warn!(error = %e, "canvas host server stopped");
            }
        });

        Ok((local_addr, tx))
    }
}

// ═══════════════════════════════════════════════════════════════
// Route handlers
// ═══════════════════════════════════════════════════════════════

/// Health check.
async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "service": "clawdesk-canvas-host",
    }))
}

/// A2UI SPA runtime — serves the HTML/JS renderer.
async fn a2ui_spa() -> Html<&'static str> {
    Html(A2UI_SPA_HTML)
}

/// WebSocket handler for live A2UI updates.
async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<ServerState>,
) -> Response {
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

async fn handle_ws(mut socket: WebSocket, state: ServerState) {
    let mut rx = state.a2ui_tx.subscribe();
    debug!("A2UI WebSocket connected");

    loop {
        tokio::select! {
            // Forward broadcast A2UI state updates to the WebSocket client
            result = rx.recv() => {
                match result {
                    Ok(msg) => {
                        if socket.send(Message::Text(msg.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!(skipped = n, "A2UI WebSocket lagged");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            // Handle incoming messages from the client
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        debug!(text = text.as_str(), "A2UI WebSocket received");
                        // Client messages can be used for event callbacks
                        // (button clicks, input changes, etc.)
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }
        }
    }

    debug!("A2UI WebSocket disconnected");
}

/// Capability-scoped file serving.
async fn cap_serve(
    State(state): State<ServerState>,
    Path((token, rest)): Path<(String, String)>,
) -> Response {
    // Validate capability token
    match state.capabilities.validate(&token) {
        Some(_agent_id) => {
            debug!(token = &token[..8], path = %rest, "capability validated");
        }
        None => {
            return (StatusCode::FORBIDDEN, "invalid or expired capability token")
                .into_response();
        }
    }

    // Serve from the canvas content directory
    let base_dir = dirs::home_dir()
        .map(|h| h.join(".clawdesk").join("canvas").join("files"))
        .unwrap_or_default();

    let file_path = base_dir.join(&rest);

    // Security: ensure path doesn't escape the base directory
    match file_path.canonicalize() {
        Ok(canonical) => {
            if !canonical.starts_with(&base_dir) {
                return (StatusCode::FORBIDDEN, "path traversal denied").into_response();
            }
            match tokio::fs::read(&canonical).await {
                Ok(bytes) => {
                    let content_type = mime_from_ext(
                        canonical.extension().and_then(|e| e.to_str()).unwrap_or(""),
                    );
                    ([(header::CONTENT_TYPE, content_type)], bytes).into_response()
                }
                Err(_) => (StatusCode::NOT_FOUND, "file not found").into_response(),
            }
        }
        Err(_) => (StatusCode::NOT_FOUND, "file not found").into_response(),
    }
}

/// Map file extension to MIME type.
fn mime_from_ext(ext: &str) -> &'static str {
    match ext.to_lowercase().as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "pdf" => "application/pdf",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

// ═══════════════════════════════════════════════════════════════
// A2UI SPA runtime (inline HTML)
// ═══════════════════════════════════════════════════════════════

/// Inline A2UI Single-Page Application renderer.
///
/// This HTML/JS runtime connects to the canvas host WebSocket and renders
/// A2UI component trees in real time. It supports all 16 A2UI component types.
const A2UI_SPA_HTML: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>ClawDesk A2UI</title>
<style>
  :root {
    --bg: #0e0e10; --fg: #e4e4e7; --accent: #6366f1;
    --card-bg: #18181b; --border: #27272a; --muted: #71717a;
    --success: #22c55e; --warning: #eab308; --error: #ef4444;
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
    font-size: 14px; color: var(--fg); background: var(--bg);
  }
  * { margin: 0; padding: 0; box-sizing: border-box; }
  body { min-height: 100vh; }
  #root { padding: 16px; }
  .a2ui-column { display: flex; flex-direction: column; gap: 8px; }
  .a2ui-row { display: flex; flex-direction: row; gap: 8px; align-items: center; flex-wrap: wrap; }
  .a2ui-text { line-height: 1.5; }
  .a2ui-markdown { line-height: 1.6; }
  .a2ui-markdown h1 { font-size: 1.8em; margin: 0.5em 0; }
  .a2ui-markdown h2 { font-size: 1.4em; margin: 0.4em 0; }
  .a2ui-markdown h3 { font-size: 1.2em; margin: 0.3em 0; }
  .a2ui-markdown p { margin: 0.4em 0; }
  .a2ui-markdown ul, .a2ui-markdown ol { padding-left: 1.5em; margin: 0.4em 0; }
  .a2ui-markdown code { background: var(--card-bg); padding: 2px 6px; border-radius: 3px; font-size: 0.9em; }
  .a2ui-markdown pre { background: var(--card-bg); padding: 12px; border-radius: 6px; overflow-x: auto; }
  .a2ui-markdown pre code { padding: 0; background: none; }
  .a2ui-code { background: var(--card-bg); padding: 12px; border-radius: 6px;
    font-family: "SF Mono", "Fira Code", monospace; font-size: 13px;
    overflow-x: auto; white-space: pre; border: 1px solid var(--border); }
  .a2ui-image { max-width: 100%; height: auto; border-radius: 6px; }
  .a2ui-button { background: var(--accent); color: white; border: none; padding: 8px 16px;
    border-radius: 6px; cursor: pointer; font-size: 14px; transition: opacity 0.15s; }
  .a2ui-button:hover { opacity: 0.85; }
  .a2ui-button.secondary { background: var(--card-bg); border: 1px solid var(--border); color: var(--fg); }
  .a2ui-button.danger { background: var(--error); }
  .a2ui-input { background: var(--card-bg); border: 1px solid var(--border); color: var(--fg);
    padding: 8px 12px; border-radius: 6px; font-size: 14px; width: 100%; }
  .a2ui-input:focus { outline: none; border-color: var(--accent); }
  .a2ui-select { background: var(--card-bg); border: 1px solid var(--border); color: var(--fg);
    padding: 8px 12px; border-radius: 6px; font-size: 14px; }
  .a2ui-table { width: 100%; border-collapse: collapse; }
  .a2ui-table th, .a2ui-table td { padding: 8px 12px; text-align: left;
    border-bottom: 1px solid var(--border); }
  .a2ui-table th { font-weight: 600; color: var(--muted); font-size: 12px; text-transform: uppercase; }
  .a2ui-progress { width: 100%; height: 8px; background: var(--card-bg);
    border-radius: 4px; overflow: hidden; }
  .a2ui-progress-bar { height: 100%; background: var(--accent); transition: width 0.3s ease; }
  .a2ui-divider { border: none; border-top: 1px solid var(--border); margin: 8px 0; }
  .a2ui-spacer { flex-shrink: 0; }
  .a2ui-card { background: var(--card-bg); border: 1px solid var(--border);
    border-radius: 8px; padding: 16px; }
  .a2ui-chart { width: 100%; min-height: 200px; background: var(--card-bg);
    border: 1px solid var(--border); border-radius: 6px; display: flex;
    align-items: center; justify-content: center; color: var(--muted); }
  #status { position: fixed; top: 8px; right: 8px; font-size: 11px;
    padding: 4px 8px; border-radius: 4px; background: var(--card-bg);
    border: 1px solid var(--border); color: var(--muted); z-index: 9999; }
  #status.connected { border-color: var(--success); color: var(--success); }
  #status.disconnected { border-color: var(--error); color: var(--error); }
</style>
</head>
<body>
<div id="status" class="disconnected">disconnected</div>
<div id="root"></div>
<script>
(function() {
  const root = document.getElementById('root');
  const status = document.getElementById('status');
  let ws = null;
  let reconnectTimer = null;
  let surfaces = {};

  function connect() {
    const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
    ws = new WebSocket(`${proto}//${location.host}/__clawdesk__/ws`);
    ws.onopen = () => {
      status.textContent = 'connected';
      status.className = 'connected';
    };
    ws.onclose = () => {
      status.textContent = 'disconnected';
      status.className = 'disconnected';
      reconnectTimer = setTimeout(connect, 2000);
    };
    ws.onerror = () => ws.close();
    ws.onmessage = (e) => {
      try {
        surfaces = JSON.parse(e.data);
        render();
      } catch(err) {
        console.error('A2UI parse error:', err);
      }
    };
  }

  function render() {
    root.innerHTML = '';
    for (const [sid, surface] of Object.entries(surfaces)) {
      const el = renderComponent(surface.root || surface);
      if (el) root.appendChild(el);
    }
  }

  function resolveText(tv, data) {
    if (!tv) return '';
    if (typeof tv === 'string') return tv;
    if (tv.type === 'binding' && data) {
      return data[tv.path] ?? `{{${tv.path}}}`;
    }
    return tv.value || '';
  }

  function renderComponent(comp, data) {
    if (!comp) return null;
    const type = comp.type || comp.Type || Object.keys(comp)[0];
    const props = comp[type] || comp.props || comp;

    switch(type?.toLowerCase()) {
      case 'column': {
        const div = document.createElement('div');
        div.className = 'a2ui-column';
        (props.children || []).forEach(c => {
          const el = renderComponent(c, data);
          if (el) div.appendChild(el);
        });
        return div;
      }
      case 'row': {
        const div = document.createElement('div');
        div.className = 'a2ui-row';
        (props.children || []).forEach(c => {
          const el = renderComponent(c, data);
          if (el) div.appendChild(el);
        });
        return div;
      }
      case 'text': {
        const span = document.createElement('span');
        span.className = 'a2ui-text';
        span.textContent = resolveText(props.content || props.text, data);
        if (props.bold) span.style.fontWeight = '600';
        if (props.italic) span.style.fontStyle = 'italic';
        if (props.color) span.style.color = props.color;
        if (props.size) span.style.fontSize = props.size;
        return span;
      }
      case 'markdown': {
        const div = document.createElement('div');
        div.className = 'a2ui-markdown';
        div.innerHTML = simpleMarkdown(resolveText(props.content || props.text, data));
        return div;
      }
      case 'code': {
        const pre = document.createElement('pre');
        pre.className = 'a2ui-code';
        pre.textContent = resolveText(props.content || props.code, data);
        if (props.language) pre.dataset.language = props.language;
        return pre;
      }
      case 'image': {
        const img = document.createElement('img');
        img.className = 'a2ui-image';
        img.src = props.src || props.url || '';
        img.alt = props.alt || '';
        if (props.width) img.style.width = props.width;
        if (props.height) img.style.height = props.height;
        return img;
      }
      case 'button': {
        const btn = document.createElement('button');
        btn.className = 'a2ui-button' + (props.variant ? ' ' + props.variant : '');
        btn.textContent = resolveText(props.label || props.text, data);
        if (props.disabled) btn.disabled = true;
        btn.onclick = () => sendEvent('click', { id: props.id, action: props.action });
        return btn;
      }
      case 'input': {
        const input = document.createElement('input');
        input.className = 'a2ui-input';
        input.type = props.input_type || 'text';
        input.placeholder = props.placeholder || '';
        input.value = props.value || '';
        if (props.id) input.dataset.id = props.id;
        input.onchange = () => sendEvent('change', { id: props.id, value: input.value });
        return input;
      }
      case 'select': {
        const sel = document.createElement('select');
        sel.className = 'a2ui-select';
        (props.options || []).forEach(opt => {
          const o = document.createElement('option');
          o.value = typeof opt === 'string' ? opt : opt.value;
          o.textContent = typeof opt === 'string' ? opt : opt.label;
          sel.appendChild(o);
        });
        if (props.value) sel.value = props.value;
        sel.onchange = () => sendEvent('change', { id: props.id, value: sel.value });
        return sel;
      }
      case 'table': {
        const table = document.createElement('table');
        table.className = 'a2ui-table';
        if (props.headers) {
          const thead = document.createElement('thead');
          const tr = document.createElement('tr');
          props.headers.forEach(h => {
            const th = document.createElement('th');
            th.textContent = h;
            tr.appendChild(th);
          });
          thead.appendChild(tr);
          table.appendChild(thead);
        }
        const tbody = document.createElement('tbody');
        (props.rows || []).forEach(row => {
          const tr = document.createElement('tr');
          (Array.isArray(row) ? row : Object.values(row)).forEach(cell => {
            const td = document.createElement('td');
            td.textContent = cell;
            tr.appendChild(td);
          });
          tbody.appendChild(tr);
        });
        table.appendChild(tbody);
        return table;
      }
      case 'chart': {
        const div = document.createElement('div');
        div.className = 'a2ui-chart';
        div.textContent = `[Chart: ${props.chart_type || 'unknown'} — ${(props.data || []).length} points]`;
        return div;
      }
      case 'progress': {
        const outer = document.createElement('div');
        outer.className = 'a2ui-progress';
        const bar = document.createElement('div');
        bar.className = 'a2ui-progress-bar';
        bar.style.width = `${Math.min(100, Math.max(0, props.value || 0))}%`;
        outer.appendChild(bar);
        return outer;
      }
      case 'divider': {
        return document.createElement('hr');
      }
      case 'spacer': {
        const div = document.createElement('div');
        div.className = 'a2ui-spacer';
        div.style.height = props.height || '16px';
        return div;
      }
      case 'card': {
        const div = document.createElement('div');
        div.className = 'a2ui-card';
        if (props.title) {
          const h = document.createElement('h3');
          h.textContent = props.title;
          h.style.marginBottom = '8px';
          div.appendChild(h);
        }
        (props.children || []).forEach(c => {
          const el = renderComponent(c, data);
          if (el) div.appendChild(el);
        });
        return div;
      }
      case 'html': {
        const div = document.createElement('div');
        div.innerHTML = props.content || '';
        return div;
      }
      default: {
        const div = document.createElement('div');
        div.textContent = `[Unknown: ${type}]`;
        div.style.color = 'var(--muted)';
        return div;
      }
    }
  }

  function sendEvent(event, payload) {
    if (ws && ws.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify({ event, ...payload }));
    }
  }

  function simpleMarkdown(text) {
    if (!text) return '';
    return text
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/^### (.+)$/gm, '<h3>$1</h3>')
      .replace(/^## (.+)$/gm, '<h2>$1</h2>')
      .replace(/^# (.+)$/gm, '<h1>$1</h1>')
      .replace(/\*\*(.+?)\*\*/g, '<strong>$1</strong>')
      .replace(/\*(.+?)\*/g, '<em>$1</em>')
      .replace(/`([^`]+)`/g, '<code>$1</code>')
      .replace(/```(\w*)\n([\s\S]*?)```/g, '<pre><code>$2</code></pre>')
      .replace(/^- (.+)$/gm, '<li>$1</li>')
      .replace(/(<li>.*<\/li>)/s, '<ul>$1</ul>')
      .replace(/\n\n/g, '</p><p>')
      .replace(/^(?!<[hulo])/gm, '<p>')
      .replace(/<p><\/p>/g, '');
  }

  // Listen for postMessage from parent window (Tauri)
  window.addEventListener('message', (e) => {
    if (e.data && e.data.type === 'a2ui-update') {
      surfaces = e.data.surfaces || {};
      render();
    }
  });

  connect();
})();
</script>
</body>
</html>
"##;

// ═══════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_from_ext_works() {
        assert_eq!(mime_from_ext("html"), "text/html; charset=utf-8");
        assert_eq!(mime_from_ext("png"), "image/png");
        assert_eq!(mime_from_ext("js"), "application/javascript; charset=utf-8");
        assert_eq!(mime_from_ext("xyz"), "application/octet-stream");
    }

    #[tokio::test]
    async fn server_creates_and_binds() {
        let caps = CapabilityStore::new("http://localhost:0".into());
        let server =
            CanvasHostServer::new(caps, "127.0.0.1:0".parse().unwrap());
        let (addr, _tx) = server.start_background().await.unwrap();
        assert_ne!(addr.port(), 0);
    }
}
