//! Fallback static file server for dev mode.
//!
//! When `cargo build -p clawdesk-tauri` compiles with `cfg(dev)`, Tauri's
//! webview loads from `http://localhost:1420` (the Vite dev server). If the
//! Vite dev server isn't running, the user sees a blank white window.
//!
//! This module detects the missing dev server and spawns a minimal HTTP
//! server on `:1420` that serves the pre-built `crates/ui/dist/` assets.
//! When Vite IS running, this module does nothing (Vite takes priority).

use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::{info, warn};

/// Check if port 1420 is already in use (Vite dev server running).
fn is_port_in_use(port: u16) -> bool {
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok()
}

/// Resolve the UI dist directory path.
fn find_dist_dir() -> Option<PathBuf> {
    // Path relative to the crate's manifest dir at compile time
    let manifest_dist = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../ui/dist");
    if manifest_dist.join("index.html").exists() {
        return Some(manifest_dist.canonicalize().unwrap_or(manifest_dist));
    }

    // Fallback: ~/.clawdesk/ui/dist (for installed deployments)
    let dot_dist = clawdesk_types::dirs::dot_clawdesk().join("ui/dist");
    if dot_dist.join("index.html").exists() {
        return Some(dot_dist);
    }

    None
}

/// Guess MIME type from file extension.
fn mime_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "application/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("ico") => "image/x-icon",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        Some("wasm") => "application/wasm",
        Some("map") => "application/json",
        _ => "application/octet-stream",
    }
}

/// Spawn a minimal static file server on port 1420 if the Vite dev server
/// isn't running and a pre-built dist/ directory exists.
///
/// Returns `true` if the fallback server was started.
pub fn maybe_start_fallback_server() -> bool {
    if is_port_in_use(1420) {
        info!("Vite dev server detected on :1420 — using live dev server");
        return false;
    }

    let dist_dir = match find_dist_dir() {
        Some(d) => d,
        None => {
            warn!(
                "No Vite dev server on :1420 and no dist/ found. \
                 UI will be blank. Run: cd crates/ui && pnpm build"
            );
            return false;
        }
    };

    info!(
        dist = %dist_dir.display(),
        "Vite dev server not running — serving UI from pre-built dist/"
    );

    std::thread::Builder::new()
        .name("ui-fallback-server".into())
        .spawn(move || {
            serve_static(dist_dir);
        })
        .expect("failed to spawn UI fallback server");

    // Give the server a moment to bind before the webview requests index.html
    std::thread::sleep(Duration::from_millis(150));

    true
}

/// Blocking static file server using std::net (no extra deps needed).
///
/// Runs a single-threaded HTTP/1.1 server that serves files from `dist_dir`.
/// For any path that doesn't map to a real file, serves `index.html` (SPA routing).
fn serve_static(dist_dir: PathBuf) {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let listener = match TcpListener::bind("127.0.0.1:1420") {
        Ok(l) => l,
        Err(e) => {
            warn!("Failed to bind UI fallback server on :1420: {e}");
            return;
        }
    };

    info!("UI fallback server listening on http://127.0.0.1:1420");

    for stream in listener.incoming() {
        let mut stream = match stream {
            Ok(s) => s,
            Err(_) => continue,
        };

        // Read the HTTP request (we only need the first line)
        let mut buf = [0u8; 4096];
        let n = match stream.read(&mut buf) {
            Ok(n) if n > 0 => n,
            _ => continue,
        };

        let request = String::from_utf8_lossy(&buf[..n]);
        let path = request
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap_or("/");

        // Resolve file path (strip query string and decode)
        let clean_path = path.split('?').next().unwrap_or(path);
        let clean_path = clean_path.trim_start_matches('/');

        // Security: prevent directory traversal
        let requested = if clean_path.contains("..") {
            dist_dir.join("index.html")
        } else if clean_path.is_empty() {
            dist_dir.join("index.html")
        } else {
            let candidate = dist_dir.join(clean_path);
            if candidate.is_file() {
                candidate
            } else {
                // SPA fallback: serve index.html for unmatched routes
                dist_dir.join("index.html")
            }
        };

        let (status, body, content_type) = match std::fs::read(&requested) {
            Ok(data) => ("200 OK", data, mime_type(&requested)),
            Err(_) => {
                let msg = b"Not Found";
                ("404 Not Found", msg.to_vec(), "text/plain")
            }
        };

        let response = format!(
            "HTTP/1.1 {status}\r\n\
             Content-Type: {content_type}\r\n\
             Content-Length: {}\r\n\
             Access-Control-Allow-Origin: *\r\n\
             Cache-Control: no-cache\r\n\
             Connection: close\r\n\
             \r\n",
            body.len()
        );

        let _ = stream.write_all(response.as_bytes());
        let _ = stream.write_all(&body);
    }
}
