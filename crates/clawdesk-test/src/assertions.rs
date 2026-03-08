//! Assertion engine for verifying agent responses.

use serde::Serialize;

use crate::suite::StepExpectation;

/// Result of a single assertion check.
#[derive(Debug, Clone, Serialize)]
pub struct AssertionResult {
    /// Human-readable assertion label.
    pub label: String,
    /// Whether the assertion passed.
    pub passed: bool,
    /// Detail message (why it failed or what was checked).
    pub detail: String,
}

/// Assertion evaluator.
pub struct Assertion;

impl Assertion {
    /// Evaluate all expectations against a response body.
    ///
    /// Returns a vec of assertion results (one per check).
    pub fn evaluate(response: &str, expect: &StepExpectation) -> Vec<AssertionResult> {
        let mut results = Vec::new();

        // --- contains ---
        for substr in &expect.contains {
            let lower_resp = response.to_lowercase();
            let lower_sub = substr.to_lowercase();
            let passed = lower_resp.contains(&lower_sub);
            results.push(AssertionResult {
                label: format!("contains '{substr}'"),
                passed,
                detail: if passed {
                    format!("found '{substr}' in response")
                } else {
                    format!("'{substr}' not found in response")
                },
            });
        }

        // --- not_contains ---
        for substr in &expect.not_contains {
            let lower_resp = response.to_lowercase();
            let lower_sub = substr.to_lowercase();
            let passed = !lower_resp.contains(&lower_sub);
            results.push(AssertionResult {
                label: format!("not_contains '{substr}'"),
                passed,
                detail: if passed {
                    format!("'{substr}' correctly absent")
                } else {
                    format!("'{substr}' found in response (unexpected)")
                },
            });
        }

        // --- matches (regex) ---
        if let Some(pattern) = &expect.matches {
            match regex::Regex::new(pattern) {
                Ok(re) => {
                    let passed = re.is_match(response);
                    results.push(AssertionResult {
                        label: format!("matches /{pattern}/"),
                        passed,
                        detail: if passed {
                            "regex matched".into()
                        } else {
                            "regex did not match".into()
                        },
                    });
                }
                Err(e) => {
                    results.push(AssertionResult {
                        label: format!("matches /{pattern}/"),
                        passed: false,
                        detail: format!("invalid regex: {e}"),
                    });
                }
            }
        }

        // --- not_matches (regex) ---
        if let Some(pattern) = &expect.not_matches {
            match regex::Regex::new(pattern) {
                Ok(re) => {
                    let passed = !re.is_match(response);
                    results.push(AssertionResult {
                        label: format!("not_matches /{pattern}/"),
                        passed,
                        detail: if passed {
                            "regex correctly did not match".into()
                        } else {
                            "regex matched (unexpected)".into()
                        },
                    });
                }
                Err(e) => {
                    results.push(AssertionResult {
                        label: format!("not_matches /{pattern}/"),
                        passed: false,
                        detail: format!("invalid regex: {e}"),
                    });
                }
            }
        }

        // --- max_tokens (approximate: word count) ---
        if let Some(max) = expect.max_tokens {
            let word_count = response.split_whitespace().count();
            let passed = word_count <= max;
            results.push(AssertionResult {
                label: format!("max_tokens({max})"),
                passed,
                detail: format!("word count: {word_count}"),
            });
        }

        // --- min_tokens ---
        if let Some(min) = expect.min_tokens {
            let word_count = response.split_whitespace().count();
            let passed = word_count >= min;
            results.push(AssertionResult {
                label: format!("min_tokens({min})"),
                passed,
                detail: format!("word count: {word_count}"),
            });
        }

        // --- is_json ---
        if expect.is_json {
            let passed = serde_json::from_str::<serde_json::Value>(response).is_ok();
            results.push(AssertionResult {
                label: "is_json".into(),
                passed,
                detail: if passed { "valid JSON".into() } else { "not valid JSON".into() },
            });
        }

        // --- json_values ---
        if !expect.json_values.is_empty() {
            match serde_json::from_str::<serde_json::Value>(response) {
                Ok(val) => {
                    for (path, expected) in &expect.json_values {
                        let actual = json_pointer(&val, path);
                        let actual_str = actual.map(|v| match v {
                            serde_json::Value::String(s) => s.clone(),
                            other => other.to_string(),
                        });
                        let passed = actual_str.as_deref() == Some(expected.as_str());
                        results.push(AssertionResult {
                            label: format!("json[{path}] == '{expected}'"),
                            passed,
                            detail: format!("actual: {:?}", actual_str),
                        });
                    }
                }
                Err(e) => {
                    for (path, expected) in &expect.json_values {
                        results.push(AssertionResult {
                            label: format!("json[{path}] == '{expected}'"),
                            passed: false,
                            detail: format!("response is not valid JSON: {e}"),
                        });
                    }
                }
            }
        }

        results
    }
}

/// Simple dot-notation JSON pointer (e.g., "data.name" → val["data"]["name"]).
fn json_pointer<'a>(val: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut current = val;
    for key in path.split('.') {
        match current {
            serde_json::Value::Object(map) => {
                current = map.get(key)?;
            }
            serde_json::Value::Array(arr) => {
                let idx: usize = key.parse().ok()?;
                current = arr.get(idx)?;
            }
            _ => return None,
        }
    }
    Some(current)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn expectation() -> StepExpectation {
        StepExpectation::default()
    }

    #[test]
    fn contains_assertion() {
        let mut exp = expectation();
        exp.contains = vec!["hello".into(), "world".into()];

        let results = Assertion::evaluate("Hello, World!", &exp);
        assert_eq!(results.len(), 2);
        assert!(results[0].passed); // "hello" (case-insensitive)
        assert!(results[1].passed); // "world"
    }

    #[test]
    fn not_contains_assertion() {
        let mut exp = expectation();
        exp.not_contains = vec!["error".into()];

        let results = Assertion::evaluate("Everything is fine!", &exp);
        assert_eq!(results.len(), 1);
        assert!(results[0].passed);

        let results = Assertion::evaluate("An error occurred", &exp);
        assert!(!results[0].passed);
    }

    #[test]
    fn regex_match() {
        let mut exp = expectation();
        exp.matches = Some(r"\d+".into());

        let results = Assertion::evaluate("The answer is 42", &exp);
        assert!(results[0].passed);

        let results = Assertion::evaluate("no numbers here", &exp);
        assert!(!results[0].passed);
    }

    #[test]
    fn token_limits() {
        let mut exp = expectation();
        exp.max_tokens = Some(5);
        exp.min_tokens = Some(2);

        let results = Assertion::evaluate("one two three", &exp);
        assert!(results[0].passed); // 3 ≤ 5
        assert!(results[1].passed); // 3 ≥ 2

        let results = Assertion::evaluate("one two three four five six", &exp);
        assert!(!results[0].passed); // 6 > 5
    }

    #[test]
    fn json_validation() {
        let mut exp = expectation();
        exp.is_json = true;
        exp.json_values.insert("data.name".into(), "Alice".into());

        let json = r#"{"data": {"name": "Alice", "age": 30}}"#;
        let results = Assertion::evaluate(json, &exp);
        assert!(results[0].passed); // is_json
        assert!(results[1].passed); // data.name == "Alice"

        let results = Assertion::evaluate("not json", &exp);
        assert!(!results[0].passed);
    }

    #[test]
    fn empty_expectation_no_assertions() {
        let exp = expectation();
        let results = Assertion::evaluate("anything", &exp);
        assert!(results.is_empty());
    }
}
