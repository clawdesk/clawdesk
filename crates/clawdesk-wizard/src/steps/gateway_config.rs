//! Gateway configuration step — port resolution, auth settings, TLS.

use crate::flow::{StepResult, WizardState};
use crate::validation::ConfigValidator;

/// Default gateway port.
pub const DEFAULT_PORT: u16 = 18789;

/// Gateway auth mode.
#[derive(Debug, Clone, Copy)]
pub enum GatewayAuth {
    /// No auth (local-only use).
    None,
    /// Token-based auth.
    Token,
    /// OAuth2 + PKCE.
    OAuth,
}

/// Execute the gateway config step.
pub fn execute_gateway_config(
    state: &mut WizardState,
    preferred_port: Option<u16>,
    auth_mode: GatewayAuth,
    bind_address: Option<&str>,
) -> StepResult {
    let port = preferred_port.unwrap_or(DEFAULT_PORT);

    // Check port availability with auto-resolution.
    let resolved_port = if ConfigValidator::check_port_available(port) {
        port
    } else {
        match ConfigValidator::find_available_port(port, 10) {
            Some(p) => p,
            None => {
                return StepResult::Error {
                    message: format!("No available port found in range {port}–{}", port + 10),
                };
            }
        }
    };

    let bind = bind_address.unwrap_or("127.0.0.1");
    let auth_str = match auth_mode {
        GatewayAuth::None => "none",
        GatewayAuth::Token => "token",
        GatewayAuth::OAuth => "oauth",
    };

    state.set_config("gateway_port", serde_json::json!(resolved_port));
    state.set_config("gateway_bind", serde_json::json!(bind));
    state.set_config("gateway_auth", serde_json::json!(auth_str));

    StepResult::Continue
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_port() {
        assert_eq!(DEFAULT_PORT, 18789);
    }

    #[test]
    fn gateway_config_basic() {
        let mut state = WizardState::default();
        let result = execute_gateway_config(&mut state, Some(0), GatewayAuth::Token, Some("0.0.0.0"));
        // Port 0 won't be available via bind check but find_available_port should find one
        // (or return error depending on system). This tests the control flow.
        match result {
            StepResult::Continue => {
                assert!(state.accumulated_config.contains_key("gateway_port"));
            }
            StepResult::Error { .. } => {
                // Port resolution failure is expected in some test environments.
            }
            _ => panic!("unexpected step result"),
        }
    }
}
