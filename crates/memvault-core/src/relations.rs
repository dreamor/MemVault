//! Semantic memory persistence (C2): turn extracted relation triples into
//! entity memories + `memory_relations` rows.
//!
//! Resolution rules:
//! - The subject is always resolved to an Entity memory — an existing one
//!   whose content matches (entity normalization), or a freshly created one
//!   (which enters the review queue like any ai-generated memory).
//! - The object resolves to an existing Entity memory when one matches;
//!   otherwise it is stored as free text (`object_text`) instead of
//!   spamming the store with entity stubs for every mentioned noun.
//! - Identical triples are never stored twice.

use chrono::Utc;
use tracing::debug;

use crate::dedup::Deduplicator;
use crate::error::Result;
use crate::llm_extractor::ExtractedRelation;
use crate::models::{Memory, MemoryRelation, MemoryType, Priority, SourceAgent};
use crate::storage::MemoryStore;

/// Jaccard threshold for matching an entity name to an existing Entity
/// memory. High bar: entities are names, and a false merge points the
/// relation at the wrong node.
const ENTITY_MATCH_THRESHOLD: f32 = 0.85;

/// Collect the one-hop neighborhood of a memory: outgoing relations (as
/// subject) plus incoming relations (as object), de-duplicated by
/// relation id. This is the C5 retrieval-expansion primitive.
pub async fn collect_relations(
    store: &(impl MemoryStore + ?Sized),
    memory_id: &str,
) -> Vec<MemoryRelation> {
    let mut out = store
        .relations_of_subject(memory_id)
        .await
        .unwrap_or_default();
    let inbound = store
        .relations_of_object(memory_id)
        .await
        .unwrap_or_default();
    for rel in inbound {
        if !out.iter().any(|r| r.relation_id == rel.relation_id) {
            out.push(rel);
        }
    }
    out
}

/// Human-readable one-line description of one relation, resolving the
/// object form (memory id rendered as `memory:<id>`, free text as-is).
pub fn relation_line(subject_content: &str, rel: &MemoryRelation) -> String {
    let object = match (&rel.object_id, &rel.object_text) {
        (Some(id), _) => format!("memory:{id}"),
        (None, Some(text)) => text.clone(),
        (None, None) => "?".to_string(),
    };
    format!("{} —{}→ {}", subject_content, rel.predicate, object)
}

/// Persist extracted triples; returns how many NEW relations were stored.
pub async fn store_relation_triples(
    store: &(impl MemoryStore + ?Sized),
    triples: &[ExtractedRelation],
    namespace: &str,
    source_agent: &SourceAgent,
    source_memory_id: Option<&str>,
) -> Result<usize> {
    let mut stored = 0usize;
    for triple in triples {
        if triple.subject.trim().is_empty()
            || triple.predicate.trim().is_empty()
            || triple.object.trim().is_empty()
        {
            continue;
        }

        let subject_id = resolve_entity(store, &triple.subject, namespace, source_agent).await?;

        // Object: reuse a matching entity when one exists; otherwise keep
        // the raw text.
        let object_entity = resolve_existing_entity(store, &triple.object, namespace).await?;

        // Skip degenerate self-loops.
        if let Some(ref obj_id) = object_entity
            && obj_id == &subject_id
        {
            continue;
        }

        // Skip exact-duplicate triples.
        let existing = store.relations_of_subject(&subject_id).await?;
        let duplicate = existing.iter().any(|r| {
            r.predicate == triple.predicate
                && match (&object_entity, &r.object_id) {
                    (Some(a), Some(b)) => a == b,
                    (None, None) => r.object_text.as_deref() == Some(triple.object.as_str()),
                    _ => false,
                }
        });
        if duplicate {
            continue;
        }

        let object_text = if object_entity.is_some() {
            None
        } else {
            Some(triple.object.trim().to_string())
        };

        let relation = MemoryRelation {
            relation_id: None,
            subject_id,
            predicate: triple.predicate.trim().to_string(),
            object_id: object_entity,
            object_text,
            confidence: 0.7,
            source_memory_id: source_memory_id.map(str::to_string),
            created_at: Utc::now(),
        };
        store.add_relation(relation).await?;
        stored += 1;
    }
    debug!(stored, "relation triples persisted");
    Ok(stored)
}

