#[cfg(test)]
mod tests {
    use memvault_core::intent;
    use memvault_core::models::*;
    use memvault_core::router::MemoryRouter;
    use memvault_core::storage::MemoryStore;
    use memvault_core::storage::sqlite::SqliteStore;
    use std::sync::Arc;

    async fn setup_store() -> Arc<SqliteStore> {
        let store = Arc::new(SqliteStore::in_memory().unwrap());

        let agent = SourceAgent {
            id: "claude-desktop".to_string(),
            agent_type: "coding-assistant".to_string(),
            session_id: Some("sess_001".to_string()),
        };

        let mut m1 = Memory::new(
            MemoryType::Preference,
            "用户偏好 Python".to_string(),
            Priority::Must,
            agent.clone(),
        );
        m1.instruction = Some("代码使用 Python，不用 Java".to_string());
        m1.tags = vec!["coding".to_string(), "python".to_string()];

        let mut m2 = Memory::new(
            MemoryType::Preference,
            "代码不加注释".to_string(),
            Priority::Must,
            agent.clone(),
        );
        m2.instruction = Some("代码不加注释，函数≤10行".to_string());
        m2.tags = vec!["coding".to_string(), "style".to_string()];

        let mut m3 = Memory::new(
            MemoryType::Fact,
            "项目使用 FastAPI + PostgreSQL".to_string(),
            Priority::Reference,
            agent.clone(),
        );
        m3.tags = vec!["coding".to_string(), "project".to_string()];

        let mut m4 = Memory::new(
            MemoryType::Preference,
            "写作风格简洁，不要过多修辞".to_string(),
            Priority::Reference,
            agent.clone(),
        );
        m4.tags = vec!["writing".to_string(), "style".to_string()];

        let mut m5 = Memory::new(
            MemoryType::Fact,
            "用户是高级 Rust 开发者".to_string(),
            Priority::Reference,
            agent,
        );
        m5.tags = vec!["coding".to_string(), "background".to_string()];

        store.save(m1).await.unwrap();
        store.save(m2).await.unwrap();
        store.save(m3).await.unwrap();
        store.save(m4).await.unwrap();
        store.save(m5).await.unwrap();

        store
    }

    // --- E2E: Save → Search → Get ---

