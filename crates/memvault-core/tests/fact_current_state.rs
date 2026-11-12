use std::sync::{Arc, Barrier};

use chrono::{Duration, Utc};
use memvault_core::facts::{
    FactAssertion, FactCardinality, FactManagementState, FactWriteContext, FactWriteMode,
    FactWriteOutcome,
};
use memvault_core::io::{Exporter, Importer};
use memvault_core::models::{Memory, MemoryLayer, MemoryType, Priority, SearchQuery, SourceAgent};
use memvault_core::storage::MemoryStore;
use memvault_core::storage::sqlite::SqliteStore;
use memvault_core::writer::MemoryWriter;

fn fact(key: &str, value: &str, order: u64) -> Memory {
    let mut memory = Memory::new(
        MemoryType::Fact,
        format!("{key} is {value}"),
        Priority::Reference,
        SourceAgent {
            id: "producer".into(),
            agent_type: "test".into(),
            session_id: None,
        },
    );
    memory.namespace = "project:Example".into();
    memory.fact_assertion = Some(FactAssertion {
        key: key.into(),
        value: value.into(),
        source_namespace: "document".into(),
        source_order: order,
        cardinality: FactCardinality::Single,
    });
    memory
}

fn producer() -> FactWriteContext {
    FactWriteContext::verified("producer").unwrap()
}

async fn resolve(store: Arc<SqliteStore>, memory: Memory) -> memvault_core::facts::FactWriteResult {
    MemoryWriter::new(store, None)
        .save_fact(memory, None, false, FactWriteMode::Resolve, &producer())
        .await
        .unwrap()
}

#[tokio::test]
async fn new_versions_retire_old_and_keep_history() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let first = resolve(store.clone(), fact("server.timeout", "5", 1)).await;
    assert_eq!(first.outcome, FactWriteOutcome::Inserted);
    let old = first.memory.unwrap();
    assert!(!old.human_reviewed);
    assert!(old.identity_verified);
    assert_eq!(
        old.fact_management.as_ref().unwrap().state(),
        FactManagementState::Active
    );
    let next = resolve(store.clone(), fact("server.timeout", "9", 2)).await;
    assert_eq!(next.outcome, FactWriteOutcome::Superseded);
    assert_eq!(next.superseded_id.as_deref(), Some(old.id.as_str()));
    let new = next.memory.unwrap();
    let retired = store.get(&old.id).await.unwrap();
    assert_eq!(retired.superseded_by.as_deref(), Some(new.id.as_str()));
    assert_eq!(retired.layer, MemoryLayer::L0);
    assert_eq!(retired.fact_assertion, old.fact_assertion);
    assert!(!new.human_reviewed);
    let history = store.get_history_snapshot(&old.id).await.unwrap().unwrap();
    assert_eq!(history.fact_management, old.fact_management);
    assert!(history.superseded_by.is_none());
    let current = store
        .search(SearchQuery::new("timeout".into()))
        .await
        .unwrap();
    assert_eq!(current.results.len(), 1);
    assert_eq!(current.results[0].memory.id, new.id);
}

#[tokio::test]
async fn retries_order_conflicts_and_same_value_watermark() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let first = resolve(store.clone(), fact("entity.owner", "Alice", 5))
        .await
        .memory
        .unwrap();
    let retry = resolve(store.clone(), fact("entity.owner", "Alice", 5)).await;
    assert_eq!(retry.outcome, FactWriteOutcome::Idempotent);
    assert_eq!(retry.memory.unwrap().id, first.id);
    assert_eq!(
        resolve(store.clone(), fact("entity.owner", "Bob", 5))
            .await
            .outcome,
        FactWriteOutcome::Conflict
    );
    assert_eq!(
        resolve(store.clone(), fact("entity.owner", "Bob", 4))
            .await
            .outcome,
        FactWriteOutcome::Stale
    );
    let advance = resolve(store.clone(), fact("entity.owner", "Alice", 9)).await;
    assert_eq!(advance.outcome, FactWriteOutcome::Superseded);
    assert_eq!(
        advance
            .memory
            .unwrap()
            .fact_management
            .unwrap()
            .source_order(),
        9
    );
    assert_eq!(
        resolve(store.clone(), fact("entity.owner", "Bob", 8))
            .await
            .outcome,
        FactWriteOutcome::Stale
    );
    assert_eq!(
        resolve(store.clone(), fact("entity.owner", "Bob", 10))
            .await
            .outcome,
        FactWriteOutcome::Superseded
    );
    assert_eq!(store.list(None, 100, 0).await.unwrap().len(), 3);
}

#[tokio::test]
async fn private_management_proof_does_not_require_public_identity_recording() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let writer = MemoryWriter::new(store.clone(), None);
    let proof = FactWriteContext::verified_with_identity_recording("producer", false).unwrap();
    let mut claimed = fact("entity.owner", "Alice", 1);
    claimed.identity_verified = true; // Client/public metadata cannot override policy.
    let first = writer
        .save_fact(claimed, None, false, FactWriteMode::Resolve, &proof)
        .await
        .unwrap();
    assert_eq!(first.outcome, FactWriteOutcome::Inserted);
    let first = first.memory.unwrap();
    assert!(!first.identity_verified);
    assert_eq!(
        first.fact_management.as_ref().unwrap().state(),
        FactManagementState::Active
    );
    let second = writer
        .save_fact(
            fact("entity.owner", "Bob", 2),
            None,
            false,
            FactWriteMode::Resolve,
            &proof,
        )
        .await
        .unwrap();
    assert_eq!(second.outcome, FactWriteOutcome::Superseded);
    let second = second.memory.unwrap();
    assert!(!second.identity_verified);
    assert_eq!(
        second.fact_management.as_ref().unwrap().producer_id(),
        "producer"
    );
    let before = serde_json::to_value(&second).unwrap();
    let retry = writer
        .save_fact(
            fact("entity.owner", "Bob", 2),
            None,
            false,
            FactWriteMode::Resolve,
            &producer(),
        )
        .await
        .unwrap();
    assert_eq!(retry.outcome, FactWriteOutcome::Idempotent);
    assert!(!retry.memory.unwrap().identity_verified);
    let mut unverified = fact("entity.owner", "Mallory", 3);
    unverified.identity_verified = true;
    let refusal = writer
        .save_fact(
            unverified,
            None,
            false,
            FactWriteMode::Resolve,
            &FactWriteContext::default(),
        )
        .await
        .unwrap();
    assert_eq!(refusal.outcome, FactWriteOutcome::ReviewRequired);
    assert!(refusal.memory.is_none());
    assert_eq!(
        serde_json::to_value(store.get(&second.id).await.unwrap()).unwrap(),
        before
    );
    let recorded = writer
        .save_fact(
            fact("entity.owner", "Carol", 3),
            None,
            false,
            FactWriteMode::Resolve,
            &producer(),
        )
        .await
        .unwrap();
    assert_eq!(recorded.outcome, FactWriteOutcome::Superseded);
    assert!(recorded.memory.unwrap().identity_verified);
    assert!(!store.get(&first.id).await.unwrap().identity_verified);
    assert!(!store.get(&second.id).await.unwrap().identity_verified);
    assert!(
        !store
            .get_history_snapshot(&second.id)
            .await
            .unwrap()
            .unwrap()
            .identity_verified
    );
}

