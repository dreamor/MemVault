use criterion::{Criterion, black_box, criterion_group, criterion_main};

use memvault_core::config::default_agent_registry;
use memvault_core::models::*;
use memvault_core::router::MemoryRouter;
use memvault_core::storage::MemoryStore;
use memvault_core::storage::sqlite::SqliteStore;
use std::sync::Arc;

fn setup_router(mem_count: usize) -> Arc<MemoryRouter> {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let agent = SourceAgent {
        id: "claude-desktop".to_string(),
        agent_type: "coding-assistant".to_string(),
        session_id: None,
    };

    for i in 0..mem_count {
        let mut m = Memory::new(
            MemoryType::Fact,
            format!(
                "Memory entry {}: user prefers pattern number {} in code",
                i,
                i % 50
            ),
            if i % 5 == 0 {
                Priority::Must
            } else {
                Priority::Reference
            },
            agent.clone(),
        );
        m.tags = vec!["coding".to_string(), format!("domain_{}", i % 10)];
        if i % 4 == 0 {
            m.instruction = Some(format!("Always use approach {} when coding", i % 20));
        }
        let _ = rt.block_on(store.save(m));
    }

    let registry = default_agent_registry();
    Arc::new(MemoryRouter::with_registry(store, registry))
}

fn bench_session_start_basic(c: &mut Criterion) {
    let router = setup_router(500);
    let rt = tokio::runtime::Runtime::new().unwrap();

    c.bench_function("session_start_basic_500", |b| {
        b.to_async(&rt).iter(|| async {
            let results = router
                .session_start(
                    black_box("claude-desktop"),
                    black_box(None),
                    black_box(None),
                )
                .await
                .unwrap().results;
            black_box(results)
        })
    });
}

fn bench_session_start_with_context(c: &mut Criterion) {
    let router = setup_router(500);
    let rt = tokio::runtime::Runtime::new().unwrap();

    c.bench_function("session_start_with_context_500", |b| {
        b.to_async(&rt).iter(|| async {
            let results = router
                .session_start(
                    black_box("claude-desktop"),
                    black_box(Some("build a Python API with FastAPI")),
                    black_box(Some("my-project")),
                )
                .await
                .unwrap().results;
            black_box(results)
        })
    });
}

fn bench_session_start_large(c: &mut Criterion) {
    let router = setup_router(2000);
    let rt = tokio::runtime::Runtime::new().unwrap();

    c.bench_function("session_start_large_2000", |b| {
        b.to_async(&rt).iter(|| async {
            let results = router
                .session_start(
                    black_box("claude-desktop"),
                    black_box(None),
                    black_box(None),
                )
                .await
                .unwrap().results;
            black_box(results)
        })
    });
}

criterion_group!(
    name = session;
    config = Criterion::default().sample_size(50);
    targets = bench_session_start_basic, bench_session_start_with_context, bench_session_start_large
);
criterion_main!(session);