    #[tokio::test]
    async fn e2e_save_search_get() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "test".to_string(),
            agent_type: "general".to_string(),
            session_id: None,
        };

        let mem = Memory::new(
            MemoryType::Preference,
            "prefer dark mode".to_string(),
            Priority::Must,
            agent,
        );
        let id = mem.id.clone();

        // Save
        let saved = store.save(mem).await.unwrap();
        assert_eq!(saved.id, id);

        // Search
        let results = store
            .search(SearchQuery::new("dark mode".to_string()))
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].memory.id, id);

        // Get
        let retrieved = store.get(&id).await.unwrap();
        assert_eq!(retrieved.content, "prefer dark mode");
        assert_eq!(retrieved.priority, Priority::Must);
    }

    // --- E2E: Save → Session Start → Formatted injection ---

    #[tokio::test]
    async fn e2e_save_session_start_inject() {
        let store = setup_store().await;
        let router = MemoryRouter::new(store);

        let results = router
            .session_start("claude-desktop", Some("帮我写一个 API"), None)
            .await
            .unwrap();

        // MUST memories should always be present
        let must_count = results
            .iter()
            .filter(|r| r.memory.priority == Priority::Must)
            .count();
        assert!(
            must_count >= 2,
            "Expected ≥2 MUST memories, got {}",
            must_count
        );

        // claude-desktop soft-penalizes "writing" tagged memories (not hard exclude)
        let writing_memories: Vec<&_> = results
            .iter()
            .filter(|r| r.memory.tags.contains(&"writing".to_string()))
            .collect();
        let coding_memories: Vec<&_> = results
            .iter()
            .filter(|r| {
                r.memory.tags.contains(&"coding".to_string()) && r.memory.priority != Priority::Must
            })
            .collect();
        // writing memories should have lower scores than coding memories
        if !writing_memories.is_empty() && !coding_memories.is_empty() {
            let max_writing_score = writing_memories
                .iter()
                .map(|r| r.score)
                .fold(0.0f64, f64::max);
            let min_coding_score = coding_memories
                .iter()
                .map(|r| r.score)
                .fold(f64::MAX, f64::min);
            assert!(
                max_writing_score < min_coding_score,
                "writing memories (max score {:.3}) should rank below coding memories (min score {:.3})",
                max_writing_score,
                min_coding_score
            );
        }

        // Format should contain [MUST] and [REF]
        let formatted = router.format_as_instructions(&results);
        assert!(formatted.contains("[MUST]"));
        assert!(formatted.contains("[MEMORY CONTEXT"));
    }

    // --- E2E: Agent filtering ---

    #[tokio::test]
    async fn e2e_different_agents_different_memories() {
        let store = setup_store().await;
        let router = MemoryRouter::new(store);

        // coding agent (excludes writing)
        let coding_results = router
            .session_start("claude-desktop", None, None)
            .await
            .unwrap();

        // default agent (no exclusions)
        let default_results = router.session_start("default", None, None).await.unwrap();

        let _coding_has_writing = coding_results
            .iter()
            .any(|r| r.memory.tags.contains(&"writing".to_string()));
        let default_has_writing = default_results
            .iter()
            .any(|r| r.memory.tags.contains(&"writing".to_string()));

        // With soft filtering, writing memories ARE present but with lower scores
        let coding_writing_score = coding_results
            .iter()
            .filter(|r| r.memory.tags.contains(&"writing".to_string()))
            .map(|r| r.score)
            .next();
        assert!(
            default_has_writing,
            "default agent should include writing memories"
        );

        // Writing memories in coding agent should be penalized (lower score)
        if let Some(ws) = coding_writing_score {
            let coding_avg = coding_results
                .iter()
                .filter(|r| {
                    !r.memory.tags.contains(&"writing".to_string())
                        && r.memory.priority != Priority::Must
                })
                .map(|r| r.score)
                .sum::<f64>()
                / coding_results.len().max(1) as f64;
            assert!(
                ws < coding_avg,
                "writing memory score ({:.3}) should be below average ({:.3})",
                ws,
                coding_avg
            );
        }
    }

    // --- E2E: MCP Resources ---

    #[tokio::test]
    async fn e2e_mcp_resources() {
        let store = setup_store().await;
        let router = MemoryRouter::new(store);

        // user-profile should only contain MUST
        let profile = router
            .get_mcp_resource_content("memory://user-profile")
            .await
            .unwrap();
        assert!(profile.contains("[MUST]"));
        assert!(
            !profile.contains("[REF]"),
            "user-profile should only have MUST entries"
        );

        // project-context should only contain REFERENCE
        let project = router
            .get_mcp_resource_content("memory://project-context")
            .await
            .unwrap();
        assert!(project.contains("[REF]"));
        assert!(
            !project.contains("[MUST]"),
            "project-context should only have REF entries"
        );

        // unknown resource should be empty
        let unknown = router
            .get_mcp_resource_content("memory://nonexistent")
            .await
            .unwrap();
        assert!(unknown.is_empty());
    }

    // --- E2E: Token Budget ---

    #[tokio::test]
    async fn e2e_token_budget_limits() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "test".to_string(),
            agent_type: "general".to_string(),
            session_id: None,
        };

        // Insert many memories to exceed token budget
        for i in 0..30 {
            let m = Memory::new(
                MemoryType::Fact,
                format!(
                    "This is a detailed memory entry number {} containing substantial content to consume token budget space",
                    i
                ),
                Priority::Reference,
                agent.clone(),
            );
            store.save(m).await.unwrap();
        }

        let router = MemoryRouter::new(store);
        let results = router.session_start("default", None, None).await.unwrap();

        // Should be capped by max_memories (8) AND token budget (1500)
        assert!(
            results.len() <= 8,
            "results ({}) should be ≤ 8",
            results.len()
        );

        // Verify the formatted output is within budget
        let formatted = router.format_as_instructions(&results);
        let estimated = MemoryRouter::estimate_tokens(&formatted);
        assert!(
            estimated <= 1600, // small margin for formatting overhead
            "formatted output ({} tokens) should be near 1500 budget",
            estimated
        );
    }

    // --- E2E: Review flow ---

    #[tokio::test]
    async fn e2e_review_approve_reject() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "test".to_string(),
            agent_type: "general".to_string(),
            session_id: None,
        };

        let m1 = Memory::new(
            MemoryType::Fact,
            "to approve".to_string(),
            Priority::Reference,
            agent.clone(),
        );
        let m2 = Memory::new(
            MemoryType::Fact,
            "to reject".to_string(),
            Priority::Reference,
            agent,
        );
        let id1 = m1.id.clone();
        let id2 = m2.id.clone();

        store.save(m1).await.unwrap();
        store.save(m2).await.unwrap();

        // Approve
        let mut mem = store.get(&id1).await.unwrap();
        assert!(!mem.human_reviewed);
        mem.human_reviewed = true;
        store.update(mem).await.unwrap();

        let approved = store.get(&id1).await.unwrap();
        assert!(approved.human_reviewed);

        // Reject (delete)
        store.delete(&id2).await.unwrap();
        assert!(store.get(&id2).await.is_err());
    }

    // --- E2E: Intent analysis integration ---

    #[tokio::test]
    async fn e2e_intent_analysis() {
        // coding intent
        let r = intent::analyze_intent("帮我写一个 Python 函数来处理数据库查询");
        assert_eq!(r.primary, intent::Intent::Coding);

        // writing intent
        let r = intent::analyze_intent("写一篇关于 AI 趋势的博客文章");
        assert_eq!(r.primary, intent::Intent::Writing);

        // general (no strong signal)
        let r = intent::analyze_intent("你好，今天怎么样");
        assert_eq!(r.primary, intent::Intent::General);
    }

    // --- E2E: YAML Agent Registry ---

    #[tokio::test]
    async fn e2e_yaml_agent_registry() {
        let store = setup_store().await;

        let yaml = r#"
agents:
  - id: custom-agent
    agent_type: research-assistant
    description: "Custom research agent"
    inject_rules:
      max_memories: 3
      token_budget: 500
      priority_order: ["MUST"]
      namespace_filter: ["global"]
      exclude_types: ["coding"]
"#;

        let config: AgentRegistryConfig = serde_yaml::from_str(yaml).unwrap();
        let router = MemoryRouter::with_registry(store, config.agents);

        let results = router
            .session_start("custom-agent", None, None)
            .await
            .unwrap();

        // max 3 memories
        assert!(
            results.len() <= 3,
            "custom agent should get ≤3 memories, got {}",
            results.len()
        );

        // MUST memories should still be included even though exclude_types has "coding"
        // (MUST always passes)
        let must_count = results
            .iter()
            .filter(|r| r.memory.priority == Priority::Must)
            .count();
        assert!(must_count > 0, "MUST memories should always be included");
    }

    // --- E2E: Multi-agent concurrent writes ---

    #[tokio::test]
    async fn e2e_multi_agent_shared_store() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());

        let agent_a = SourceAgent {
            id: "agent-a".to_string(),
            agent_type: "coding".to_string(),
            session_id: None,
        };
        let agent_b = SourceAgent {
            id: "agent-b".to_string(),
            agent_type: "writing".to_string(),
            session_id: None,
        };

        // Agent A writes
        let m1 = Memory::new(
            MemoryType::Preference,
            "from agent A".to_string(),
            Priority::Must,
            agent_a,
        );
        store.save(m1).await.unwrap();

        // Agent B writes
        let m2 = Memory::new(
            MemoryType::Fact,
            "from agent B".to_string(),
            Priority::Reference,
            agent_b,
        );
        store.save(m2).await.unwrap();

        // Both memories visible to any agent
        let all = store.list(None, 100, 0).await.unwrap();
        assert_eq!(all.len(), 2);

        let sources: Vec<&str> = all.iter().map(|m| m.source_agent.id.as_str()).collect();
        assert!(sources.contains(&"agent-a"));
        assert!(sources.contains(&"agent-b"));
    }

    // --- E2E: Confirm Read (passive tracking via session_start) ---

    #[tokio::test]
    async fn e2e_session_start_tracks_access_count() {
        let store = setup_store().await;
        let router = MemoryRouter::new(store.clone());

        // Capture IDs before session_start
        let all_before = store.list(None, 100, 0).await.unwrap();
        let before_count: u32 = all_before.iter().map(|m| m.access_count).sum();
        assert_eq!(
            before_count, 0,
            "all memories should start with access_count = 0"
        );

        // session_start should passively increment access_count
        let _results = router
            .session_start("claude-desktop", Some("帮我写代码"), None)
            .await
            .unwrap();

        // Verify access_count was incremented
        let all_after = store.list(None, 100, 0).await.unwrap();
        let after_count: u32 = all_after.iter().map(|m| m.access_count).sum();
        assert!(
            after_count > 0,
            "access_count should be > 0 after session_start"
        );

        // Must memories should have been read
        for m in &all_after {
            if m.priority == Priority::Must {
                assert!(
                    m.access_count >= 1,
                    "MUST memory {} should have access_count >= 1",
                    m.id
                );
            }
        }
    }

    // --- E2E: Confirm Read (explicit via confirm_read) ---

    #[tokio::test]
    async fn e2e_confirm_read_updates_access_and_timestamp() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "test".to_string(),
            agent_type: "test".to_string(),
            session_id: None,
        };

        let m1 = Memory::new(
            MemoryType::Fact,
            "memory one".to_string(),
            Priority::Reference,
            agent.clone(),
        );
        let m2 = Memory::new(
            MemoryType::Fact,
            "memory two".to_string(),
            Priority::Reference,
            agent,
        );
        let id1 = m1.id.clone();
        let id2 = m2.id.clone();

        store.save(m1).await.unwrap();
        store.save(m2).await.unwrap();

        let router = MemoryRouter::new(store.clone());

        // Confirm one memory
        router
            .confirm_read(std::slice::from_ref(&id1))
            .await
            .unwrap();

        let m1_after = store.get(&id1).await.unwrap();
        let m2_after = store.get(&id2).await.unwrap();

        assert_eq!(
            m1_after.access_count, 1,
            "confirmed memory should have access_count = 1"
        );
        assert!(
            m1_after.last_read_at.is_some(),
            "confirmed memory should have last_read_at set"
        );
        assert_eq!(
            m2_after.access_count, 0,
            "unconfirmed memory should still have access_count = 0"
        );

        // Confirm multiple memories
        router
            .confirm_read(&[id1.clone(), id2.clone()])
            .await
            .unwrap();

        let m1_final = store.get(&id1).await.unwrap();
        let m2_final = store.get(&id2).await.unwrap();

        assert_eq!(
            m1_final.access_count, 2,
            "double-confirmed memory should have access_count = 2"
        );
        assert_eq!(
            m2_final.access_count, 1,
            "once-confirmed memory should have access_count = 1"
        );
    }

    // --- E2E: Store direct record_access ---

    #[tokio::test]
    async fn e2e_store_record_access_batch() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "test".to_string(),
            agent_type: "test".to_string(),
            session_id: None,
        };

        let m1 = Memory::new(
            MemoryType::Fact,
            "a".to_string(),
            Priority::Reference,
            agent.clone(),
        );
        let m2 = Memory::new(
            MemoryType::Fact,
            "b".to_string(),
            Priority::Reference,
            agent.clone(),
        );
        let m3 = Memory::new(
            MemoryType::Fact,
            "c".to_string(),
            Priority::Reference,
            agent,
        );
        let id1 = m1.id.clone();
        let id2 = m2.id.clone();
        let id3 = m3.id.clone();

        store.save(m1).await.unwrap();
        store.save(m2).await.unwrap();
        store.save(m3).await.unwrap();

        // Batch record_access via store directly
        store
            .record_access(&[id1.clone(), id2.clone()])
            .await
            .unwrap();

        assert_eq!(store.get(&id1).await.unwrap().access_count, 1);
        assert_eq!(store.get(&id2).await.unwrap().access_count, 1);
        assert_eq!(store.get(&id3).await.unwrap().access_count, 0);

        // Empty list is a no-op
        store.record_access(&[]).await.unwrap();

        assert_eq!(
            store.get(&id1).await.unwrap().access_count,
            1,
            "should not change after empty batch"
        );
    }

    // --- E2E: List with namespace filter ---

    #[tokio::test]
    async fn e2e_list_namespace_filter() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "test".to_string(),
            agent_type: "test".to_string(),
            session_id: None,
        };

        let mut m1 = Memory::new(
            MemoryType::Fact,
            "global mem".into(),
            Priority::Reference,
            agent.clone(),
        );
        m1.namespace = "global".into();
        let mut m2 = Memory::new(
            MemoryType::Fact,
            "project mem".into(),
            Priority::Reference,
            agent.clone(),
        );
        m2.namespace = "project:my-app".into();

        store.save(m1).await.unwrap();
        store.save(m2).await.unwrap();

        let global = store.list(Some("global"), 100, 0).await.unwrap();
        assert_eq!(global.len(), 1);
        assert_eq!(global[0].content, "global mem");

        let all = store.list(None, 100, 0).await.unwrap();
        assert_eq!(all.len(), 2);
    }

    // --- E2E: Search with type and priority filters ---

    #[tokio::test]
    async fn e2e_search_filters() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "test".to_string(),
            agent_type: "test".into(),
            session_id: None,
        };
        let m1 = Memory::new(
            MemoryType::Preference,
            "pref Python".into(),
            Priority::Must,
            agent.clone(),
        );
        let m2 = Memory::new(
            MemoryType::Fact,
            "fact about Rust".into(),
            Priority::Reference,
            agent,
        );
        store.save(m1).await.unwrap();
        store.save(m2).await.unwrap();

        // Filter by priority
        let must_results = store
            .search(SearchQuery {
                priority_filter: Some(Priority::Must),
                top_k: 10,
                ..SearchQuery::new("Python".into())
            })
            .await
            .unwrap();
        assert_eq!(must_results.len(), 1);
        assert_eq!(must_results[0].memory.memory_type, MemoryType::Preference);
    }

    // --- E2E: Update memory preserves fields ---

    #[tokio::test]
    async fn e2e_update_memory() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "test".to_string(),
            agent_type: "test".into(),
            session_id: None,
        };
        let mem = Memory::new(
            MemoryType::Fact,
            "original".into(),
            Priority::Reference,
            agent,
        );
        let id = mem.id.clone();
        store.save(mem).await.unwrap();

        let mut retrieved = store.get(&id).await.unwrap();
        retrieved.content = "updated".to_string();
        retrieved.instruction = Some("new instruction".to_string());
        store.update(retrieved).await.unwrap();

        let updated = store.get(&id).await.unwrap();
        assert_eq!(updated.content, "updated");
        assert_eq!(updated.instruction, Some("new instruction".to_string()));
    }

    // --- E2E: Cross-namespace fallback ---

    #[tokio::test]
    async fn e2e_cross_namespace_fallback() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let agent = SourceAgent {
            id: "coding-agent".to_string(),
            agent_type: "coding-assistant".into(),
            session_id: None,
        };

        let mut local = Memory::new(
            MemoryType::Fact,
            "project memory".into(),
            Priority::Reference,
            agent.clone(),
        );
        local.namespace = "project:test".to_string();
        local.tags = vec!["coding".into()];

        let mut global = Memory::new(
            MemoryType::Fact,
            "global preference".into(),
            Priority::Reference,
            agent.clone(),
        );
        global.namespace = "global".to_string();
        global.tags = vec!["general".into()];

        store.save(local).await.unwrap();
        store.save(global).await.unwrap();

        let registry = vec![AgentProfile {
            id: "coding-agent".into(),
            agent_type: "coding-assistant".into(),
            description: "".into(),
            inject_rules: InjectRules {
                max_memories: 10,
                namespace_filter: vec!["project:test".into()],
                ..InjectRules::default()
            },
            api_key: None,
        }];
        let router = MemoryRouter::with_registry(store, registry);
        let results = router
            .session_start("coding-agent", None, Some("test"))
            .await
            .unwrap();

        // Should include project memory and potentially global (cross-namespace fallback)
        let has_project = results.iter().any(|r| r.memory.content == "project memory");
        assert!(has_project, "project memory should be found");
    }

    // --- E2E: Agent profile matching ---

    #[tokio::test]
    async fn e2e_agent_profile_fallback() {
        let store = setup_store().await;
        let router = MemoryRouter::new(store);

        // Unknown agent gets "default" profile
        let profile = router.get_agent_profile("totally-unknown-agent");
        assert_eq!(profile.id, "default");
        assert_eq!(profile.agent_type, "general-assistant");

        // Partial match via type prefix
        let profile2 = router.get_agent_profile_with_client_info("my-coding-tool", Some("Claude"));
        assert_eq!(profile2.agent_type, "coding-assistant");
    }
}