/// Find an existing Entity memory matching `name`, or None.
async fn resolve_existing_entity(
    store: &(impl MemoryStore + ?Sized),
    name: &str,
    namespace: &str,
) -> Result<Option<String>> {
    let name_words = Deduplicator::tokenize(name);
    if name_words.is_empty() {
        return Ok(None);
    }
    let candidates = store
        .search(crate::models::SearchQuery {
            query: name.to_string(),
            namespace: Some(namespace.to_string()),
            type_filter: Some(MemoryType::Entity),
            top_k: 10,
            ..crate::models::SearchQuery::new(name.to_string())
        })
        .await?
        .results;
    for c in candidates {
        let cand_words = Deduplicator::tokenize(&c.memory.content);
        if Deduplicator::jaccard_similarity(&name_words, &cand_words) >= ENTITY_MATCH_THRESHOLD {
            return Ok(Some(c.memory.id));
        }
    }
    Ok(None)
}

/// Resolve a subject name to an entity memory id, creating the entity when
/// it does not exist yet (entity normalization at write time).
async fn resolve_entity(
    store: &(impl MemoryStore + ?Sized),
    name: &str,
    namespace: &str,
    source_agent: &SourceAgent,
) -> Result<String> {
    if let Some(id) = resolve_existing_entity(store, name, namespace).await? {
        return Ok(id);
    }
    let mut entity = Memory::new(
        MemoryType::Entity,
        name.trim().to_string(),
        Priority::Background,
        source_agent.clone(),
    );
    entity.namespace = namespace.to_string();
    entity.tags = vec!["entity".to_string()];
    // ai_generated=true + human_reviewed=false (defaults) → review queue.
    let saved = store.save(entity).await?;
    Ok(saved.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::sqlite::SqliteStore;

    fn agent() -> SourceAgent {
        SourceAgent {
            id: "tester".to_string(),
            agent_type: "coding-assistant".to_string(),
            session_id: None,
        }
    }

    fn triple(s: &str, p: &str, o: &str) -> ExtractedRelation {
        ExtractedRelation {
            subject: s.to_string(),
            predicate: p.to_string(),
            object: o.to_string(),
        }
    }

    #[tokio::test]
    async fn test_store_triples_creates_entities_and_relations() {
        let store = SqliteStore::in_memory().unwrap();
        let stored = store_relation_triples(
            &store,
            &[triple("dashboard service", "depends_on", "PostgreSQL")],
            "global",
            &agent(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(stored, 1);

        // Subject entity was created.
        let subjects = store
            .search(crate::models::SearchQuery {
                query: "dashboard service".to_string(),
                type_filter: Some(MemoryType::Entity),
                top_k: 5,
                ..crate::models::SearchQuery::new("dashboard service".to_string())
            })
            .await
            .unwrap()
            .results;
        assert!(!subjects.is_empty());
        let subject = &subjects[0].memory;
        assert!(!subject.human_reviewed, "created entities enter review");

        // Relation stored with the object as free text (no entity for it).
        let rels = store.relations_of_subject(&subject.id).await.unwrap();
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].predicate, "depends_on");
        assert_eq!(rels[0].object_text.as_deref(), Some("PostgreSQL"));
    }

    #[tokio::test]
    async fn test_store_triples_reuses_existing_subject_entity() {
        let store = SqliteStore::in_memory().unwrap();
        let n1 = store_relation_triples(
            &store,
            &[triple("dashboard service", "depends_on", "PostgreSQL")],
            "global",
            &agent(),
            None,
        )
        .await
        .unwrap();
        let n2 = store_relation_triples(
            &store,
            &[triple("dashboard service", "located_in", "prod-cluster")],
            "global",
            &agent(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(n1, 1);
        assert_eq!(n2, 1);

        // Only ONE entity memory for "dashboard service".
        let entities = store
            .search(crate::models::SearchQuery {
                query: "dashboard service".to_string(),
                type_filter: Some(MemoryType::Entity),
                top_k: 10,
                ..crate::models::SearchQuery::new("dashboard service".to_string())
            })
            .await
            .unwrap()
            .results;
        let exact: Vec<_> = entities
            .iter()
            .filter(|r| r.memory.content == "dashboard service")
            .collect();
        assert_eq!(exact.len(), 1);

        let rels = store
            .relations_of_subject(&exact[0].memory.id)
            .await
            .unwrap();
        assert_eq!(rels.len(), 2);
    }

    #[tokio::test]
    async fn test_store_triples_dedups_identical_triple() {
        let store = SqliteStore::in_memory().unwrap();
        let triples = vec![triple("svc", "uses", "redis")];
        let n1 = store_relation_triples(&store, &triples, "global", &agent(), None)
            .await
            .unwrap();
        let n2 = store_relation_triples(&store, &triples, "global", &agent(), None)
            .await
            .unwrap();
        assert_eq!(n1, 1);
        assert_eq!(n2, 0, "identical triple must not be stored twice");
    }

    #[tokio::test]
    async fn test_store_triples_skips_empty_and_self_loop() {
        let store = SqliteStore::in_memory().unwrap();
        let stored = store_relation_triples(
            &store,
            &[
                triple("", "uses", "x"),
                triple("a", "", "x"),
                triple("a", "uses", ""),
            ],
            "global",
            &agent(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(stored, 0);
    }

    #[tokio::test]
    async fn test_object_resolves_to_existing_entity() {
        let store = SqliteStore::in_memory().unwrap();
        // Create the object entity first via another triple.
        store_relation_triples(
            &store,
            &[triple("PostgreSQL", "maintained_by", "db-team")],
            "global",
            &agent(),
            None,
        )
        .await
        .unwrap();

        let stored = store_relation_triples(
            &store,
            &[triple("dashboard service", "depends_on", "PostgreSQL")],
            "global",
            &agent(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(stored, 1);

        // The new relation must point at the existing PostgreSQL entity, not
        // free text.
        let subjects = store
            .search(crate::models::SearchQuery {
                query: "dashboard service".to_string(),
                type_filter: Some(MemoryType::Entity),
                top_k: 5,
                ..crate::models::SearchQuery::new("dashboard service".to_string())
            })
            .await
            .unwrap()
            .results;
        let rels = store
            .relations_of_subject(&subjects[0].memory.id)
            .await
            .unwrap();
        assert_eq!(rels.len(), 1);
        assert!(rels[0].object_id.is_some());
        assert!(rels[0].object_text.is_none());

        // And the object entity sees the inbound relation.
        let inbound = store
            .relations_of_object(rels[0].object_id.as_ref().unwrap())
            .await
            .unwrap();
        assert_eq!(inbound.len(), 1);
    }

    #[tokio::test]
    async fn test_collect_relations_one_hop() {
        let store = SqliteStore::in_memory().unwrap();
        // dashboard --depends_on--> PostgreSQL, dashboard --located_in--> prod
        store_relation_triples(
            &store,
            &[
                triple("dashboard service", "depends_on", "PostgreSQL"),
                triple("dashboard service", "located_in", "prod-cluster"),
            ],
            "global",
            &agent(),
            None,
        )
        .await
        .unwrap();

        let subjects = store
            .search(crate::models::SearchQuery {
                query: "dashboard service".to_string(),
                type_filter: Some(MemoryType::Entity),
                top_k: 5,
                ..crate::models::SearchQuery::new("dashboard service".to_string())
            })
            .await
            .unwrap()
            .results;
        let dashboard_id = &subjects[0].memory.id;

        let neighborhood = collect_relations(&store, dashboard_id).await;
        assert_eq!(neighborhood.len(), 2, "both outgoing relations");
        let predicates: Vec<&str> = neighborhood.iter().map(|r| r.predicate.as_str()).collect();
        assert!(predicates.contains(&"depends_on"));
        assert!(predicates.contains(&"located_in"));
    }

    #[tokio::test]
    async fn test_relation_line_formats() {
        let rel_text = crate::models::MemoryRelation {
            relation_id: Some(1),
            subject_id: "mem_a".to_string(),
            predicate: "uses".to_string(),
            object_id: None,
            object_text: Some("redis".to_string()),
            confidence: 0.8,
            source_memory_id: None,
            created_at: chrono::Utc::now(),
        };
        assert_eq!(
            relation_line("svc", &rel_text),
            "svc —uses→ redis".to_string()
        );

        let rel_id = crate::models::MemoryRelation {
            relation_id: Some(2),
            subject_id: "mem_a".to_string(),
            predicate: "depends_on".to_string(),
            object_id: Some("mem_b".to_string()),
            object_text: None,
            confidence: 0.8,
            source_memory_id: None,
            created_at: chrono::Utc::now(),
        };
        assert_eq!(
            relation_line("svc", &rel_id),
            "svc —depends_on→ memory:mem_b".to_string()
        );
    }
}