#[tokio::test]
async fn keys_qualifiers_and_source_scopes_are_independent() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    for key in [
        "server.timeout|zone=a",
        "server.timeout|zone=b",
        "server.retry|zone=a",
    ] {
        assert_eq!(
            resolve(store.clone(), fact(key, "5", 1)).await.outcome,
            FactWriteOutcome::Inserted
        );
    }
    let mut other_scope = fact("server.timeout|zone=a", "9", 1);
    other_scope
        .fact_assertion
        .as_mut()
        .unwrap()
        .source_namespace = "another-document".into();
    assert_eq!(
        resolve(store.clone(), other_scope).await.outcome,
        FactWriteOutcome::Inserted
    );
    assert_eq!(store.list(None, 100, 0).await.unwrap().len(), 4);
}

#[tokio::test]
async fn expired_head_still_orders_and_decay_keeps_guard() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let mut old = fact("server.timeout", "5", 5);
    old.expires_at = Some(Utc::now() - Duration::seconds(1));
    let first = resolve(store.clone(), old).await.memory.unwrap();
    memvault_core::decay::DecayManager::new(store.clone(), Default::default())
        .run_decay()
        .await
        .unwrap();
    assert_eq!(
        store.get(&first.id).await.unwrap().namespace,
        "project:example"
    );
    assert_eq!(
        resolve(store.clone(), fact("server.timeout", "4", 4))
            .await
            .outcome,
        FactWriteOutcome::Stale
    );
    assert_eq!(
        resolve(store.clone(), fact("server.timeout", "9", 6))
            .await
            .outcome,
        FactWriteOutcome::Superseded
    );
}

#[tokio::test]
async fn force_and_preserve_are_unmanaged_and_multiple_heads_are_ambiguous() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let old = resolve(store.clone(), fact("server.timeout", "5", 5))
        .await
        .memory
        .unwrap();
    let writer = MemoryWriter::new(store.clone(), None);
    let mut forged = fact("server.timeout", "9", 9);
    forged.fact_management = old.fact_management.clone();
    let forced = writer
        .save_fact(forged, None, true, FactWriteMode::Resolve, &producer())
        .await
        .unwrap();
    assert_eq!(forced.outcome, FactWriteOutcome::Inserted);
    assert!(forced.memory.unwrap().fact_management.is_none());
    assert!(store.get(&old.id).await.unwrap().superseded_by.is_none());
    assert_eq!(
        resolve(store.clone(), fact("server.timeout", "10", 10))
            .await
            .outcome,
        FactWriteOutcome::Ambiguity
    );
    let plain = writer
        .save(fact("other.timeout", "5", 1), None, false)
        .await
        .unwrap();
    assert!(plain.memory().fact_management.is_none());
    assert_eq!(
        resolve(store.clone(), fact("other.timeout", "9", 2))
            .await
            .outcome,
        FactWriteOutcome::ReviewRequired
    );
}

#[tokio::test]
async fn reviews_and_semantic_edits_suspend_and_clear_assertion() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    for (key, review) in [("edited.timeout", false), ("reviewed.timeout", true)] {
        let mut old = resolve(store.clone(), fact(key, "5", 1))
            .await
            .memory
            .unwrap();
        if review {
            old.human_reviewed = true;
        } else {
            old.content = "Human correction".into();
        }
        let updated = store.update(old).await.unwrap();
        assert!(updated.fact_assertion.is_none());
        let management = updated.fact_management.unwrap();
        assert_eq!(management.state(), FactManagementState::Suspended);
        assert_eq!(management.key(), key);
        assert_eq!(
            resolve(store.clone(), fact(key, "9", 2)).await.outcome,
            FactWriteOutcome::ReviewRequired
        );
    }
}

#[tokio::test]
async fn unverified_and_other_producer_refuse_without_mutating() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let writer = MemoryWriter::new(store.clone(), None);
    let mut input = fact("unverified.timeout", "5", 1);
    input.identity_verified = true;
    assert_eq!(
        writer
            .save_fact(
                input,
                None,
                false,
                FactWriteMode::Resolve,
                &FactWriteContext::default()
            )
            .await
            .unwrap()
            .outcome,
        FactWriteOutcome::ReviewRequired
    );
    assert!(store.list(None, 100, 0).await.unwrap().is_empty());
    let first = resolve(store.clone(), fact("server.timeout", "5", 1))
        .await
        .memory
        .unwrap();
    let mut incoming = fact("server.timeout", "9", 2);
    incoming.source_agent.id = "other-producer".into();
    let result = writer
        .save_fact(
            incoming,
            None,
            false,
            FactWriteMode::Resolve,
            &FactWriteContext::verified("other-producer").unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(result.outcome, FactWriteOutcome::Conflict);
    assert!(store.get(&first.id).await.unwrap().superseded_by.is_none());
}

#[tokio::test]
async fn metadata_validation_and_public_import_cannot_forge_management() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    for value in [
        "",
        "api_key=sk-dummycredentialfortesting",
        &"x".repeat(8193),
    ] {
        assert!(store.save(fact("server.timeout", value, 1)).await.is_err());
    }
    let managed = resolve(store.clone(), fact("server.timeout", "5", 1))
        .await
        .memory
        .unwrap();
    let mut imported = managed.clone();
    imported.id = "mem_imported_fact".into();
    let json = serde_json::to_string(&vec![imported]).unwrap();
    Importer::new(store.clone())
        .import_json(&json)
        .await
        .unwrap();
    assert!(
        store
            .get("mem_imported_fact")
            .await
            .unwrap()
            .fact_management
            .is_none()
    );
    let codec_store = Arc::new(SqliteStore::in_memory().unwrap());
    codec_store
        .save(fact("literal.timeout", "5", 1))
        .await
        .unwrap();
    let markdown = Exporter::new(codec_store)
        .export_markdown(None)
        .await
        .unwrap();
    assert!(markdown[0].1.contains("fact_assertion:"));
    let parsed = Importer::parse_markdown(&markdown[0].1).unwrap();
    assert_eq!(
        parsed.fact_assertion,
        fact("literal.timeout", "5", 1).fact_assertion
    );
}

