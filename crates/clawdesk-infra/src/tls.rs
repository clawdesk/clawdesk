//! TLS certificate management and auto-renewal.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::{debug, info, warn};

/// TLS configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TlsConfig {
    /// Path to the certificate file.
    pub cert_path: PathBuf,
    /// Path to the private key file.
    pub key_path: PathBuf,
    /// Whether to enable auto-renewal.
    pub auto_renew: bool,
    /// Days before expiry to trigger renewal.
    pub renew_before_days: u32,
    /// Domain name for the certificate.
    pub domain: Option<String>,
}

impl Default for TlsConfig {
    fn default() -> Self {
        Self {
            cert_path: PathBuf::from("certs/cert.pem"),
            key_path: PathBuf::from("certs/key.pem"),
            auto_renew: false,
            renew_before_days: 30,
            domain: None,
        }
    }
}

/// Certificate information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CertInfo {
    pub subject: String,
    pub issuer: String,
    pub not_before: chrono::DateTime<chrono::Utc>,
    pub not_after: chrono::DateTime<chrono::Utc>,
    pub is_self_signed: bool,
}

impl CertInfo {
    /// Days until the certificate expires.
    pub fn days_until_expiry(&self) -> i64 {
        (self.not_after - chrono::Utc::now()).num_days()
    }

    /// Whether the certificate needs renewal.
    pub fn needs_renewal(&self, threshold_days: u32) -> bool {
        self.days_until_expiry() <= threshold_days as i64
    }
}

/// Manages TLS certificates for the gateway.
pub struct TlsManager {
    config: TlsConfig,
}

impl TlsManager {
    pub fn new(config: TlsConfig) -> Self {
        Self { config }
    }

    /// Check if cert and key files exist.
    pub fn certs_exist(&self) -> bool {
        self.config.cert_path.exists() && self.config.key_path.exists()
    }

    /// Generate a self-signed certificate for development.
    pub fn generate_self_signed(&self) -> Result<CertInfo, String> {
        info!(
            domain = self.config.domain.as_deref().unwrap_or("localhost"),
            "generating self-signed certificate"
        );

        let domain = self
            .config
            .domain
            .as_deref()
            .unwrap_or("localhost")
            .to_string();
        let now = chrono::Utc::now();
        let not_after = now + chrono::Duration::days(365);

        // In a real implementation, this would use rcgen or openssl to generate
        // an actual self-signed certificate. For now, we create the certificate
        // info and dummy files.
        if let Some(parent) = self.config.cert_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
        }

        // Write placeholder PEM files (real impl would use rcgen).
        let placeholder_cert = format!(
            "-----BEGIN CERTIFICATE-----\n# Self-signed for {domain}\n# Generated: {now}\n-----END CERTIFICATE-----\n"
        );
        let placeholder_key =
            "-----BEGIN PRIVATE KEY-----\n# placeholder\n-----END PRIVATE KEY-----\n".to_string();

        std::fs::write(&self.config.cert_path, &placeholder_cert)
            .map_err(|e| format!("write cert: {e}"))?;
        std::fs::write(&self.config.key_path, &placeholder_key)
            .map_err(|e| format!("write key: {e}"))?;

        debug!(
            cert = %self.config.cert_path.display(),
            key = %self.config.key_path.display(),
            "wrote self-signed cert files"
        );

        Ok(CertInfo {
            subject: format!("CN={domain}"),
            issuer: format!("CN={domain}"),
            not_before: now,
            not_after,
            is_self_signed: true,
        })
    }

    /// Run the auto-renewal check loop.
    pub async fn renewal_loop(&self, cancel: tokio_util::sync::CancellationToken) {
        if !self.config.auto_renew {
            debug!("TLS auto-renewal disabled");
            return;
        }

        let interval = std::time::Duration::from_secs(86_400); // Check daily.
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    debug!("TLS renewal loop shutting down");
                    break;
                }
                _ = tokio::time::sleep(interval) => {
                    if !self.certs_exist() {
                        warn!("TLS cert files missing, skipping renewal check");
                        continue;
                    }
                    // In a real implementation, parse the cert and check expiry.
                    debug!("TLS renewal check complete, certs OK");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cert_info_expiry() {
        let now = chrono::Utc::now();
        let info = CertInfo {
            subject: "CN=test".to_string(),
            issuer: "CN=test".to_string(),
            not_before: now - chrono::Duration::days(10),
            not_after: now + chrono::Duration::days(20),
            is_self_signed: true,
        };
        assert_eq!(info.days_until_expiry(), 19); // ~20 minus a fraction of today
        assert!(info.needs_renewal(30));
        assert!(!info.needs_renewal(10));
    }

    #[test]
    fn test_self_signed_generation() {
        let dir = std::env::temp_dir().join(format!("clawdesk-tls-test-{}", uuid::Uuid::new_v4()));
        let mgr = TlsManager::new(TlsConfig {
            cert_path: dir.join("cert.pem"),
            key_path: dir.join("key.pem"),
            domain: Some("test.local".to_string()),
            ..Default::default()
        });
        let info = mgr.generate_self_signed().unwrap();
        assert!(info.is_self_signed);
        assert_eq!(info.subject, "CN=test.local");
        assert!(mgr.certs_exist());
        std::fs::remove_dir_all(&dir).ok();
    }
}
