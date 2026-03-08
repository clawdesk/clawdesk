//! Integration tests for the ACP crate.
//!
//! Tests multi-phase protocol interactions across ACP modules.

#[cfg(test)]
mod tests {
    use clawdesk_acp::agent_card::AgentCard;
    use clawdesk_acp::capability::{CapSet, CapabilityId};
    use clawdesk_acp::router::{AgentDirectory, AgentRouter, RoutingDecision};
    use clawdesk_acp::task::{Task, TaskEvent, TaskState};
    use clawdesk_acp::content_router::ContentRouterBuilder;
    use clawdesk_acp::discovery::{DiscoveryCache, DiscoveryCacheConfig};
    use clawdesk_acp::error::{AcpError, AcpErrorKind};

    fn make_agent_card(id: &str, caps: Vec<CapabilityId>) -> AgentCard {
        let mut card = AgentCard::new(id, id, format!("http://localhost:8080/agents/{id}"));
        card.set_capabilities(caps);
        card
    }

    // ---- Phase 1: Agent Directory & Discovery ----

    #[test]
    fn phase1_register_and_lookup() {
        let mut dir = AgentDirectory::new();
        let card = make_agent_card("agent-1", vec![CapabilityId::TextGeneration, CapabilityId::CodeExecution]);
        dir.register(card);

        let found = dir.get("agent-1");
        assert!(found.is_some());
        assert_eq!(found.unwrap().card.name, "agent-1");
    }

    #[test]
    fn phase1_deregister() {
        let mut dir = AgentDirectory::new();
        dir.register(make_agent_card("agent-1", vec![CapabilityId::TextGeneration]));
        dir.deregister("agent-1");
        assert!(dir.get("agent-1").is_none());
    }

    // ---- Phase 2: Capability-Based Routing ----

    #[test]
    fn phase2_route_to_best_agent() {
        let mut dir = AgentDirectory::new();
        dir.register(make_agent_card("coder", vec![CapabilityId::TextGeneration, CapabilityId::CodeExecution]));
        dir.register(make_agent_card("artist", vec![CapabilityId::ImageProcessing]));

        let router = AgentRouter::new();
        let decision = router.route(&dir, &[CapabilityId::CodeExecution], &[]);

        match decision {
            RoutingDecision::Route { agent_id, .. } => assert_eq!(agent_id, "coder"),
            _ => panic!("Expected Route decision"),
        }
    }

    #[test]
    fn phase2_no_matching_agent() {
        let mut dir = AgentDirectory::new();
        dir.register(make_agent_card("artist", vec![CapabilityId::ImageProcessing]));

        let router = AgentRouter::new();
        // Use Mathematics — completely disjoint from ImageProcessing/MediaProcessing hierarchy
        let decision = router.route(&dir, &[CapabilityId::Mathematics], &[]);

        assert!(matches!(decision, RoutingDecision::NoMatch { .. }));
    }

    // ---- Phase 3: Typed Capability Bitfield ----

    #[test]
    fn phase3_capset_routing_equivalence() {
        let mut caps_a: CapSet = CapSet::empty();
        caps_a.insert(CapabilityId::TextGeneration);
        caps_a.insert(CapabilityId::CodeGeneration);

        let mut required: CapSet = CapSet::empty();
        required.insert(CapabilityId::CodeGeneration);

        let overlap = caps_a.overlap_score(&required);
        assert!((overlap - 1.0).abs() < 0.01);
    }

    // ---- Phase 4: Task State Machine ----

    #[test]
    fn phase4_task_lifecycle_happy_path() {
        let mut task = Task::new("agent-a", "agent-b", serde_json::json!({"prompt": "hello"}));
        assert_eq!(task.state, TaskState::Submitted);

        task.apply_event(TaskEvent::Work).unwrap();
        assert_eq!(task.state, TaskState::Working);

        task.apply_event(TaskEvent::Complete { output: serde_json::json!({"result": "done"}) }).unwrap();
        assert_eq!(task.state, TaskState::Completed);
    }

    #[test]
    fn phase4_task_failure_path() {
        let mut task = Task::new("agent-a", "agent-b", serde_json::json!({}));
        task.apply_event(TaskEvent::Work).unwrap();
        task.apply_event(TaskEvent::Fail { error: "broke".into() }).unwrap();
        assert_eq!(task.state, TaskState::Failed);
    }

    #[test]
    fn phase4_invalid_transition() {
        let mut task = Task::new("a", "b", serde_json::json!({}));
        let result = task.apply_event(TaskEvent::Complete { output: serde_json::json!({}) });
        assert!(result.is_err());
    }

    // ---- Phase 5: Content-Based Routing ----

    #[test]
    fn phase5_content_routing_integration() {
        let router = ContentRouterBuilder::new()
            .rule("rust", &["coder"], 2.0)
            .rule("image", &["vision"], 2.0)
            .rule("transcribe", &["audio"], 3.0)
            .build();

        let result = router.route("Please write some rust code to process an image");
        assert!(result.agent_scores.contains_key("coder"));
        assert!(result.agent_scores.contains_key("vision"));
        assert_eq!(result.matched_patterns.len(), 2);
    }

    // ---- Phase 6: Discovery Cache ----

    #[test]
    fn phase6_discovery_cache_put_get() {
        let config = DiscoveryCacheConfig::default();
        let mut cache = DiscoveryCache::new(config);

        let card = make_agent_card("cached-agent", vec![]);
        cache.put("cached-agent", card.clone(), None);

        let cached = cache.get("cached-agent");
        assert!(cached.is_some());
        assert_eq!(cached.unwrap().id, "cached-agent");
    }

    // ---- Phase 7: Error Propagation ----

    #[test]
    fn phase7_error_causal_chain() {
        let root = AcpError::new(AcpErrorKind::Network { detail: "timeout".into() });
        let mid = AcpError::new(AcpErrorKind::DiscoveryFailed {
            url: "http://agent.local".into(),
            detail: "unreachable".into(),
        }).with_source(Box::new(root));
        let top = AcpError::new(AcpErrorKind::NoMatchingAgent {
            required_capabilities: vec!["text".into()],
        }).with_source(Box::new(mid));

        let chain = top.causal_chain();
        assert!(chain.len() >= 1);
    }

    // ---- Phase 8: End-to-End ----

    #[test]
    fn phase8_end_to_end() {
        // 1. Register.
        let mut dir = AgentDirectory::new();
        dir.register(make_agent_card("nlp", vec![CapabilityId::TextGeneration]));
        dir.register(make_agent_card("vision", vec![CapabilityId::ImageProcessing]));

        // 2. Cache.
        let mut cache = DiscoveryCache::new(DiscoveryCacheConfig::default());
        for card in dir.list() {
            cache.put(&card.id, card.clone(), None);
        }

        // 3. Content route.
        let content_router = ContentRouterBuilder::new()
            .rule("summarize", &["nlp"], 3.0)
            .rule("image", &["vision"], 2.0)
            .build();
        let result = content_router.route("Please summarize this document");
        assert_eq!(result.best_agent.as_deref(), Some("nlp"));

        // 4. Verify cached.
        assert!(cache.get("nlp").is_some());

        // 5. Task creation.
        let task = Task::new("nlp", "nlp", serde_json::json!({"text": "Summarize"}));
        assert_eq!(task.state, TaskState::Submitted);
    }
}