#[tokio::test]
async fn deletion_and_forget_leave_no_ghost_ordering_head() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let first = resolve(store.clone(), fact("server.timeout", "5", 99))
        .await
        .memory
        .unwrap();
    store.delete(&first.id).await.unwrap();
    assert_eq!(
        resolve(store.clone(), fact("server.timeout", "9", 1))
            .await
            .outcome,
        FactWriteOutcome::Inserted
    );
    store.purge(&first.id).await.unwrap();
    assert!(
        store
            .get_history_snapshot(&first.id)
            .await
            .unwrap()
            .is_none()
    );
    let head = store.list(None, 100, 0).await.unwrap().pop().unwrap();
    store.purge(&head.id).await.unwrap();
    assert_eq!(
        resolve(store.clone(), fact("server.timeout", "10", 0))
            .await
            .outcome,
        FactWriteOutcome::Inserted
    );
}

#[test]
fn concurrent_first_inserts_and_max_ordinal_converge_after_restart() {
    let dir = std::env::temp_dir().join(format!("memvault_fact_race_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("facts.db");
    let store = Arc::new(SqliteStore::new(&path).unwrap());
    let barrier = Arc::new(Barrier::new(8));
    let handles: Vec<_> = (1..=8)
        .map(|order| {
            let store = store.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                barrier.wait();
                runtime.block_on(resolve(
                    store,
                    fact("server.timeout", &order.to_string(), order),
                ))
            })
        })
        .collect();
    for handle in handles {
        let result = handle.join().unwrap();
        assert!(matches!(
            result.outcome,
            FactWriteOutcome::Inserted | FactWriteOutcome::Superseded | FactWriteOutcome::Stale
        ));
    }
    drop(store);
    let store = Arc::new(SqliteStore::new(&path).unwrap());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let memories = store.list(None, 100, 0).await.unwrap();
        let heads: Vec<_> = memories
            .iter()
            .filter(|m| m.superseded_by.is_none())
            .collect();
        assert_eq!(heads.len(), 1);
        assert_eq!(heads[0].fact_assertion.as_ref().unwrap().source_order, 8);
        assert_eq!(
            resolve(store.clone(), fact("server.timeout", "0", 0))
                .await
                .outcome,
            FactWriteOutcome::Stale
        );
    });
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn legacy_json_missing_fields_and_assertion_embedded_save_roundtrip() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let mut legacy = fact("legacy.timeout", "5", 1);
    legacy.fact_assertion = None;
    let mut json = serde_json::to_value(&legacy).unwrap();
    json.as_object_mut().unwrap().remove("fact_assertion");
    json.as_object_mut().unwrap().remove("fact_management");
    let decoded: Memory = serde_json::from_value(json).unwrap();
    assert!(decoded.fact_assertion.is_none());
    assert!(decoded.fact_management.is_none());
    let saved = store.save(decoded).await.unwrap();
    assert!(store.get(&saved.id).await.unwrap().fact_assertion.is_none());
    let asserted = store
        .save_with_embedding(fact("literal.timeout", "9", u64::MAX), vec![1.0, 0.0])
        .await
        .unwrap();
    let loaded = store.get(&asserted.id).await.unwrap();
    assert_eq!(loaded.fact_assertion, asserted.fact_assertion);
    assert!(loaded.fact_management.is_none());
    assert!(store.get_embedding(&loaded.id).await.unwrap().is_some());
}

#[tokio::test]
async fn malformed_and_sensitive_assertion_metadata_are_rejected_everywhere() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let writer = MemoryWriter::new(store.clone(), None);
    for field in ["key", "value", "source_namespace"] {
        for bad in ["", "\n", "AKIAABCDEFGHIJKLMNOP"] {
            let mut memory = fact("server.timeout", "5", 1);
            memory.content = "safe ordinary body".into();
            let assertion = memory.fact_assertion.as_mut().unwrap();
            match field {
                "key" => assertion.key = bad.into(),
                "value" => assertion.value = bad.into(),
                _ => assertion.source_namespace = bad.into(),
            }
            assert!(writer.save(memory.clone(), None, false).await.is_err());
            assert!(
                store
                    .save_with_embedding(memory.clone(), vec![1.0])
                    .await
                    .is_err()
            );
            assert!(store.resolve_fact(memory, &producer()).await.is_err());
        }
    }
    let mut split_secret = fact("api_key", "dummycredential", 1);
    split_secret.content = "safe body".into();
    assert!(store.save(split_secret).await.is_err());
    let good = fact("server.timeout", "5", 1);
    for malformed in [
        serde_json::json!({"key":"x"}),
        serde_json::json!({"key":"x","value":"5","source_namespace":"doc","source_order":-1,"cardinality":"single"}),
        serde_json::json!({"key":"x","value":"5","source_namespace":"doc","source_order":1,"cardinality":"not-a-cardinality"}),
    ] {
        let mut json = serde_json::to_value(&good).unwrap();
        json["fact_assertion"] = malformed;
        assert!(
            Importer::new(store.clone())
                .import_json(&serde_json::to_string(&vec![json]).unwrap())
                .await
                .is_err()
        );
    }
    assert!(store.list(None, 100, 0).await.unwrap().is_empty());
}

#[tokio::test]
async fn multi_event_unknown_preserve_but_never_resolve_or_fuzzy_merge() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let writer = MemoryWriter::new(store.clone(), None);
    for cardinality in [
        FactCardinality::Multi,
        FactCardinality::Event,
        FactCardinality::Unknown,
    ] {
        let mut memory = fact("entity.member", "Alice", 1);
        memory.fact_assertion.as_mut().unwrap().cardinality = cardinality;
        assert_eq!(
            writer
                .save_fact(
                    memory.clone(),
                    None,
                    false,
                    FactWriteMode::Resolve,
                    &producer()
                )
                .await
                .unwrap()
                .outcome,
            FactWriteOutcome::ReviewRequired
        );
        for _ in 0..2 {
            memory.id = format!("mem_{}", uuid::Uuid::new_v4().as_simple());
            assert_eq!(
                writer
                    .save_fact(
                        memory.clone(),
                        None,
                        false,
                        FactWriteMode::Preserve,
                        &producer()
                    )
                    .await
                    .unwrap()
                    .outcome,
                FactWriteOutcome::Inserted
            );
        }
    }
    assert_eq!(store.list(None, 100, 0).await.unwrap().len(), 6);
    assert!(
        memvault_core::dedup::Deduplicator::new(store.clone(), None)
            .scan(None)
            .await
            .unwrap()
            .duplicates
            .is_empty()
    );
}

