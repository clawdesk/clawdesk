//! Property-based testing helpers using proptest strategies.

use proptest::prelude::*;

/// Strategy for generating valid model IDs.
pub fn model_id_strategy() -> impl Strategy<Value = String> {
    prop::string::string_regex("[a-z][a-z0-9-]{2,40}(/[a-z][a-z0-9-]{2,40}){0,2}")
        .unwrap()
}

/// Strategy for generating chat messages with controlled content.
pub fn message_strategy() -> impl Strategy<Value = String> {
    prop::string::string_regex("[a-zA-Z0-9 .,!?\\n]{1,500}")
        .unwrap()
}

/// Strategy for generating JSON-like configuration values.
pub fn config_value_strategy() -> impl Strategy<Value = serde_json::Value> {
    prop_oneof![
        Just(serde_json::Value::Null),
        any::<bool>().prop_map(serde_json::Value::Bool),
        any::<i64>().prop_map(|n| serde_json::Value::Number(n.into())),
        "[a-zA-Z0-9_]{1,50}".prop_map(|s| serde_json::Value::String(s)),
    ]
}

/// Strategy for generating port numbers.
pub fn port_strategy() -> impl Strategy<Value = u16> {
    1024u16..=65535u16
}

/// Strategy for generating session keys.
pub fn session_key_strategy() -> impl Strategy<Value = String> {
    prop::string::string_regex("agent:[a-z]+(:subagent:[a-z]+){0,3}")
        .unwrap()
}

/// Property test helper — assert invariant holds for N random inputs.
pub fn assert_invariant<T: std::fmt::Debug, F: Fn(&T) -> bool>(
    strategy: impl Strategy<Value = T>,
    invariant: F,
    description: &str,
) {
    let mut runner = proptest::test_runner::TestRunner::default();
    let result = runner.run(&strategy, |value| {
        prop_assert!(
            invariant(&value),
            "invariant violated for {:?}: {}",
            value,
            description
        );
        Ok(())
    });
    if let Err(e) = result {
        panic!("property test failed: {} — {}", description, e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    proptest! {
        #[test]
        fn model_ids_are_nonempty(id in model_id_strategy()) {
            prop_assert!(!id.is_empty());
        }

        #[test]
        fn messages_are_bounded(msg in message_strategy()) {
            prop_assert!(msg.len() <= 500);
        }

        #[test]
        fn ports_are_valid(port in port_strategy()) {
            prop_assert!(port >= 1024);
        }
    }
}
