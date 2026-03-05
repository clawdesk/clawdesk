    // ── Three-Phase Team Orchestration ─────────────────────────────────
    //
    // Architecture (principal-engineer design, informed by OpenFang research):
    //
    // When the agent is a team router with teammates, we orchestrate in
    // three phases — each with a fail-open fallback:
    //
    //   Phase 1 — DECOMPOSITION: Router LLM analyzes the user's request
    //     and crafts specific, actionable sub-tasks per specialist.
    //     Transforms "design a cake shop" → "Write a business plan with
    //     TAM/SAM/SOM, revenue projections Y1-3, pricing per category…"
    //     Fallback: generic role-based framing.
    //
    //   Phase 2 — PARALLEL DISPATCH: Each specialist receives their bespoke
    //     sub-task (not the raw user message). Full agent loop with persona
    //     as system prompt. Bounded concurrency via llm_concurrency semaphore.
    //     Fallback: individual agent errors captured, team continues.
    //
    //   Phase 3 — SYNTHESIS: Router LLM reads all specialist outputs and
    //     produces a coherent, unified document. Streamed live to the user.
    //     Fallback: concatenated results with attribution headers.
    //
    // Total wall-clock ≈ decomposition + max(specialist latencies) + synthesis.
    // Provider resolution: model="default" reuses the router's Arc (O(1)).

    // Register this run in the cancellation registry BEFORE any LLM call.
    {
        let mut active = state.active_chat_runs.write().await;
        active.insert(chat_id.clone(), run_cancel.clone());
    }

    let team_response: Option<clawdesk_agents::runner::AgentResponse> = {
        let mut team_result: Option<clawdesk_agents::runner::AgentResponse> = None;

        if agent.team_role.as_deref() == Some("router") {
            if let Some(ref team_id) = agent.team_id {
                // Gather teammates (deterministic ordering by name).
                let teammates: Vec<DesktopAgent> = {
                    let agents_guard = state.agents.read().map_err(|e| e.to_string())?;
                    let mut mates: Vec<_> = agents_guard
                        .values()
                        .filter(|a| a.team_id.as_deref() == Some(team_id.as_str()) && a.id != agent.id)
                        .cloned()
                        .collect();
                    mates.sort_by(|a, b| a.name.cmp(&b.name));
                    mates
                };

                if !teammates.is_empty() {
                    // ── Provider Resolution (synchronous, no locks across awaits) ──
                    let mut resolved_mates: Vec<(DesktopAgent, String, Arc<dyn clawdesk_providers::Provider>)> =
                        Vec::with_capacity(teammates.len());

                    for mate in &teammates {
                        let is_default = mate.model.is_empty()
                            || mate.model == "default"
                            || mate.model == "auto";

                        let (resolved_model, resolved_provider) = if is_default {
                            (model_full_id.clone(), Arc::clone(&provider_for_team))
                        } else {
                            let mid = AppState::resolve_model_id(&mate.model);
                            let p = {
                                use clawdesk_providers::capability::ProviderCaps;
                                let negotiator = state.negotiator.read().map_err(|e| e.to_string())?;
                                let required = ProviderCaps::TEXT_COMPLETION.union(ProviderCaps::SYSTEM_PROMPT);
                                match negotiator.resolve_model(&mid, required) {
                                    Some((p, _)) => Arc::clone(p),
                                    None => {
                                        drop(negotiator);
                                        state.resolve_provider(&mate.model)?
                                    }
                                }
                            };
                            (mid, p)
                        };
                        resolved_mates.push((mate.clone(), resolved_model, resolved_provider));
                    }

                    tracing::info!(
                        team_id = %team_id,
                        router = %agent.name,
                        members = resolved_mates.len(),
                        "Three-phase team orchestration: providers resolved"
                    );

                    // Collect agent metadata for later reference (synthesis headers).
                    let agent_meta: Vec<(String, String, String)> = resolved_mates.iter()
                        .map(|(m, _, _)| (
                            m.id.clone(),
                            m.name.clone(),
                            m.team_role.clone().unwrap_or_else(|| "specialist".to_string()),
                        ))
                        .collect();

                    // ════════════════════════════════════════════════════════════
                    // PHASE 1: TASK DECOMPOSITION via Router LLM
                    // ════════════════════════════════════════════════════════════
                    //
                    // The router LLM analyzes the user's request and produces
                    // specific, actionable sub-tasks per specialist. This is the
                    // critical quality multiplier — it transforms vague requests
                    // into concrete deliverable specifications.

                    let _ = app.emit("agent:event", serde_json::json!({
                        "agent_id": agent.id,
                        "event": "StreamChunk",
                        "data": "🧠 **Analyzing request and planning team tasks...**\n\n",
                    }));

                    // Build decomposition prompt with team roster.
                    let mut roster_for_decomp = String::new();
                    for (mate, _, _) in &resolved_mates {
                        let role = mate.team_role.as_deref().unwrap_or("specialist");
                        let hint: String = mate.persona.chars().take(300).collect();
                        roster_for_decomp.push_str(&format!(
                            "- **{}** (agent_id: `{}`, role: {})\n  Capabilities: {}\n",
                            mate.name, mate.id, role, hint.trim(),
                        ));
                    }

                    let decomp_system_prompt = format!(
                        "You are a task decomposition engine for a multi-agent team. \
                        Your ONLY job is to analyze the user's request and break it into \
                        SPECIFIC, ACTIONABLE sub-tasks — one per team member.\n\n\
                        ## Team Members\n\n{}\n\n\
                        ## Output Format\n\n\
                        Output ONLY a JSON object with this exact structure:\n\
                        ```json\n\
                        {{\n\
                          \"tasks\": [\n\
                            {{\n\
                              \"agent_id\": \"<exact agent_id from above>\",\n\
                              \"task\": \"Detailed, specific task with expected deliverable format\"\n\
                            }}\n\
                          ]\n\
                        }}\n\
                        ```\n\n\
                        ## Rules\n\n\
                        - Each task must be HYPER-SPECIFIC: include expected sections, data points, \
                        metrics, formats, and exact deliverables.\n\
                        - BAD task: \"Handle the business aspects of a cake shop\"\n\
                        - GOOD task: \"Write a complete Business Plan section containing: 1) Market \
                        analysis with TAM/SAM/SOM size estimates and 3 local competitors with their \
                        strengths/weaknesses, 2) Revenue projections for years 1-3 with monthly \
                        breakdown for year 1 and assumptions stated, 3) Pricing strategy with specific \
                        cake categories (wedding, custom, daily) and price ranges per category, \
                        4) Break-even analysis with fixed costs (rent, staff, equipment) and variable \
                        costs (ingredients, packaging) itemized\"\n\
                        - Each task should produce a COMPLETE, STANDALONE deliverable section.\n\
                        - The specialist should be able to WRITE the actual content, not just plan it.\n\
                        - Only assign to agents whose role matches. Skip agents if the request \
                        doesn't need their expertise.\n\
                        - Output ONLY the JSON. No other text before or after.",
                        roster_for_decomp,
                    );

                    let decomp_request = clawdesk_providers::ProviderRequest {
                        model: model_full_id.clone(),
                        messages: vec![
                            clawdesk_providers::ChatMessage::new(
                                clawdesk_providers::MessageRole::User,
                                request.content.as_str(),
                            ),
                        ],
                        system_prompt: Some(decomp_system_prompt),
                        max_tokens: Some(2048),
                        temperature: Some(0.3),
                        tools: vec![],
                        stream: false,
                    };

                    // Acquire concurrency permit for decomposition LLM call.
                    let decomp_permit = state.llm_concurrency.acquire().await
                        .map_err(|_| "LLM concurrency semaphore closed".to_string())?;

                    let decomp_result = tokio::time::timeout(
                        tokio::time::Duration::from_secs(60),
                        provider_for_team.complete(&decomp_request),
                    ).await;

                    drop(decomp_permit);

                    // Parse decomposition result into per-agent tasks.
                    // Fallback: generic role-based framing if parsing or LLM fails.
                    let generic_fallback = |mates: &[(DesktopAgent, String, Arc<dyn clawdesk_providers::Provider>)]| -> Vec<(String, String)> {
                        mates.iter().map(|(m, _, _)| {
                            let role = m.team_role.as_deref().unwrap_or("specialist");
                            (m.id.clone(), format!(
                                "You are the team's {} specialist. Produce a COMPLETE, DETAILED \
                                section for your area of expertise. Write the actual deliverable \
                                content — not a list of what you would do. Be specific with numbers, \
                                names, timelines, and actionable steps.\n\nRequest: {}",
                                role, request.content,
                            ))
                        }).collect()
                    };

                    let mut decomp_tokens_in: u64 = 0;
                    let mut decomp_tokens_out: u64 = 0;

                    let agent_tasks: Vec<(String, String)> = match decomp_result {
                        Ok(Ok(response)) => {
                            decomp_tokens_in = response.usage.input_tokens;
                            decomp_tokens_out = response.usage.output_tokens;
                            tracing::info!(
                                tokens_in = response.usage.input_tokens,
                                tokens_out = response.usage.output_tokens,
                                "Phase 1 decomposition: LLM responded"
                            );

                            // Try to extract JSON from response (may be wrapped in ```json...```).
                            let content = &response.content;
                            let parsed_tasks: Option<Vec<(String, String)>> = (|| {
                                let json_str = {
                                    let start = content.find('{')?;
                                    let end = content.rfind('}')?;
                                    if end <= start { return None; }
                                    &content[start..=end]
                                };

                                let parsed: serde_json::Value = serde_json::from_str(json_str).ok()?;
                                let tasks = parsed.get("tasks")?.as_array()?;

                                let mut result = Vec::new();
                                for task_obj in tasks {
                                    let aid = task_obj.get("agent_id")?.as_str()?;
                                    let task_text = task_obj.get("task")?.as_str()?;
                                    if resolved_mates.iter().any(|(m, _, _)| m.id == aid) {
                                        result.push((aid.to_string(), task_text.to_string()));
                                    }
                                }
                                if result.is_empty() { None } else { Some(result) }
                            })();

                            match parsed_tasks {
                                Some(tasks) => {
                                    tracing::info!(
                                        task_count = tasks.len(),
                                        "Phase 1 decomposition: parsed specific tasks"
                                    );
                                    // Emit the decomposed task plan to user.
                                    let mut plan_text = format!(
                                        "📋 **Task plan** ({} specialist{}):\n\n",
                                        tasks.len(),
                                        if tasks.len() == 1 { "" } else { "s" },
                                    );
                                    for (aid, task_text) in &tasks {
                                        let name = agent_meta.iter()
                                            .find(|(id, _, _)| id == aid)
                                            .map(|(_, n, _)| n.as_str())
                                            .unwrap_or("Unknown");
                                        let preview = if task_text.len() > 150 {
                                            format!("{}…", &task_text[..task_text.char_indices().take_while(|(i, _)| *i < 150).last().map(|(i, c)| i + c.len_utf8()).unwrap_or(150)])
                                        } else {
                                            task_text.clone()
                                        };
                                        plan_text.push_str(&format!("- **{}**: {}\n", name, preview));
                                    }
                                    plan_text.push_str("\n---\n\n");
                                    let _ = app.emit("agent:event", serde_json::json!({
                                        "agent_id": agent.id,
                                        "event": "StreamChunk",
                                        "data": &plan_text,
                                    }));
                                    tasks
                                }
                                None => {
                                    tracing::warn!(
                                        raw_len = content.len(),
                                        "Phase 1 decomposition: JSON parse failed, using generic framing"
                                    );
                                    let _ = app.emit("agent:event", serde_json::json!({
                                        "agent_id": agent.id,
                                        "event": "StreamChunk",
                                        "data": "⚠️ Task decomposition couldn't parse — using general assignment.\n\n---\n\n",
                                    }));
                                    generic_fallback(&resolved_mates)
                                }
                            }
                        }
                        Ok(Err(e)) => {
                            tracing::warn!(error = %e, "Phase 1 decomposition: provider error, using generic framing");
                            let _ = app.emit("agent:event", serde_json::json!({
                                "agent_id": agent.id,
                                "event": "StreamChunk",
                                "data": "⚠️ Task decomposition failed — using general assignment.\n\n---\n\n",
                            }));
                            generic_fallback(&resolved_mates)
                        }
                        Err(_) => {
                            tracing::warn!("Phase 1 decomposition: timed out after 60s, using generic framing");
                            let _ = app.emit("agent:event", serde_json::json!({
                                "agent_id": agent.id,
                                "event": "StreamChunk",
                                "data": "⚠️ Task decomposition timed out — using general assignment.\n\n---\n\n",
                            }));
                            generic_fallback(&resolved_mates)
                        }
                    };

                    // ════════════════════════════════════════════════════════════
                    // PHASE 2: PARALLEL SPECIALIST DISPATCH
                    // ════════════════════════════════════════════════════════════
                    //
                    // Each specialist receives their bespoke sub-task from Phase 1.
                    // Full agent loop with persona as system prompt.

                    // Build a task lookup: agent_id → decomposed task.
                    let task_map: std::collections::HashMap<String, String> =
                        agent_tasks.into_iter().collect();

                    let mut handles = Vec::with_capacity(resolved_mates.len());

                    for (mate, resolved_model, mate_provider) in resolved_mates {
                        let mate_name = mate.name.clone();
                        let mate_role = mate.team_role.clone().unwrap_or_else(|| "specialist".to_string());
                        let mate_persona = mate.persona.clone();
                        let mate_id = mate.id.clone();
                        // Look up the decomposed task for this agent. Fallback to generic.
                        let task = task_map.get(&mate_id).cloned().unwrap_or_else(|| {
                            format!(
                                "Produce a complete, detailed section for your area of \
                                expertise ({}). Write the actual deliverable — not a list \
                                of plans. Request: {}",
                                mate_role, request.content,
                            )
                        });

                        let cancel = run_cancel.clone();
                        let tools = Arc::clone(&state.tool_registry);
                        let sandbox_eng = Arc::clone(&state.sandbox_engine);
                        let memory = Arc::clone(&state.memory);
                        let app_clone = app.clone();
                        let router_agent_id = agent.id.clone();
                        let llm_sem = Arc::clone(&state.llm_concurrency);

                        handles.push(tokio::spawn(async move {
                            // Acquire concurrency permit — bounds total parallel LLM
                            // calls, preventing rate-limit exhaustion.
                            let _permit = llm_sem.acquire().await
                                .map_err(|_| "LLM concurrency semaphore closed".to_string())?;

                            let _ = app_clone.emit("agent:event", serde_json::json!({
                                "agent_id": router_agent_id,
                                "event": "StreamChunk",
                                "data": format!("⏳ **{}** ({}) is working...\n", mate_name, mate_role),
                            }));

                            let config = clawdesk_agents::AgentConfig {
                                model: resolved_model,
                                system_prompt: String::new(),
                                max_tool_rounds: 5,
                                ..Default::default()
                            };
                            let runner = clawdesk_agents::AgentRunner::new(
                                mate_provider, tools, config, cancel,
                            )
                            .with_sandbox_gate(Arc::new(crate::commands::SandboxGateAdapter {
                                engine: sandbox_eng,
                            }))
                            .with_memory_recall({
                                let mem = memory;
                                Arc::new(move |query: String| {
                                    let mem = Arc::clone(&mem);
                                    Box::pin(async move {
                                        match mem.recall(&query, Some(5)).await {
                                            Ok(results) => results.into_iter().filter_map(|r| {
                                                let text = r.content?;
                                                if text.is_empty() { return None; }
                                                Some(clawdesk_agents::MemoryRecallResult {
                                                    relevance: r.score as f64,
                                                    source: r.metadata.get("source")
                                                        .and_then(|v| v.as_str())
                                                        .map(String::from),
                                                    content: text,
                                                })
                                            }).collect(),
                                            Err(_) => vec![],
                                        }
                                    })
                                })
                            });

                            let sub_history = vec![
                                clawdesk_providers::ChatMessage::new(
                                    clawdesk_providers::MessageRole::User,
                                    task.as_str(),
                                ),
                            ];
                            let timeout = tokio::time::Duration::from_secs(180);
                            let result = match tokio::time::timeout(timeout, runner.run(sub_history, mate_persona)).await {
                                Ok(Ok(response)) => {
                                    tracing::info!(
                                        agent = %mate_name,
                                        tokens_in = response.input_tokens,
                                        tokens_out = response.output_tokens,
                                        rounds = response.total_rounds,
                                        "Phase 2 dispatch: sub-agent completed"
                                    );
                                    Ok((response.content, response.input_tokens, response.output_tokens))
                                }
                                Ok(Err(e)) => Err(format!("Agent error: {e}")),
                                Err(_) => Err("Timed out after 180s".to_string()),
                            };

                            let status_icon = if result.is_ok() { "✅" } else { "❌" };
                            let _ = app_clone.emit("agent:event", serde_json::json!({
                                "agent_id": router_agent_id,
                                "event": "StreamChunk",
                                "data": format!("{} **{}** completed.\n", status_icon, mate_name),
                            }));

                            Ok::<_, String>((mate_name, mate_role, result))
                        }));
                    }

                    // Collect Phase 2 results.
                    let mut dispatch_results: Vec<(String, String, Result<(String, u64, u64), String>)> =
                        Vec::with_capacity(handles.len());
                    for handle in handles {
                        match handle.await {
                            Ok(Ok(tuple)) => dispatch_results.push(tuple),
                            Ok(Err(e)) => tracing::error!(error = %e, "Phase 2 dispatch: sub-agent task error"),
                            Err(e) => tracing::error!(error = %e, "Phase 2 dispatch: join error"),
                        }
                    }

                    let succeeded = dispatch_results.iter().filter(|(_, _, r)| r.is_ok()).count();
                    let p2_input_tokens: u64 = dispatch_results.iter()
                        .filter_map(|(_, _, r)| r.as_ref().ok()).map(|(_, i, _)| i).sum();
                    let p2_output_tokens: u64 = dispatch_results.iter()
                        .filter_map(|(_, _, r)| r.as_ref().ok()).map(|(_, _, o)| o).sum();

                    tracing::info!(
                        team_id = %team_id,
                        total = dispatch_results.len(),
                        succeeded = succeeded,
                        failed = dispatch_results.len() - succeeded,
                        p2_input_tokens,
                        p2_output_tokens,
                        "Phase 2 dispatch: all sub-agents completed"
                    );

                    if succeeded == 0 {
                        // All specialists failed — fall through to normal router path.
                        tracing::warn!("Phase 2 dispatch: all sub-agents failed, falling back to direct router response");
                    } else {
                        // ════════════════════════════════════════════════════════════
                        // PHASE 3: SYNTHESIS via Router LLM
                        // ════════════════════════════════════════════════════════════
                        //
                        // The router LLM reads all specialist outputs and produces a
                        // coherent, unified document. Streamed live to the user.
                        // Fallback: concatenated results with attribution headers.

                        let _ = app.emit("agent:event", serde_json::json!({
                            "agent_id": agent.id,
                            "event": "StreamChunk",
                            "data": "\n---\n\n✍️ **Synthesizing team outputs into a unified response...**\n\n",
                        }));

                        // Build synthesis context with all specialist outputs.
                        let mut specialist_sections = String::new();
                        for (name, role, result) in &dispatch_results {
                            specialist_sections.push_str(&format!("### {} ({})\n\n", name, role));
                            match result {
                                Ok((content, _, _)) => {
                                    specialist_sections.push_str(content);
                                    specialist_sections.push_str("\n\n");
                                }
                                Err(e) => {
                                    specialist_sections.push_str(&format!(
                                        "*[This specialist encountered an error: {}]*\n\n", e
                                    ));
                                }
                            }
                        }

                        let synth_system_prompt = format!(
                            "You are a synthesis engine. Your team of {} specialists has each \
                            produced a section in response to the user's request. Your job is to \
                            combine their work into a SINGLE, POLISHED, COHERENT document.\n\n\
                            ## Rules\n\n\
                            - Preserve ALL substantive content from each specialist.\n\
                            - Remove redundancy — if two specialists cover the same point, keep \
                            the more detailed version.\n\
                            - Add smooth transitions between sections so it reads as one document.\n\
                            - Use clear headings and professional Markdown formatting.\n\
                            - Maintain a consistent, professional tone throughout.\n\
                            - Do NOT add new information — only organize and polish what's provided.\n\
                            - Do NOT mention the team, specialists, or synthesis process.\n\
                            - Write as if YOU are the single author of this complete document.\n\
                            - The output should be the final deliverable, ready for the user.",
                            succeeded,
                        );

                        let synth_user_msg = format!(
                            "Original request: {}\n\n\
                            ---\n\n\
                            ## Specialist Reports\n\n{}\n\
                            ---\n\n\
                            Combine the above into a single, coherent document that directly \
                            addresses the original request. Write the complete output now.",
                            request.content,
                            specialist_sections,
                        );

                        let synth_request = clawdesk_providers::ProviderRequest {
                            model: model_full_id.clone(),
                            messages: vec![
                                clawdesk_providers::ChatMessage::new(
                                    clawdesk_providers::MessageRole::User,
                                    synth_user_msg.as_str(),
                                ),
                            ],
                            system_prompt: Some(synth_system_prompt),
                            max_tokens: Some(8192),
                            temperature: Some(0.4),
                            tools: vec![],
                            stream: true,
                        };

                        // Acquire concurrency permit for synthesis.
                        let synth_permit = state.llm_concurrency.acquire().await
                            .map_err(|_| "LLM concurrency semaphore closed".to_string())?;

                        // Stream synthesis directly to user.
                        let (synth_tx, mut synth_rx) =
                            tokio::sync::mpsc::channel::<clawdesk_providers::StreamChunk>(64);

                        let synth_provider = Arc::clone(&provider_for_team);
                        let synth_handle = tokio::spawn(async move {
                            synth_provider.stream(&synth_request, synth_tx).await
                        });

                        let mut synth_content = String::new();
                        let mut synth_tokens_in: u64 = 0;
                        let mut synth_tokens_out: u64 = 0;

                        while let Some(chunk) = synth_rx.recv().await {
                            if !chunk.delta.is_empty() {
                                synth_content.push_str(&chunk.delta);
                                let _ = app.emit("agent:event", serde_json::json!({
                                    "agent_id": agent.id,
                                    "event": "StreamChunk",
                                    "data": &chunk.delta,
                                }));
                            }
                            if let Some(ref usage) = chunk.usage {
                                synth_tokens_in = usage.input_tokens;
                                synth_tokens_out = usage.output_tokens;
                            }
                        }

                        // Wait for the provider stream task to complete.
                        let synth_stream_result = synth_handle.await;
                        drop(synth_permit);

                        let synthesis_ok = match synth_stream_result {
                            Ok(Ok(())) => true,
                            Ok(Err(e)) => {
                                tracing::warn!(error = %e, "Phase 3 synthesis: provider stream error");
                                false
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "Phase 3 synthesis: join error");
                                false
                            }
                        };

                        let final_content = if synthesis_ok && !synth_content.is_empty() {
                            tracing::info!(
                                synth_len = synth_content.len(),
                                synth_tokens_in,
                                synth_tokens_out,
                                "Phase 3 synthesis: completed successfully"
                            );
                            synth_content
                        } else {
                            // Synthesis failed — fall back to concatenated results
                            // with attribution headers (structural coordination).
                            tracing::warn!("Phase 3 synthesis: failed or empty, falling back to concatenated output");
                            let _ = app.emit("agent:event", serde_json::json!({
                                "agent_id": agent.id,
                                "event": "StreamChunk",
                                "data": "\n\n⚠️ *Synthesis unavailable — showing individual specialist outputs:*\n\n",
                            }));

                            let mut fallback = String::new();
                            for (idx, (name, role, result)) in dispatch_results.iter().enumerate() {
                                let header = format!(
                                    "{}## {} ({})\n\n",
                                    if idx == 0 { "" } else { "\n---\n\n" },
                                    name, role,
                                );
                                fallback.push_str(&header);
                                let _ = app.emit("agent:event", serde_json::json!({
                                    "agent_id": agent.id,
                                    "event": "StreamChunk",
                                    "data": &header,
                                }));

                                match result {
                                    Ok((content, _, _)) => {
                                        fallback.push_str(content);
                                        // Stream in UTF-8-safe chunks.
                                        let mut remaining = content.as_str();
                                        while !remaining.is_empty() {
                                            let end = if remaining.len() <= 512 {
                                                remaining.len()
                                            } else {
                                                let mut i = 512;
                                                while i > 0 && !remaining.is_char_boundary(i) {
                                                    i -= 1;
                                                }
                                                if i == 0 { remaining.len().min(512) } else { i }
                                            };
                                            let (chunk, rest) = remaining.split_at(end);
                                            let _ = app.emit("agent:event", serde_json::json!({
                                                "agent_id": agent.id,
                                                "event": "StreamChunk",
                                                "data": chunk,
                                            }));
                                            remaining = rest;
                                        }
                                    }
                                    Err(e) => {
                                        let err_text = format!("*[Agent error: {}]*\n", e);
                                        fallback.push_str(&err_text);
                                        let _ = app.emit("agent:event", serde_json::json!({
                                            "agent_id": agent.id,
                                            "event": "StreamChunk",
                                            "data": &err_text,
                                        }));
                                    }
                                }
                                fallback.push_str("\n\n");
                            }
                            fallback
                        };

                        // Aggregate token counts across all phases.
                        let total_input = decomp_tokens_in + p2_input_tokens + synth_tokens_in;
                        let total_output = decomp_tokens_out + p2_output_tokens + synth_tokens_out;

                        tracing::info!(
                            total_input,
                            total_output,
                            decomp_tokens_in,
                            decomp_tokens_out,
                            p2_input_tokens,
                            p2_output_tokens,
                            synth_tokens_in,
                            synth_tokens_out,
                            specialists = succeeded,
                            "Three-phase team orchestration: complete"
                        );

                        // Clean up runner resources (not used for team path).
                        drop(runner);
                        drop(_event_tx_keepalive);
                        let _ = forward_task.await;

                        {
                            let mut active = state.active_chat_runs.write().await;
                            active.remove(&chat_id);
                        }

                        team_result = Some(clawdesk_agents::runner::AgentResponse {
                            content: final_content,
                            total_rounds: dispatch_results.len() + 2, // decomp + specialists + synthesis
                            tool_messages: vec![],
                            finish_reason: clawdesk_providers::FinishReason::Stop,
                            input_tokens: total_input,
                            output_tokens: total_output,
                            segments: vec![],
                            active_skills: vec![],
                            messaging_sends: vec![],
                        });
                    }
                }
            }
        }

        team_result
    };

    // ── Team path completed or fall-through to normal single-agent path ──

    let (agent_response, execution_err): (clawdesk_agents::runner::AgentResponse, Option<String>) =
        if let Some(resp) = team_response {
            (resp, None)
        } else {
            // ── Normal path: single-agent LLM call with full failover ──
            //
            // `run_with_failover()` uses the FailoverController DFA:
            //   Level 1: Retry same model with decorrelated-jitter backoff
            //   Level 2: Rotate to next auth profile via ProfileRotator
            //   Level 3: Fallback to next model in the failover chain
            //   Level 4: Thinking-level downgrade on context overflow
            let _llm_permit = state.llm_concurrency.acquire().await
                .map_err(|_| "LLM concurrency semaphore closed".to_string())?;
            // Cancel token already registered above (before team_response block).
            let run_result = runner
                .run_with_failover(history.clone(), system_prompt.clone())
                .await
                .map_err(|e| e.to_string());
            drop(_llm_permit);

            {
                let mut active = state.active_chat_runs.write().await;
                active.remove(&chat_id);
            }

            // Drop ALL broadcast senders so the forwarder sees `Closed` and exits.
            drop(runner);
            drop(_event_tx_keepalive);
            let _ = forward_task.await;

            match run_result {
                Ok(resp) => (resp, None),
                Err(e) => {
                    let msg = format!("Agent execution failed: {}", e);
                    let _ = app.emit(
                        "system:alert",
                        serde_json::json!({
                            "level": "error",
                            "title": "Agent execution failed",
                            "message": msg.clone(),
                        }),
                    );
                    let err_resp = clawdesk_agents::runner::AgentResponse {
                        content: msg.clone(),
                        total_rounds: 1,
                        tool_messages: vec![],
                        finish_reason: clawdesk_providers::FinishReason::Stop,
                        input_tokens: 0,
                        output_tokens: 0,
                        segments: vec![],
                        active_skills: vec![],
                        messaging_sends: vec![],
                    };
                    (err_resp, Some(msg))
                }
            }
        };
