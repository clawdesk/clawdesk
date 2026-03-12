//! OAuth2 Authorization Code + PKCE flow engine.
//!
//! Implements RFC 7636 PKCE for secure OAuth authentication:
//! 1. Generate code_verifier + code_challenge
//! 2. Open browser to authorization URL
//! 3. Spin up temporary localhost HTTP server for redirect callback
//! 4. Exchange authorization code for tokens
//! 5. Store tokens in credential vault

use crate::ExtensionError;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::net::SocketAddr;
use tracing::{debug, info};

/// OAuth2 token response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthTokenResponse {
    pub access_token: String,
    pub token_type: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
    #[serde(default)]
    pub scope: Option<String>,
}

/// PKCE code verifier/challenge pair
#[derive(Debug)]
pub struct PkceChallenge {
    /// Random 43-128 char string from safe charset
    pub verifier: String,
    /// BASE64URL(SHA256(verifier))
    pub challenge: String,
}

impl PkceChallenge {
    /// Generate a new PKCE challenge.
    ///
    /// verifier: 43-128 chars from [A-Z, a-z, 0-9, -, ., _, ~]
    /// challenge: BASE64URL(SHA256(verifier))
    /// entropy: 43 chars × log₂(66) ≈ 260 bits
    pub fn generate() -> Self {
        const CHARSET: &[u8] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
        const VERIFIER_LEN: usize = 64;

        let mut rng = rand::thread_rng();
        let verifier: String = (0..VERIFIER_LEN)
            .map(|_| {
                let idx = (rng.next_u32() as usize) % CHARSET.len();
                CHARSET[idx] as char
            })
            .collect();

        let hash = Sha256::digest(verifier.as_bytes());
        let challenge = base64_url_encode(&hash);

        Self {
            verifier,
            challenge,
        }
    }
}

/// Generate a cryptographically random state parameter (128 bits).
pub fn generate_state() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// Build the authorization URL with PKCE parameters.
pub fn build_auth_url(
    auth_url: &str,
    client_id: &str,
    redirect_uri: &str,
    scopes: &[String],
    state: &str,
    pkce: &PkceChallenge,
) -> String {
    let scope = scopes.join(" ");
    format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&code_challenge={}&code_challenge_method=S256&access_type=offline&prompt=consent",
        auth_url,
        urlencoding(client_id),
        urlencoding(redirect_uri),
        urlencoding(&scope),
        urlencoding(state),
        urlencoding(&pkce.challenge)
    )
}

/// Exchange authorization code for tokens.
///
/// Google OAuth requires `client_secret` for installed/web app types.
/// Pass `None` only for providers that support pure PKCE without a secret.
pub async fn exchange_code(
    token_url: &str,
    client_id: &str,
    code: &str,
    redirect_uri: &str,
    code_verifier: &str,
) -> Result<OAuthTokenResponse, ExtensionError> {
    exchange_code_with_secret(token_url, client_id, None, code, redirect_uri, code_verifier).await
}

/// Exchange authorization code for tokens, optionally including client_secret.
pub async fn exchange_code_with_secret(
    token_url: &str,
    client_id: &str,
    client_secret: Option<&str>,
    code: &str,
    redirect_uri: &str,
    code_verifier: &str,
) -> Result<OAuthTokenResponse, ExtensionError> {
    let client = reqwest::Client::new();

    tracing::debug!(
        token_url,
        client_id_len = client_id.len(),
        has_secret = client_secret.is_some(),
        code_len = code.len(),
        redirect_uri,
        "exchanging OAuth code for tokens"
    );

    let mut params = vec![
        ("grant_type", "authorization_code"),
        ("client_id", client_id),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("code_verifier", code_verifier),
    ];
    // Google OAuth requires client_secret for installed/web app clients
    if let Some(secret) = client_secret {
        params.push(("client_secret", secret));
    }

    let response = client
        .post(token_url)
        .form(&params)
        .send()
        .await
        .map_err(|e| ExtensionError::OAuthError(format!("token exchange: {}", e)))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        tracing::error!(
            status = %status,
            body = %body,
            token_url,
            "OAuth token exchange failed"
        );
        return Err(ExtensionError::OAuthError(format!(
            "token exchange failed (HTTP {}): {}",
            status, body
        )));
    }

    tracing::info!("OAuth token exchange successful");
    response
        .json()
        .await
        .map_err(|e| ExtensionError::OAuthError(format!("parse token response: {}", e)))
}

/// Refresh an access token using a refresh token.
pub async fn refresh_token(
    token_url: &str,
    client_id: &str,
    refresh_token: &str,
) -> Result<OAuthTokenResponse, ExtensionError> {
    let client = reqwest::Client::new();

    let response = client
        .post(token_url)
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", client_id),
            ("refresh_token", refresh_token),
        ])
        .send()
        .await
        .map_err(|e| ExtensionError::OAuthError(format!("token refresh: {}", e)))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(ExtensionError::OAuthError(format!(
            "token refresh failed (HTTP {}): {}",
            status, body
        )));
    }

    response
        .json()
        .await
        .map_err(|e| ExtensionError::OAuthError(format!("parse refresh response: {}", e)))
}