#[tokio::test]
async fn protected_input_authorities_are_never_managed() {
    use memvault_core::models::{SkillMeta, Visibility};
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    for mutation in 0..10 {
        let mut memory = fact("protected.timeout", "5", 1);
        match mutation {
            0 => memory.human_reviewed = true,
            1 => memory.ai_generated = false,
            2 => memory.priority = Priority::Must,
            3 => memory.visibility = Visibility::Shared,
            4 => memory.instruction = Some("a directive".into()),
            5 => {
                memory.skill_meta = Some(SkillMeta {
                    trigger: None,
                    steps: vec!["step".into()],
                    verification: None,
                    version: 1,
                })
            }
            6 => memory.proposes_supersede_of = Some("mem_target".into()),
            7 => memory.memory_type = MemoryType::Entity,
            8 => memory.layer = MemoryLayer::L3,
            _ => memory.tags = vec!["decision".into()],
        }
        assert_eq!(
            resolve(store.clone(), memory).await.outcome,
            FactWriteOutcome::ReviewRequired
        );
    }
    assert!(store.list(None, 100, 0).await.unwrap().is_empty());
}

#[tokio::test]
async fn authority_and_namespace_edits_suspend_original_stream_identity() {
    use memvault_core::models::Visibility;
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    for mutation in 0..7 {
        let key = format!("protected.timeout|case={mutation}");
        let mut head = resolve(store.clone(), fact(&key, "5", 1))
            .await
            .memory
            .unwrap();
        match mutation {
            0 => head.priority = Priority::Must,
            1 => head.ai_generated = false,
            2 => head.identity_verified = false,
            3 => head.visibility = Visibility::Shared,
            4 => head.namespace = "project:HumanMoved".into(),
            5 => head.instruction = Some("human directive".into()),
            _ => head.source_agent.id = "human".into(),
        }
        let updated = store.update(head).await.unwrap();
        assert!(updated.fact_assertion.is_none());
        let management = updated.fact_management.unwrap();
        assert_eq!(management.namespace(), "project:example");
        assert_eq!(management.state(), FactManagementState::Suspended);
        assert_eq!(
            resolve(store.clone(), fact(&key, "9", 2)).await.outcome,
            FactWriteOutcome::ReviewRequired
        );
    }
}

#[tokio::test]
async fn pending_proposal_suspends_target_and_review_keeps_guard() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let head = resolve(store.clone(), fact("server.timeout", "5", 1))
        .await
        .memory
        .unwrap();
    let proposal = memvault_core::proposal::propose_text_edit(
        &*store,
        &head.id,
        "5",
        "9",
        memvault_core::proposal::EditField::Content,
        SourceAgent {
            id: "human-reviewer".into(),
            agent_type: "test".into(),
            session_id: None,
        },
    )
    .await
    .unwrap();
    let frozen = store.get(&head.id).await.unwrap();
    assert_eq!(frozen.content, head.content);
    assert!(frozen.fact_assertion.is_none());
    assert_eq!(
        frozen.fact_management.unwrap().state(),
        FactManagementState::Suspended
    );
    assert_eq!(
        resolve(store.clone(), fact("server.timeout", "10", 10))
            .await
            .outcome,
        FactWriteOutcome::ReviewRequired
    );
    memvault_core::proposal::approve(&*store, &proposal.proposal_id)
        .await
        .unwrap();
    assert_eq!(
        resolve(store.clone(), fact("server.timeout", "11", 11))
            .await
            .outcome,
        FactWriteOutcome::ReviewRequired
    );
}

struct CountingEmbedder {
    calls: std::sync::atomic::AtomicUsize,
    fail: bool,
}

#[async_trait::async_trait]
impl memvault_core::embedding::EmbeddingProvider for CountingEmbedder {
    async fn embed(&self, texts: &[String]) -> memvault_core::error::Result<Vec<Vec<f32>>> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.fail {
            return Err(memvault_core::error::MemVaultError::Storage(
                "injected embedder failure".into(),
            ));
        }
        Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
    }
    fn dimension(&self) -> usize {
        2
    }
}

