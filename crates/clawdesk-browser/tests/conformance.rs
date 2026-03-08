//! Cross-layer browser protocol conformance tests.
//!
//! Ensures that the skill prompt, canonical registry, agent tool
//! implementations, and Tauri-advertised surfaces are consistent.

#[cfg(test)]
mod tests {
    use clawdesk_browser::tool_registry::{BrowserToolId, resolve_alias, is_deprecated_alias};

    /// The skill provider must inject exactly the core 7 tools.
    #[tokio::test]
    async fn skill_injection_matches_canonical_core() {
        use clawdesk_agents::runner::SkillProvider;
        let provider = clawdesk_skills::browser_skill::BrowserSkillProvider::with_availability(true);

        let injection = provider
            .select_skills("please browse to example.com", "s1", None, 1, 4000)
            .await;

        let expected = BrowserToolId::core_tool_names();
        assert_eq!(
            injection.tool_names, expected,
            "skill injection tool names don't match canonical core tools"
        );
        assert_eq!(injection.tool_names.len(), 7);
    }

    /// All canonical tool names must resolve through the alias resolver.
    #[test]
    fn all_canonical_names_resolve() {
        for tool in BrowserToolId::all() {
            let name = tool.canonical_name();
            let resolved = resolve_alias(name);
            assert_eq!(
                resolved,
                Some(*tool),
                "canonical name '{}' failed to resolve",
                name
            );
        }
    }

    /// Deprecated aliases must resolve to canonical IDs.
    #[test]
    fn deprecated_aliases_resolve_correctly() {
        // browser_read_page → browser_extract_text
        assert_eq!(
            resolve_alias("browser_read_page"),
            Some(BrowserToolId::ExtractText)
        );
        // browser_execute_js → browser_eval_js
        assert_eq!(
            resolve_alias("browser_execute_js"),
            Some(BrowserToolId::EvalJs)
        );
    }

    /// Deprecated aliases are flagged as deprecated.
    #[test]
    fn deprecated_flag_is_set() {
        assert!(is_deprecated_alias("browser_read_page"));
        assert!(is_deprecated_alias("browser_execute_js"));
        assert!(!is_deprecated_alias("browser_navigate"));
        assert!(!is_deprecated_alias("browser_extract_text"));
    }

    /// The action registry's parse_tool_call supports all core canonical names.
    #[test]
    fn parse_tool_call_accepts_canonical_names() {
        use clawdesk_browser::parse_tool_call;

        // Navigate
        let action = parse_tool_call(
            "browser_navigate",
            &serde_json::json!({"url": "https://example.com"}),
        );
        assert!(action.is_some(), "browser_navigate should parse");

        // Click with index (preferred)
        let action = parse_tool_call(
            "browser_click",
            &serde_json::json!({"index": 5}),
        );
        assert!(action.is_some(), "browser_click with index should parse");

        // Click with selector (fallback)
        let action = parse_tool_call(
            "browser_click",
            &serde_json::json!({"selector": "#btn"}),
        );
        assert!(action.is_some(), "browser_click with selector should parse");

        // Type with index
        let action = parse_tool_call(
            "browser_type",
            &serde_json::json!({"index": 3, "text": "hello"}),
        );
        assert!(action.is_some(), "browser_type with index should parse");

        // Screenshot
        let action = parse_tool_call("browser_screenshot", &serde_json::json!({}));
        assert!(action.is_some(), "browser_screenshot should parse");
    }

    /// Deprecated alias names are accepted by parse_tool_call.
    #[test]
    fn parse_tool_call_accepts_deprecated_aliases() {
        use clawdesk_browser::parse_tool_call;

        // browser_read_page → browser_extract_text
        let action = parse_tool_call(
            "browser_read_page",
            &serde_json::json!({}),
        );
        assert!(action.is_some(), "browser_read_page alias should parse");

        // browser_execute_js → browser_eval_js
        let action = parse_tool_call(
            "browser_execute_js",
            &serde_json::json!({"expression": "1+1"}),
        );
        assert!(action.is_some(), "browser_execute_js alias should parse");
    }

    /// Index-based targeting survives through parse_tool_call and produces
    /// a data-ci selector (not discarded into selector-only semantics).
    #[test]
    fn index_targeting_preserved_through_parsing() {
        use clawdesk_browser::{parse_tool_call, BrowserAction};

        let action = parse_tool_call(
            "browser_click",
            &serde_json::json!({"index": 42}),
        ).expect("should parse");

        match action {
            BrowserAction::Click { selector } => {
                assert!(
                    selector.contains("data-ci"),
                    "index-based click should use data-ci selector, got: {}",
                    selector
                );
                assert!(
                    selector.contains("42"),
                    "data-ci selector should contain the index number, got: {}",
                    selector
                );
            }
            _ => panic!("expected Click action"),
        }
    }
}
