//! Behavioral regression suite — Tier-1 "fixed tests" adapted from a
//! memory-eval framework: exercises the real store/writer/router pipeline
//! (no mocks) to lock in a handful of guarantees that must never silently
//! regress: sensitive content is refused, a superseded memory cannot be
//! resurrected by later writes, conflicts are flagged and clear once
//! resolved, near-duplicate merges keep the residual, and MUST-priority
//! memories are never dropped by either injection cap.
//!
//! Distinct from `tests/e2e.rs`: that file drives `store.save()` directly
//! and never exercises `MemoryWriter`'s delta-write path — several of the
//! scenarios here specifically need the real dedup/merge behavior.

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use memvault_core::evidence::{self, EvidenceKind};
    use memvault_core::extractor::{ExtractionRejectReason, Extractor, SourceRole};
    use memvault_core::models::*;
    use memvault_core::router::MemoryRouter;
    use memvault_core::storage::MemoryStore;
    use memvault_core::storage::sqlite::SqliteStore;
    use memvault_core::writer::{MemoryWriter, WriteOutcome};

    fn agent(id: &str) -> SourceAgent {
        SourceAgent {
            id: id.to_string(),
            agent_type: "general".to_string(),
            session_id: None,
        }
    }

    // --- Sensitive content is refused, at both guard points ---

    #[test]
    fn sensitive_content_rejected_at_extraction() {
        let text = "our deploy key is AKIAABCDEFGHIJKLMNOP, please keep it safe";
        let guarded = Extractor::extract_guarded(text, SourceRole::User);

        assert!(
            guarded.outcome.memories.is_empty(),
            "no memory must be produced from credential-bearing text"
        );
        assert!(
            matches!(
                guarded.rejection,
                Some(ExtractionRejectReason::SensitiveContent(_))
            ),
            "rejection reason must be content-based, not provenance-based: {:?}",
            guarded.rejection
        );
    }

    #[tokio::test]
    async fn sensitive_content_rejected_at_writer_save() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let mem = Memory::new(
            MemoryType::Fact,
            "db password=Sup3rSecret!42".to_string(),
            Priority::Reference,
            agent("test"),
        );
        let writer = MemoryWriter::new(store.clone(), None).with_enabled(true);

        let result = writer.save(mem, None, false).await;
        assert!(
            result.is_err(),
            "MemoryWriter must refuse to persist sensitive content"
        );

        let all = store.list(None, 10, 0).await.unwrap();
        assert!(all.is_empty(), "nothing should have been persisted");
    }

    // --- A superseded memory must not be resurrected by a later write ---

    #[tokio::test]
    async fn superseded_memory_not_merged_by_dedup() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let ag = agent("dedup-test");

        let old = Memory::new(
            MemoryType::Fact,
            "The project backend uses FastAPI and PostgreSQL for the database layer and API endpoints for the mobile client application.".to_string(),
            Priority::Reference,
            ag.clone(),
        );
        let old = store.save(old).await.unwrap();

        let replacement = Memory::new(
            MemoryType::Fact,
            "The project backend uses FastAPI and PostgreSQL for the database layer and API endpoints for the mobile client application, now deployed on AWS.".to_string(),
            Priority::Reference,
            ag.clone(),
        );
        let replacement = store.save(replacement).await.unwrap();

        store.supersede(&old.id, &replacement.id).await.unwrap();

        // Write near-identical content again. Before the fix, `check_duplicate`
        // pulled `old` back into its candidate pool via `store.list()` (which,
        // unlike `search()`, does not filter `superseded_by`) and merged into
        // it — the new content would silently vanish into a dead memory.
        let writer = MemoryWriter::new(store.clone(), None).with_enabled(true);
        let incoming = Memory::new(
            MemoryType::Fact,
            "The project backend uses FastAPI and PostgreSQL for the database layer and API endpoints for the mobile client application.".to_string(),
            Priority::Reference,
            ag,
        );
        let outcome = writer.save(incoming, None, false).await.unwrap();

        let resolved_id = match &outcome {
            WriteOutcome::Merged { memory, .. } => memory.id.clone(),
            WriteOutcome::Skipped { memory, .. } => memory.id.clone(),
            WriteOutcome::Inserted(memory) => memory.id.clone(),
        };
        assert_ne!(
            resolved_id, old.id,
            "must never resolve into the superseded memory: {:?}",
            outcome
        );

        let old_after = store.get(&old.id).await.unwrap();
        assert_eq!(
            old_after.superseded_by,
            Some(replacement.id.clone()),
            "the superseded memory itself must stay untouched"
        );
    }

    // --- Conflicting MUST facts are flagged, and clear once resolved ---

    #[tokio::test]
    async fn contradicting_musts_flagged_then_resolved_by_supersede() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let ag = agent("claude-desktop");

        let mut claim = Memory::new(
            MemoryType::Preference,
            "deploy window is Friday".to_string(),
            Priority::Must,
            ag.clone(),
        );
        claim.human_reviewed = true;
        let claim = store.save(claim).await.unwrap();

        let mut rebuttal = Memory::new(
            MemoryType::Preference,
            "deploy window is Monday".to_string(),
            Priority::Must,
            ag,
        );
        rebuttal.human_reviewed = true;
        let rebuttal = store.save(rebuttal).await.unwrap();

        evidence::add_evidence(
            store.as_ref(),
            &rebuttal.id,
            EvidenceKind::Contradicts,
            Some(&claim.id),
            None,
            0.9,
        )
        .await
        .unwrap();

        let router = MemoryRouter::new(store.clone());
        let before = router
            .session_start_layered("claude-desktop", Some("deploy window"), None)
            .await
            .unwrap();
        assert_eq!(
            before.conflicts.len(),
            1,
            "must flag the injected pair as conflicting"
        );

        // Resolve it: the rebuttal is superseded by the (now settled) claim.
        store.supersede(&rebuttal.id, &claim.id).await.unwrap();

        let after = router
            .session_start_layered("claude-desktop", Some("deploy window"), None)
            .await
            .unwrap();
        assert!(
            after.conflicts.is_empty(),
            "a resolved (superseded) rebuttal must stop being flagged: {:?}",
            after.conflicts
        );
    }

    // --- Near-duplicate merges via MemoryWriter keep the residual ---

    #[tokio::test]
    async fn near_duplicate_merge_keeps_residual_via_writer() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let ag = agent("writer-test");
        let writer = MemoryWriter::new(store.clone(), None).with_enabled(true);

        let first = Memory::new(
            MemoryType::Fact,
            "The project backend uses FastAPI and PostgreSQL for the database layer and API endpoints for the mobile client application.".to_string(),
            Priority::Reference,
            ag.clone(),
        );
        let outcome1 = writer.save(first, None, false).await.unwrap();
        let saved1 = match outcome1 {
            WriteOutcome::Inserted(m) => m,
            other => panic!(
                "first save into an empty namespace must insert, got {:?}",
                other
            ),
        };

        let second = Memory::new(
            MemoryType::Fact,
            "The project backend uses FastAPI and PostgreSQL for the database layer and API endpoints for the mobile client application. Redis is used for caching.".to_string(),
            Priority::Reference,
            ag,
        );
        let outcome2 = writer.save(second, None, false).await.unwrap();

        match outcome2 {
            WriteOutcome::Merged {
                memory,
                residual_added,
                ..
            } => {
                assert_eq!(memory.id, saved1.id, "must merge into the existing memory");
                assert!(
                    residual_added,
                    "the Redis sentence is genuinely new, not a rewording"
                );
                assert!(
                    memory.content.contains("Redis"),
                    "residual content must be retained, not discarded: {}",
                    memory.content
                );
            }
            other => panic!(
                "expected a Merge for near-duplicate content, got {:?}",
                other
            ),
        }

        let all = store.list(None, 10, 0).await.unwrap();
        assert_eq!(all.len(), 1, "a merge must not create a second row");
    }

    // --- MUST survives both injection caps ---

    #[tokio::test]
    async fn must_priority_survives_max_memories_cap() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let ag = agent("cap-test");

        let mut must_ids = Vec::new();
        for i in 0..5 {
            let m = Memory::new(
                MemoryType::Preference,
                format!("Mandatory rule number {i}: always follow this instruction."),
                Priority::Must,
                ag.clone(),
            );
            must_ids.push(store.save(m).await.unwrap().id);
        }

        let profile = AgentProfile {
            id: "max-memories-cap".to_string(),
            agent_type: "general".to_string(),
            description: String::new(),
            inject_rules: InjectRules {
                max_memories: 2,
                token_budget: 5000,
                priority_order: vec![Priority::Must, Priority::Reference],
                namespace_filter: vec!["global".to_string()],
                exclude_types: Vec::new(),
            },
            api_key: None,
            inject_channel: None,
        };
        let router = MemoryRouter::with_registry(store, vec![profile]);
        let injection = router
            .session_start("max-memories-cap", None, None)
            .await
            .unwrap();

        let injected_must_count = injection
            .results
            .iter()
            .filter(|r| r.memory.priority == Priority::Must)
            .count();
        assert_eq!(
            injected_must_count, 5,
            "all 5 MUST memories must survive a max_memories=2 cap, got {}",
            injected_must_count
        );
        for id in &must_ids {
            assert!(
                injection.skipped.iter().all(|s| &s.id != id),
                "MUST memory {} must never appear in skipped",
                id
            );
        }
    }

    #[tokio::test]
    async fn must_priority_survives_token_budget_cap() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let ag = agent("budget-cap-test");

        let mut must_ids = Vec::new();
        for i in 0..5 {
            let mut m = Memory::new(
                MemoryType::Preference,
                format!(
                    "MUST rule number {i}: always follow this mandatory instruction no matter what happens in the session."
                ),
                Priority::Must,
                ag.clone(),
            );
            m.instruction = Some(m.content.clone());
            must_ids.push(store.save(m).await.unwrap().id);
        }
        for i in 0..5 {
            let m = Memory::new(
                MemoryType::Fact,
                format!(
                    "Reference filler memory number {i} with enough text to consume token budget space as well."
                ),
                Priority::Reference,
                ag.clone(),
            );
            store.save(m).await.unwrap();
        }

        let profile = AgentProfile {
            id: "token-budget-cap".to_string(),
            agent_type: "general".to_string(),
            description: String::new(),
            inject_rules: InjectRules {
                max_memories: 20,
                token_budget: 40,
                priority_order: vec![Priority::Must, Priority::Reference],
                namespace_filter: vec!["global".to_string()],
                exclude_types: Vec::new(),
            },
            api_key: None,
            inject_channel: None,
        };
        let router = MemoryRouter::with_registry(store, vec![profile]);
        let injection = router
            .session_start("token-budget-cap", None, None)
            .await
            .unwrap();

        let injected_must_count = injection
            .results
            .iter()
            .filter(|r| r.memory.priority == Priority::Must)
            .count();
        assert_eq!(
            injected_must_count, 5,
            "all 5 MUST memories must survive a tight token budget, got {}",
            injected_must_count
        );
        for id in &must_ids {
            assert!(
                injection.skipped.iter().all(|s| &s.id != id),
                "MUST memory {} must never be cut by the token budget",
                id
            );
        }
    }
}
