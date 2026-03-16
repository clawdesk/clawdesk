//! Contract testing — interface verification for ACP and channel protocols.

use serde::{Deserialize, Serialize};

/// A contract test case verifying behavioral guarantees.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractTest {
    pub name: String,
    pub protocol: Protocol,
    pub input: serde_json::Value,
    pub expected_output_shape: serde_json::Value,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    Acp,
    Channel,
    Mcp,
    Provider,
}

/// Contract verification result.
#[derive(Debug, Clone)]
pub struct ContractVerification {
    pub test_name: String,
    pub passed: bool,
    pub message: String,
    pub duration_ms: u64,
}

/// Verify that a JSON value matches an expected shape.
///
/// Shape matching checks keys and value types, not exact values.
pub fn verify_shape(actual: &serde_json::Value, expected_shape: &serde_json::Value) -> bool {
    match (actual, expected_shape) {
        (serde_json::Value::Object(a), serde_json::Value::Object(e)) => {
            e.iter().all(|(key, expected_type)| {
                if let Some(actual_val) = a.get(key) {
                    match expected_type.as_str() {
                        Some("string") => actual_val.is_string(),
                        Some("number") => actual_val.is_number(),
                        Some("boolean") => actual_val.is_boolean(),
                        Some("array") => actual_val.is_array(),
                        Some("object") => actual_val.is_object(),
                        Some("any") => true,
                        _ => verify_shape(actual_val, expected_type),
                    }
                } else {
                    false // Missing required key.
                }
            })
        }
        _ => true, // Non-object types are considered matching.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn shape_verification_passes() {
        let actual = json!({
            "id": "abc123",
            "status": "ok",
            "count": 42
        });
        let shape = json!({
            "id": "string",
            "status": "string",
            "count": "number"
        });
        assert!(verify_shape(&actual, &shape));
    }

    #[test]
    fn shape_verification_fails_on_missing_key() {
        let actual = json!({ "id": "abc" });
        let shape = json!({ "id": "string", "status": "string" });
        assert!(!verify_shape(&actual, &shape));
    }

    #[test]
    fn shape_verification_fails_on_wrong_type() {
        let actual = json!({ "count": "not a number" });
        let shape = json!({ "count": "number" });
        assert!(!verify_shape(&actual, &shape));
    }
}