#[tokio::test]
async fn embeddings_are_post_commit_best_effort_and_old_vectors_survive() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let failing = Arc::new(CountingEmbedder {
        calls: Default::default(),
        fail: true,
    });
    let writer = MemoryWriter::new(store.clone(), Some(failing.clone()));
    let first = writer
        .save_fact(
            fact("server.timeout", "5", 1),
            None,
            false,
            FactWriteMode::Resolve,
            &producer(),
        )
        .await
        .unwrap()
        .memory
        .unwrap();
    assert_eq!(failing.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(store.get_embedding(&first.id).await.unwrap().is_none());
    assert!(
        store
            .list_without_embedding(100)
            .await
            .unwrap()
            .iter()
            .any(|m| m.id == first.id)
    );
    store
        .set_embedding(&first.id, vec![1.0, 0.0])
        .await
        .unwrap();
    let next = writer
        .save_fact(
            fact("server.timeout", "9", 2),
            Some(vec![0.0, 1.0]),
            false,
            FactWriteMode::Resolve,
            &producer(),
        )
        .await
        .unwrap()
        .memory
        .unwrap();
    assert_eq!(failing.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(store.get_embedding(&first.id).await.unwrap().is_some());
    assert!(store.get_embedding(&next.id).await.unwrap().is_some());
    for memory in [
        fact("server.timeout", "9", 2),
        fact("server.timeout", "10", 1),
        fact("server.timeout", "10", 2),
    ] {
        writer
            .save_fact(memory, None, false, FactWriteMode::Resolve, &producer())
            .await
            .unwrap();
    }
    assert_eq!(failing.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn fuzzy_vector_and_promotion_paths_do_not_absorb_asserted_records() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let asserted = store
        .save_with_embedding(fact("server.timeout", "5", 1), vec![1.0, 0.0])
        .await
        .unwrap();
    let mut legacy = fact("legacy", "5", 1);
    legacy.fact_assertion = None;
    legacy.content = "unrelated vocabulary".into();
    store
        .save_with_embedding(legacy, vec![0.0, 1.0])
        .await
        .unwrap();
    let embedder = Arc::new(CountingEmbedder {
        calls: Default::default(),
        fail: false,
    });
    let dedup = memvault_core::dedup::Deduplicator::new(store.clone(), Some(embedder));
    assert!(
        dedup
            .check_duplicate("new unrelated phrasing", Some("project:example"))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        dedup
            .check_duplicate(&asserted.content, Some("project:example"))
            .await
            .unwrap()
            .is_none()
    );
    let writer = MemoryWriter::new(store.clone(), None);
    let mut plain = fact("plain", "5", 1);
    plain.fact_assertion = None;
    plain.content = asserted.content.clone();
    assert!(matches!(
        writer.save(plain, None, false).await.unwrap(),
        memvault_core::writer::WriteOutcome::Inserted(_)
    ));
    for order in 2..6 {
        let mut structured = fact(&format!("server.timeout|zone={order}"), "5", order);
        structured.tags = vec!["same-tag".into()];
        structured.access_count = 10;
        store.save(structured).await.unwrap();
    }
    let before = store.list(None, 100, 0).await.unwrap();
    memvault_core::promote::Promoter::new(store.clone(), Default::default())
        .run()
        .await
        .unwrap();
    for memory in before.iter().filter(|m| m.fact_assertion.is_some()) {
        let after = store.get(&memory.id).await.unwrap();
        assert_eq!(after.fact_assertion, memory.fact_assertion);
        assert_eq!(after.layer, memory.layer);
        assert!(after.superseded_by.is_none());
    }
}

#[tokio::test]
async fn markdown_resolution_is_unsupported_without_files_or_provider_calls() {
    let dir = std::env::temp_dir().join(format!("memvault_fact_markdown_{}", uuid::Uuid::new_v4()));
    let store = Arc::new(
        memvault_core::storage::markdown::MarkdownStore::open(&dir)
            .await
            .unwrap(),
    );
    let before: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let bytes = std::fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect();
    let embedder = Arc::new(CountingEmbedder {
        calls: Default::default(),
        fail: false,
    });
    let writer = MemoryWriter::new(store.clone(), Some(embedder.clone()));
    let unsupported = writer
        .save_fact(
            fact("server.timeout", "5", 1),
            None,
            false,
            FactWriteMode::Resolve,
            &producer(),
        )
        .await
        .unwrap();
    assert_eq!(unsupported.outcome, FactWriteOutcome::Unsupported);
    assert!(unsupported.memory.is_none());
    assert!(!unsupported.is_written());
    assert_eq!(embedder.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    for (path, bytes) in before {
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
    assert!(store.list(None, 100, 0).await.unwrap().is_empty());
    let literal = writer
        .save_fact(
            fact("server.timeout", "5", 1),
            None,
            false,
            FactWriteMode::Preserve,
            &producer(),
        )
        .await
        .unwrap()
        .memory
        .unwrap();
    assert!(literal.fact_management.is_none());
    assert_eq!(
        store.get(&literal.id).await.unwrap().fact_assertion,
        literal.fact_assertion
    );
    drop(writer);
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn markdown_export_import_cannot_transfer_managed_authority() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let head = resolve(store.clone(), fact("entity.owner", "Alice", 1))
        .await
        .memory
        .unwrap();
    let markdown = Exporter::new(store)
        .export_markdown(None)
        .await
        .unwrap()
        .pop()
        .unwrap()
        .1;
    let codec = Importer::parse_markdown(&markdown).unwrap();
    assert_eq!(codec.fact_management, head.fact_management);
    let other = Arc::new(SqliteStore::in_memory().unwrap());
    Importer::new(other.clone())
        .import_markdown_content(&markdown)
        .await
        .unwrap();
    let imported = other.get(&head.id).await.unwrap();
    assert_eq!(imported.fact_assertion, head.fact_assertion);
    assert!(imported.fact_management.is_none());
    assert_eq!(
        resolve(other, fact("entity.owner", "Bob", 2)).await.outcome,
        FactWriteOutcome::ReviewRequired
    );
}

#[tokio::test]
async fn injected_failures_roll_back_new_rows_retirement_history_and_both_fts_indexes() {
    let dir = std::env::temp_dir().join(format!("memvault_fact_rollback_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("facts.db");
    let store = Arc::new(SqliteStore::new(&path).unwrap());
    let first = resolve(store.clone(), fact("server.timeout", "5", 1))
        .await
        .memory
        .unwrap();
    let conn = rusqlite::Connection::open(&path).unwrap();
    let counts = |connection: &rusqlite::Connection| -> Vec<i64> {
        [
            "memories",
            "memory_history",
            "memories_fts",
            "memory_history_fts",
        ]
        .iter()
        .map(|table| {
            connection
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap()
        })
        .collect()
    };
    let before = counts(&conn);
    for trigger in [
        "CREATE TRIGGER fail_fact AFTER INSERT ON memories WHEN NEW.id='mem_candidate' BEGIN SELECT RAISE(ABORT, 'after-insert'); END",
        "CREATE TRIGGER fail_fact AFTER INSERT ON memory_history BEGIN SELECT RAISE(ABORT, 'after-history'); END",
        "CREATE TRIGGER fail_fact AFTER UPDATE OF superseded_by ON memories WHEN NEW.superseded_by IS NOT NULL BEGIN SELECT RAISE(ABORT, 'after-retirement'); END",
    ] {
        conn.execute_batch(trigger).unwrap();
        let mut incoming = fact("server.timeout", "9", 2);
        incoming.id = "mem_candidate".into();
        assert!(store.resolve_fact(incoming, &producer()).await.is_err());
        assert_eq!(counts(&conn), before);
        assert!(store.get("mem_candidate").await.is_err());
        assert!(store.get(&first.id).await.unwrap().superseded_by.is_none());
        assert!(
            store
                .get_history_snapshot(&first.id)
                .await
                .unwrap()
                .is_none()
        );
        conn.execute_batch("DROP TRIGGER fail_fact").unwrap();
    }
    // Failure on the *first* insert also rolls back without an ordering ghost.
    conn.execute_batch("CREATE TRIGGER fail_fact AFTER INSERT ON memories WHEN NEW.id='mem_candidate' BEGIN SELECT RAISE(ABORT, 'first-insert'); END").unwrap();
    let mut other = fact("first.timeout", "5", 9);
    other.id = "mem_candidate".into();
    assert!(store.resolve_fact(other, &producer()).await.is_err());
    assert_eq!(counts(&conn), before);
    conn.execute_batch("DROP TRIGGER fail_fact").unwrap();
    assert_eq!(
        resolve(store.clone(), fact("first.timeout", "5", 1))
            .await
            .outcome,
        FactWriteOutcome::Inserted
    );
    drop(conn);
    drop(store);
    let store = Arc::new(SqliteStore::new(&path).unwrap());
    assert_eq!(
        resolve(store.clone(), fact("server.timeout", "9", 2))
            .await
            .outcome,
        FactWriteOutcome::Superseded
    );
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn concurrent_equal_ordinal_first_inserts_are_idempotent_across_pools() {
    let dir =
        std::env::temp_dir().join(format!("memvault_fact_equal_race_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("facts.db");
    let stores = [
        Arc::new(SqliteStore::new(&path).unwrap()),
        Arc::new(SqliteStore::new(&path).unwrap()),
    ];
    let barrier = Arc::new(Barrier::new(8));
    let handles: Vec<_> = (0..8)
        .map(|index| {
            let store = stores[index % 2].clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                barrier.wait();
                runtime.block_on(resolve(store, fact("server.timeout", "5", 1)))
            })
        })
        .collect();
    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert_eq!(
        results
            .iter()
            .filter(|result| result.outcome == FactWriteOutcome::Inserted)
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|result| result.outcome == FactWriteOutcome::Idempotent)
            .count(),
        7
    );
    let ids: std::collections::HashSet<_> = results
        .iter()
        .map(|result| result.memory.as_ref().unwrap().id.clone())
        .collect();
    assert_eq!(ids.len(), 1);
    drop(stores);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn malformed_updates_are_not_silently_cleared_and_expiry_retries_roundtrip() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let mut original = fact("server.timeout", "5", 1);
    original.expires_at = Some("2099-01-01T00:00:00.123456789Z".parse().unwrap());
    let saved = resolve(store.clone(), original.clone())
        .await
        .memory
        .unwrap();
    assert_eq!(
        resolve(store.clone(), original.clone()).await.outcome,
        FactWriteOutcome::Idempotent
    );
    let mut malformed = saved.clone();
    malformed.fact_assertion.as_mut().unwrap().value.clear();
    assert!(store.update(malformed).await.is_err());
    let loaded = store.get(&saved.id).await.unwrap();
    assert_eq!(
        loaded.fact_management.unwrap().state(),
        FactManagementState::Active
    );
    assert!(loaded.fact_assertion.is_some());
    let mut different_body = original;
    different_body.content = "equal value but different semantic body".into();
    assert_eq!(
        resolve(store.clone(), different_body).await.outcome,
        FactWriteOutcome::Conflict
    );
    let higher = resolve(store.clone(), fact("server.timeout", "9", 2))
        .await
        .memory
        .unwrap();
    let historical = store
        .get_as_of(&saved.id, saved.created_at)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(historical.memory.fact_assertion, saved.fact_assertion);
    assert!(historical.memory.superseded_by.is_none());
    assert!(
        store
            .get_as_of(&higher.id, saved.created_at)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn sqlite_migrations_upgrade_version_28_and_reconcile_partial_version_zero() {
    let dir =
        std::env::temp_dir().join(format!("memvault_fact_migration_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    for version in [28, 0] {
        let path = dir.join(format!("facts-{version}.db"));
        let store = SqliteStore::new(&path).unwrap();
        let mut legacy = fact("legacy", "5", 1);
        legacy.fact_assertion = None;
        let id = store.save(legacy).await.unwrap().id;
        drop(store);
        let conn = rusqlite::Connection::open(&path).unwrap();
        let checksum: String = conn
            .query_row(
                "SELECT checksum FROM schema_migrations WHERE version=28",
                [],
                |row| row.get(0),
            )
            .unwrap();
        conn.execute_batch("DROP INDEX idx_memories_fact_group; ALTER TABLE memories DROP COLUMN fact_management; ALTER TABLE memories DROP COLUMN fact_group_namespace; ALTER TABLE memories DROP COLUMN fact_source_namespace; ALTER TABLE memories DROP COLUMN fact_key;").unwrap();
        if version == 28 {
            conn.execute_batch("ALTER TABLE memories DROP COLUMN fact_assertion")
                .unwrap();
        }
        conn.execute_batch(&format!(
            "DELETE FROM schema_migrations WHERE version>28; PRAGMA user_version={version}"
        ))
        .unwrap();
        drop(conn);
        let store = Arc::new(SqliteStore::new(&path).unwrap());
        assert_eq!(store.schema_fingerprint().unwrap().0, 34);
        assert!(store.get(&id).await.unwrap().fact_assertion.is_none());
        assert_eq!(
            resolve(store.clone(), fact("server.timeout", "5", 1))
                .await
                .outcome,
            FactWriteOutcome::Inserted
        );
        let conn = rusqlite::Connection::open(&path).unwrap();
        let after: String = conn
            .query_row(
                "SELECT checksum FROM schema_migrations WHERE version=28",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(after, checksum);
        drop(conn);
        drop(store);
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn quarantine_assertion_edits_revoke_review_and_clear_stale_metadata() {
    use memvault_core::storage::markdown::{MarkdownStore, MarkdownTrustMode};
    let dir =
        std::env::temp_dir().join(format!("memvault_fact_quarantine_{}", uuid::Uuid::new_v4()));
    let store = Arc::new(MarkdownStore::open(&dir).await.unwrap());
    let mut memory = fact("server.timeout", "5", 1);
    memory.human_reviewed = true;
    memory.identity_verified = true;
    let saved = store.save(memory).await.unwrap();
    let path = dir.join(format!("{}.md", saved.id));
    let content = std::fs::read_to_string(&path).unwrap();
    let edited = content.replace("\"value\":\"5\"", "\"value\":\"9\"");
    assert_ne!(content, edited);
    std::fs::write(&path, edited).unwrap();
    store
        .reindex_with_trust_mode(MarkdownTrustMode::Quarantine)
        .await
        .unwrap();
    let loaded = store.get(&saved.id).await.unwrap();
    assert!(!loaded.human_reviewed);
    assert!(!loaded.identity_verified);
    assert!(loaded.fact_assertion.is_none());
    assert!(
        !std::fs::read_to_string(path)
            .unwrap()
            .contains("fact_assertion:")
    );
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn trusted_history_restore_keeps_binding_but_never_reactivates_management() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    for deleted in [false, true] {
        let key = format!("server.timeout|deleted={deleted}");
        let original = resolve(store.clone(), fact(&key, "5", 9))
            .await
            .memory
            .unwrap();
        if deleted {
            store.delete(&original.id).await.unwrap();
        } else {
            let mut reviewed = original.clone();
            reviewed.human_reviewed = true;
            store.update(reviewed).await.unwrap();
        }
        let checkpoint = store
            .list_checkpoints(Some(&original.id), 1)
            .await
            .unwrap()
            .pop()
            .unwrap();
        let restored = store
            .restore_checkpoint(checkpoint.history_id)
            .await
            .unwrap();
        let mark = restored.fact_management.as_ref().unwrap();
        assert_eq!(mark.producer_id(), "producer");
        assert_eq!(mark.key(), key);
        assert_eq!(mark.source_order(), 9);
        assert_eq!(mark.state(), FactManagementState::Suspended);
        assert!(restored.fact_assertion.is_none());
        let persisted = store.get(&original.id).await.unwrap();
        assert_eq!(persisted.fact_management, restored.fact_management);
        assert!(persisted.fact_assertion.is_none());
        assert_eq!(
            resolve(store.clone(), fact(&key, "9", 10)).await.outcome,
            FactWriteOutcome::ReviewRequired
        );
    }
}

#[tokio::test]
async fn checkpoint_restore_on_reused_id_keeps_current_guard_and_atomic_indexes() {
    let dir = std::env::temp_dir().join(format!("memvault_fact_restore_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("facts.db");
    let store = Arc::new(SqliteStore::new(&path).unwrap());
    let conn = rusqlite::Connection::open(&path).unwrap();
    let counts = || -> Vec<i64> {
        [
            "memories",
            "memory_history",
            "memories_fts",
            "memory_history_fts",
        ]
        .iter()
        .map(|table| {
            conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
        })
        .collect()
    };
    for historical_managed in [false, true] {
        let mut historical = fact("historical.timeout", "5", 1);
        historical.namespace = "project:historical".into();
        historical.source_agent.id = "historical-producer".into();
        historical.content = "historical restore vocabulary".into();
        historical.expires_at = Some("2099-01-01T00:00:00.123456Z".parse().unwrap());
        if historical_managed {
            historical = store
                .resolve_fact(
                    historical,
                    &FactWriteContext::verified("historical-producer").unwrap(),
                )
                .await
                .unwrap()
                .memory
                .unwrap();
        } else {
            historical.fact_assertion = None;
            historical = store.save(historical).await.unwrap();
        }
        store.delete(&historical.id).await.unwrap();
        let checkpoint = store
            .list_checkpoints(Some(&historical.id), 1)
            .await
            .unwrap()
            .pop()
            .unwrap();
        let key = format!("current.timeout|historical_managed={historical_managed}");
        let mut replacement = fact(&key, "99", 99);
        replacement.id = historical.id.clone();
        replacement.namespace = "project:current".into();
        replacement
            .fact_assertion
            .as_mut()
            .unwrap()
            .source_namespace = "current-document".into();
        let current = resolve(store.clone(), replacement).await.memory.unwrap();
        let before = counts();
        // Even a failure after the full row UPDATE rolls its pre-update history
        // and historical FTS back; no separate guard/index repair may escape.
        conn.execute_batch("CREATE TRIGGER fail_restore AFTER UPDATE ON memories BEGIN SELECT RAISE(ABORT, 'restore-update'); END").unwrap();
        assert!(
            store
                .restore_checkpoint(checkpoint.history_id)
                .await
                .is_err()
        );
        assert_eq!(counts(), before);
        assert_eq!(
            store.get(&current.id).await.unwrap().fact_management,
            current.fact_management
        );
        conn.execute_batch("DROP TRIGGER fail_restore").unwrap();

        let restored = store
            .restore_checkpoint(checkpoint.history_id)
            .await
            .unwrap();
        let mark = restored.fact_management.as_ref().unwrap();
        assert_eq!(mark.state(), FactManagementState::Suspended);
        assert_eq!(mark.producer_id(), "producer");
        assert_eq!(mark.namespace(), "project:current");
        assert_eq!(mark.source_namespace(), "current-document");
        assert_eq!(mark.key(), key);
        assert_eq!(mark.source_order(), 99);
        assert!(restored.fact_assertion.is_none());
        assert_eq!(restored.content, historical.content);
        assert_eq!(restored.source_agent.id, historical.source_agent.id);
        assert_eq!(
            restored.source_agent.agent_type,
            historical.source_agent.agent_type
        );
        assert_eq!(
            restored.source_agent.session_id,
            historical.source_agent.session_id
        );
        assert_eq!(restored.namespace, historical.namespace);
        assert_eq!(restored.expires_at, historical.expires_at);
        let persisted = store.get(&current.id).await.unwrap();
        assert_eq!(persisted.fact_management, restored.fact_management);
        assert!(persisted.fact_assertion.is_none());
        let index: (String, String, String) = conn.query_row(
            "SELECT fact_group_namespace, fact_source_namespace, fact_key FROM memories WHERE id=?1",
            [&current.id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).unwrap();
        assert_eq!(
            index,
            (
                "project:current".into(),
                "current-document".into(),
                key.clone()
            )
        );
        assert_eq!(
            counts(),
            vec![before[0], before[1] + 1, before[2], before[3] + 1]
        );
        let snapshot = store
            .get_history_snapshot(&current.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(snapshot.fact_management, current.fact_management);
        assert_eq!(snapshot.fact_assertion, current.fact_assertion);
        let history_fts_count: i64 = conn.query_row(
            "SELECT count(*) FROM memory_history_fts WHERE history_id=(SELECT MAX(history_id) FROM memory_history WHERE memory_id=?1)",
            [&current.id], |row| row.get(0),
        ).unwrap();
        assert_eq!(history_fts_count, 1);
        assert!(
            store
                .search(SearchQuery::new("restore vocabulary".into()))
                .await
                .unwrap()
                .results
                .iter()
                .any(|hit| hit.memory.id == current.id)
        );
        for order in [0, 100, u64::MAX] {
            let mut next = fact(&key, "new", order);
            next.namespace = "project:current".into();
            next.fact_assertion.as_mut().unwrap().source_namespace = "current-document".into();
            assert_eq!(
                resolve(store.clone(), next).await.outcome,
                FactWriteOutcome::ReviewRequired
            );
        }
        assert_eq!(
            counts(),
            vec![before[0], before[1] + 1, before[2], before[3] + 1]
        );
    }
    drop(conn);
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn checkpoint_restore_cannot_give_current_unmanaged_row_historical_private_binding() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let historical = resolve(store.clone(), fact("historical.timeout", "5", 99))
        .await
        .memory
        .unwrap();
    store.delete(&historical.id).await.unwrap();
    let checkpoint = store
        .list_checkpoints(Some(&historical.id), 1)
        .await
        .unwrap()
        .pop()
        .unwrap();
    let mut unmanaged = fact("current.timeout", "9", 1);
    unmanaged.id = historical.id.clone();
    unmanaged.fact_assertion = None;
    let current = store.save(unmanaged).await.unwrap();
    assert!(current.fact_management.is_none());
    let restored = store
        .restore_checkpoint(checkpoint.history_id)
        .await
        .unwrap();
    assert!(restored.fact_management.is_none());
    assert_eq!(restored.fact_assertion, historical.fact_assertion);
    let persisted = store.get(&current.id).await.unwrap();
    assert!(persisted.fact_management.is_none());
    assert_eq!(persisted.fact_assertion, historical.fact_assertion);
    assert_eq!(
        resolve(store.clone(), fact("historical.timeout", "new", 100))
            .await
            .outcome,
        FactWriteOutcome::ReviewRequired
    );
}

#[tokio::test]
async fn fact_replacement_has_one_exact_transition_and_never_backdates_successor() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let old = resolve(store.clone(), fact("server.timeout", "5", 1))
        .await
        .memory
        .unwrap();
    let mut incoming = fact("server.timeout", "9", 2);
    // Deterministically simulate a caller constructing the successor long before
    // its transaction: both facts would otherwise be current at this instant.
    let constructed_at = old.created_at;
    incoming.created_at = constructed_at;
    incoming.updated_at = constructed_at;
    incoming.occurred_at = Some(constructed_at - Duration::days(1));
    incoming.expires_at = Some("2099-01-01T00:00:00.123456Z".parse().unwrap());
    let occurred_at = incoming.occurred_at;
    let expires_at = incoming.expires_at;
    let new = resolve(store.clone(), incoming).await.memory.unwrap();
    assert!(new.created_at > constructed_at);
    assert_eq!(new.updated_at, new.created_at);
    assert_eq!(new.occurred_at, occurred_at);
    assert_eq!(new.expires_at, expires_at);
    assert_eq!(new.fact_assertion.as_ref().unwrap().source_order, 2);
    assert_eq!(store.get(&new.id).await.unwrap().created_at, new.created_at);
    assert_eq!(store.get(&old.id).await.unwrap().updated_at, new.created_at);
    let checkpoint = store
        .list_checkpoints(Some(&old.id), 1)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(
        checkpoint
            .changed_at
            .parse::<chrono::DateTime<Utc>>()
            .unwrap(),
        new.created_at
    );
    let old_at_construction = store
        .get_as_of(&old.id, constructed_at)
        .await
        .unwrap()
        .unwrap();
    match old_at_construction.source {
        memvault_core::models::AsOfSource::History { replaced_at, .. } => {
            assert_eq!(replaced_at, new.created_at)
        }
        _ => panic!("predecessor must come from its pre-transition snapshot"),
    }
    assert!(old_at_construction.memory.superseded_by.is_none());
    assert!(
        store
            .get_as_of(&new.id, constructed_at)
            .await
            .unwrap()
            .is_none()
    );
    for (instant, expected_id) in [
        (constructed_at, &old.id),
        (new.created_at - Duration::nanoseconds(1), &old.id),
        (new.created_at, &new.id),
        (new.created_at + Duration::nanoseconds(1), &new.id),
    ] {
        let mut query = SearchQuery::new("timeout".into());
        query.as_of = Some(instant);
        let hits = store.search(query).await.unwrap().results;
        assert_eq!(hits.len(), 1, "one current fact at {instant}");
        assert_eq!(&hits[0].memory.id, expected_id);
    }
    let retired = store
        .get_as_of(&old.id, new.created_at)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        retired.memory.superseded_by.as_deref(),
        Some(new.id.as_str())
    );
    assert_eq!(retired.memory.layer, MemoryLayer::L0);
}

#[tokio::test]
async fn initial_fact_insert_stamps_system_time_but_preserves_plain_save_clock() {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let mut input = fact("server.timeout", "5", 1);
    let constructed_at = Utc::now() - Duration::hours(1);
    input.created_at = constructed_at;
    input.updated_at = constructed_at;
    input.occurred_at = Some(constructed_at - Duration::days(1));
    input.expires_at = Some("2099-01-01T00:00:00.123456Z".parse().unwrap());
    let new = resolve(store.clone(), input.clone()).await.memory.unwrap();
    assert!(new.created_at > constructed_at);
    assert_eq!(new.updated_at, new.created_at);
    assert_eq!(new.occurred_at, input.occurred_at);
    assert_eq!(new.expires_at, input.expires_at);
    assert!(
        store
            .get_as_of(&new.id, constructed_at)
            .await
            .unwrap()
            .is_none()
    );
    input.id = "mem_plain_clock".into();
    input.fact_assertion = None;
    let plain = store.save(input).await.unwrap();
    assert_eq!(plain.created_at, constructed_at);
    assert_eq!(
        store.get(&plain.id).await.unwrap().created_at,
        constructed_at
    );
}

#[test]
fn fact_successor_constructed_while_write_locked_starts_after_lock_release() {
    let dir =
        std::env::temp_dir().join(format!("memvault_fact_clock_lock_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("facts.db");
    let store = Arc::new(SqliteStore::new(&path).unwrap());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let old = runtime
        .block_on(resolve(store.clone(), fact("server.timeout", "5", 1)))
        .memory
        .unwrap();
    let mut conn = rusqlite::Connection::open(&path).unwrap();
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let incoming = fact("server.timeout", "9", 2);
    let constructed_at = incoming.created_at;
    let worker_store = store.clone();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        ready_tx.send(()).unwrap();
        let result = runtime.block_on(resolve(worker_store, incoming));
        done_tx.send(()).unwrap();
        result
    });
    ready_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert_eq!(
        done_rx.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    );
    let released_at = Utc::now();
    tx.commit().unwrap();
    let new = worker.join().unwrap().memory.unwrap();
    assert!(new.created_at >= released_at);
    assert!(new.created_at > constructed_at);
    let boundary: String = conn
        .query_row(
            "SELECT changed_at FROM memory_history WHERE memory_id=?1",
            [&old.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        boundary.parse::<chrono::DateTime<Utc>>().unwrap(),
        new.created_at
    );
    runtime.block_on(async {
        let mut query = SearchQuery::new("timeout".into());
        query.as_of = Some(constructed_at);
        let hits = store.search(query).await.unwrap().results;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].memory.id, old.id);
        assert!(
            store
                .get_as_of(&new.id, constructed_at)
                .await
                .unwrap()
                .is_none()
        );
    });
    drop(conn);
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}
