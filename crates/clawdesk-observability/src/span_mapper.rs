//! Span Type Mapping
//!
//! Maps custom SpanType enum to OTEL GenAI operations and vice versa.

use crate::genai_conventions::Operation;

/// Map custom SpanType to OTEL GenAI Operation.
pub fn span_type_to_operation(span_type: u64) -> Operation {
    match span_type {
        0 => Operation::CreateAgent,
        1 => Operation::Chat,
        2 => Operation::Chat,
        3 => Operation::ExecuteTool,
        4 => Operation::ExecuteTool,
        5 => Operation::Chat,
        6 => Operation::InvokeAgent,
        7 => Operation::Chat,
        8 => Operation::Chat,
        9 => Operation::Embeddings,
        10 => Operation::ExecuteTool,
        11 => Operation::ExecuteTool,
        12 => Operation::ExecuteTool,
        13 => Operation::Chat,
        14 => Operation::Chat,
        15 => Operation::TextCompletion,
        _ => Operation::Chat,
    }
}

/// Map OTEL GenAI Operation to custom SpanType value.
pub fn operation_to_span_type(operation: Operation) -> u64 {
    match operation {
        Operation::Chat => 1,
        Operation::TextCompletion => 15,
        Operation::Embeddings => 9,
        Operation::CreateAgent => 0,
        Operation::InvokeAgent => 6,
        Operation::ExecuteTool => 3,
    }
}

/// Infer operation from span name (fallback heuristic).
pub fn infer_operation_from_name(name: &str) -> Operation {
    let lower = name.to_lowercase();
    if lower.contains("chat") || lower.contains("llm") {
        Operation::Chat
    } else if lower.contains("tool") || lower.contains("function") {
        Operation::ExecuteTool
    } else if lower.contains("embed") {
        Operation::Embeddings
    } else if lower.contains("agent") {
        Operation::InvokeAgent
    } else if lower.contains("completion") {
        Operation::TextCompletion
    } else {
        Operation::Chat
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_span_type_mapping() {
        assert_eq!(span_type_to_operation(1), Operation::Chat);
        assert_eq!(span_type_to_operation(3), Operation::ExecuteTool);
        assert_eq!(span_type_to_operation(9), Operation::Embeddings);
    }

    #[test]
    fn test_operation_mapping() {
        assert_eq!(operation_to_span_type(Operation::Chat), 1);
        assert_eq!(operation_to_span_type(Operation::ExecuteTool), 3);
        assert_eq!(operation_to_span_type(Operation::Embeddings), 9);
    }

    #[test]
    fn test_name_inference() {
        assert_eq!(infer_operation_from_name("chat completion"), Operation::Chat);
        assert_eq!(
            infer_operation_from_name("tool execution"),
            Operation::ExecuteTool
        );
        assert_eq!(
            infer_operation_from_name("embedding generation"),
            Operation::Embeddings
        );
    }
}
