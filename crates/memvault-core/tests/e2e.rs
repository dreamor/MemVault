#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use memvault_core::models::*;
    use memvault_core::router::MemoryRouter;
    use memvault_core::storage::MemoryStore;
    use memvault_core::storage::sqlite::SqliteStore;
    use memvault_core::intent;

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
        let results = store.search(SearchQuery::new("dark mode".to_string())).await.unwrap();
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
        let must_count = results.iter().filter(|r| r.memory.priority == Priority::Must).count();
        assert!(must_count >= 2, "Expected ≥2 MUST memories, got {}", must_count);

        // claude-desktop excludes "writing" type
        let has_writing = results.iter().any(|r| r.memory.tags.contains(&"writing".to_string()));
        assert!(!has_writing, "coding-assistant should NOT receive writing memories");

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
        let default_results = router
            .session_start("default", None, None)
            .await
            .unwrap();

        let coding_has_writing = coding_results.iter()
            .any(|r| r.memory.tags.contains(&"writing".to_string()));
        let default_has_writing = default_results.iter()
            .any(|r| r.memory.tags.contains(&"writing".to_string()));

        assert!(!coding_has_writing, "coding agent should exclude writing memories");
        assert!(default_has_writing, "default agent should include writing memories");

        // default agent should have more memories than coding agent
        assert!(
            default_results.len() >= coding_results.len(),
            "default ({}) should have ≥ coding ({}) memories",
            default_results.len(),
            coding_results.len()
        );
    }

    // --- E2E: MCP Resources ---

    #[tokio::test]
    async fn e2e_mcp_resources() {
        let store = setup_store().await;
        let router = MemoryRouter::new(store);

        // user-profile should only contain MUST
        let profile = router.get_mcp_resource_content("memory://user-profile").await.unwrap();
        assert!(profile.contains("[MUST]"));
        assert!(!profile.contains("[REF]"), "user-profile should only have MUST entries");

        // project-context should only contain REFERENCE
        let project = router.get_mcp_resource_content("memory://project-context").await.unwrap();
        assert!(project.contains("[REF]"));
        assert!(!project.contains("[MUST]"), "project-context should only have REF entries");

        // unknown resource should be empty
        let unknown = router.get_mcp_resource_content("memory://nonexistent").await.unwrap();
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
                format!("This is a detailed memory entry number {} containing substantial content to consume token budget space", i),
                Priority::Reference,
                agent.clone(),
            );
            store.save(m).await.unwrap();
        }

        let router = MemoryRouter::new(store);
        let results = router.session_start("default", None, None).await.unwrap();

        // Should be capped by max_memories (8) AND token budget (1500)
        assert!(results.len() <= 8, "results ({}) should be ≤ 8", results.len());

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

        let m1 = Memory::new(MemoryType::Fact, "to approve".to_string(), Priority::Reference, agent.clone());
        let m2 = Memory::new(MemoryType::Fact, "to reject".to_string(), Priority::Reference, agent);
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

        let results = router.session_start("custom-agent", None, None).await.unwrap();

        // max 3 memories
        assert!(results.len() <= 3, "custom agent should get ≤3 memories, got {}", results.len());

        // MUST memories should still be included even though exclude_types has "coding"
        // (MUST always passes)
        let must_count = results.iter().filter(|r| r.memory.priority == Priority::Must).count();
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
        let m1 = Memory::new(MemoryType::Preference, "from agent A".to_string(), Priority::Must, agent_a);
        store.save(m1).await.unwrap();

        // Agent B writes
        let m2 = Memory::new(MemoryType::Fact, "from agent B".to_string(), Priority::Reference, agent_b);
        store.save(m2).await.unwrap();

        // Both memories visible to any agent
        let all = store.list(None, 100, 0).await.unwrap();
        assert_eq!(all.len(), 2);

        let sources: Vec<&str> = all.iter().map(|m| m.source_agent.id.as_str()).collect();
        assert!(sources.contains(&"agent-a"));
        assert!(sources.contains(&"agent-b"));
    }
}