/// Run the full OAuth2 PKCE flow:
/// 1. Generate PKCE challenge + state
/// 2. Open browser to auth URL
/// 3. Listen on localhost for redirect
/// 4. Exchange code for tokens
///
/// Google OAuth requires `client_secret` for installed/web app types.
/// Pure public clients (mobile apps) can pass `None`.
pub async fn run_pkce_flow(
    auth_url: &str,
    token_url: &str,
    client_id: &str,
    client_secret: Option<&str>,
    scopes: &[String],
) -> Result<OAuthTokenResponse, ExtensionError> {
    let pkce = PkceChallenge::generate();
    let state = generate_state();

    // Bind to random port on localhost
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| ExtensionError::OAuthError(format!("bind listener: {}", e)))?;

    let addr = listener
        .local_addr()
        .map_err(|e| ExtensionError::OAuthError(format!("get local addr: {}", e)))?;

    let redirect_uri = format!("http://127.0.0.1:{}/callback", addr.port());

    let url = build_auth_url(auth_url, client_id, &redirect_uri, scopes, &state, &pkce);

    info!(port = addr.port(), "opening browser for OAuth");

    // Open browser
    open_browser(&url);

    // Wait for callback (with timeout)
    let code = wait_for_callback(listener, &state).await?;

    // Exchange code for tokens
    let tokens = exchange_code_with_secret(token_url, client_id, client_secret, &code, &redirect_uri, &pkce.verifier).await?;

    info!("OAuth PKCE flow completed successfully");
    Ok(tokens)
}

/// Wait for the OAuth callback on the localhost listener.
async fn wait_for_callback(
    listener: tokio::net::TcpListener,
    expected_state: &str,
) -> Result<String, ExtensionError> {
    let timeout = std::time::Duration::from_secs(300); // 5 minute timeout

    let result = tokio::time::timeout(timeout, async {
        let (mut socket, _) = listener
            .accept()
            .await
            .map_err(|e| ExtensionError::OAuthError(format!("accept: {}", e)))?;

        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let mut buf = vec![0u8; 4096];
        let n = socket
            .read(&mut buf)
            .await
            .map_err(|e| ExtensionError::OAuthError(format!("read: {}", e)))?;

        let request = String::from_utf8_lossy(&buf[..n]);

        // Parse the GET request for query params
        let first_line = request.lines().next().unwrap_or("");
        let path = first_line.split_whitespace().nth(1).unwrap_or("");

        tracing::debug!(path, "OAuth callback received");

        // Extract code and state from query string
        let query = path.split('?').nth(1).unwrap_or("");
        let mut params: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        for pair in query.split('&') {
            if let Some((key, value)) = pair.split_once('=') {
                params.insert(key.to_string(), value.to_string());
            }
        }

        // Verify state
        let state = params.get("state").cloned().unwrap_or_default();
        if state != expected_state {
            let response = "HTTP/1.1 400 Bad Request\r\nContent-Type: text/html\r\n\r\n<h1>CSRF verification failed</h1>";
            let _ = socket.write_all(response.as_bytes()).await;
            return Err(ExtensionError::OAuthError("state mismatch — possible CSRF".into()));
        }

        // Check for error
        if let Some(error) = params.get("error") {
            let desc = params.get("error_description").cloned().unwrap_or_default();
            let response = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n<h1>Authorization failed: {}</h1>", error);
            let _ = socket.write_all(response.as_bytes()).await;
            return Err(ExtensionError::OAuthError(format!("{}: {}", error, desc)));
        }

        let code = params
            .get("code")
            .cloned()
            .ok_or_else(|| ExtensionError::OAuthError("no code in callback".into()))?;

        // Send success response
        let response = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n<h1>✅ Authorization successful!</h1><p>You can close this tab and return to ClawDesk.</p>";
        let _ = socket.write_all(response.as_bytes()).await;

        Ok(code)
    })
    .await;

    match result {
        Ok(r) => r,
        Err(_) => Err(ExtensionError::OAuthError("OAuth callback timed out (5 minutes)".into())),
    }
}

/// Open a URL in the default browser.
fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(url).spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", url])
            .spawn();
    }
}

/// BASE64URL encoding (no padding, URL-safe)
fn base64_url_encode(data: &[u8]) -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    URL_SAFE_NO_PAD.encode(data)
}

/// Minimal URL encoding for query parameters
fn urlencoding(s: &str) -> String {
    s.replace(' ', "%20")
        .replace('&', "%26")
        .replace('=', "%3D")
        .replace('+', "%2B")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_generation() {
        let pkce = PkceChallenge::generate();
        assert!(pkce.verifier.len() >= 43);
        assert!(pkce.verifier.len() <= 128);
        assert!(!pkce.challenge.is_empty());

        // Verify challenge = BASE64URL(SHA256(verifier))
        let hash = Sha256::digest(pkce.verifier.as_bytes());
        let expected = base64_url_encode(&hash);
        assert_eq!(pkce.challenge, expected);
    }

    #[test]
    fn state_generation() {
        let state = generate_state();
        assert_eq!(state.len(), 32); // 16 bytes = 32 hex chars
        // All hex chars
        assert!(state.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn auth_url_building() {
        let pkce = PkceChallenge::generate();
        let url = build_auth_url(
            "https://example.com/auth",
            "client123",
            "http://localhost:8080/callback",
            &["read".to_string(), "write".to_string()],
            "state123",
            &pkce,
        );
        assert!(url.contains("response_type=code"));
        assert!(url.contains("client_id=client123"));
        assert!(url.contains("code_challenge_method=S256"));
    }
}
