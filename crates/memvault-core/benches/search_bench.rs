use criterion::{Criterion, black_box, criterion_group, criterion_main};

use memvault_core::models::*;
use memvault_core::storage::MemoryStore;
use memvault_core::storage::sqlite::SqliteStore;
use std::sync::Arc;

fn setup_store(mem_count: usize) -> Arc<SqliteStore> {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let agent = SourceAgent {
        id: "bench".to_string(),
        agent_type: "benchmark".to_string(),
        session_id: None,
    };

    for i in 0..mem_count {
        let mut m = Memory::new(
            MemoryType::Fact,
            format!(
                "Benchmark memory entry number {} with enough content for realistic search",
                i
            ),
            if i % 10 == 0 {
                Priority::Must
            } else {
                Priority::Reference
            },
            agent.clone(),
        );
        m.tags = vec![format!("tag_{}", i % 20), "common".to_string()];
        if i % 3 == 0 {
            m.instruction = Some(format!("Use this instruction for item {}", i));
        }
        let _ = rt.block_on(store.save(m));
    }

    store
}

fn bench_keyword_search(c: &mut Criterion) {
    let store = setup_store(500);
    let rt = tokio::runtime::Runtime::new().unwrap();

    c.bench_function("search_keyword_500", |b| {
        b.to_async(&rt).iter(|| async {
            let results = store
                .search(SearchQuery::new(black_box("Benchmark memory".to_string())))
                .await
                .unwrap();
            black_box(results)
        })
    });
}

fn bench_empty_search(c: &mut Criterion) {
    let store = setup_store(500);
    let rt = tokio::runtime::Runtime::new().unwrap();

    c.bench_function("search_empty_query_500", |b| {
        b.to_async(&rt).iter(|| async {
            let results = store
                .search(SearchQuery::new(black_box(String::new())))
                .await
                .unwrap();
            black_box(results)
        })
    });
}

fn bench_search_with_filters(c: &mut Criterion) {
    let store = setup_store(500);
    let rt = tokio::runtime::Runtime::new().unwrap();

    c.bench_function("search_priority_must_500", |b| {
        b.to_async(&rt).iter(|| async {
            let results = store
                .search(SearchQuery {
                    query: black_box("memory".to_string()),
                    priority_filter: Some(Priority::Must),
                    top_k: 10,
                    ..SearchQuery::new(String::new())
                })
                .await
                .unwrap();
            black_box(results)
        })
    });
}

fn bench_list_memories(c: &mut Criterion) {
    let store = setup_store(500);
    let rt = tokio::runtime::Runtime::new().unwrap();

    c.bench_function("list_500", |b| {
        b.to_async(&rt).iter(|| async {
            let results = store
                .list(black_box(None), black_box(100), black_box(0))
                .await
                .unwrap();
            black_box(results)
        })
    });
}

criterion_group!(
    name = search;
    config = Criterion::default().sample_size(50);
    targets = bench_keyword_search, bench_empty_search, bench_search_with_filters, bench_list_memories
);
criterion_main!(search);
