use std::sync::Arc;

use crate::embedding::EmbeddingProvider;

/// One row of the "what works without an LLM/embedding provider" report
/// surfaced by `memvault status`.
pub struct CapabilityStatus {
    pub name: &'static str,
    pub available: bool,
    pub note: &'static str,
}

/// Reports which MemVault features are available given the current
/// embedding provider state. MemVault has no separate "LLM for generation"
/// concept — embedding is the only optional external-service dependency in
/// the retrieval/extraction/dedup pipeline, so this is the single input.
pub fn capability_report(embedder: &Option<Arc<dyn EmbeddingProvider>>) -> Vec<CapabilityStatus> {
    let has_embedder = embedder.is_some();
    vec![
        CapabilityStatus {
            name: "关键词检索 (List/Search)",
            available: true,
            note: "始终可用,基于 SQLite LIKE + 同义词扩展,不依赖 embedding",
        },
        CapabilityStatus {
            name: "规则实体抽取 (Extract)",
            available: true,
            note: "纯规则匹配,不依赖 LLM",
        },
        CapabilityStatus {
            name: "衰减/归档 (Decay)",
            available: true,
            note: "纯时间衰减公式,不依赖 LLM/embedding",
        },
        CapabilityStatus {
            name: "去重 - 关键词基线 (Jaccard)",
            available: true,
            note: "始终可用",
        },
        CapabilityStatus {
            name: "去重 - 向量辅助",
            available: has_embedder,
            note: if has_embedder {
                "已启用"
            } else {
                "未配置 embedding provider,降级为纯 Jaccard 匹配"
            },
        },
        CapabilityStatus {
            name: "MCP 混合/语义检索 (search_memory mode=hybrid/semantic)",
            available: has_embedder,
            note: if has_embedder {
                "已启用,仅 memvault-mcp 提供(CLI search 不支持 mode 参数)"
            } else {
                "未配置 embedding provider,hybrid 会静默降级为纯关键词,semantic 会报错"
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeEmbedder;
    #[async_trait::async_trait]
    impl EmbeddingProvider for FakeEmbedder {
        async fn embed(&self, texts: &[String]) -> crate::error::Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|_| vec![0.0]).collect())
        }
        fn dimension(&self) -> usize {
            1
        }
    }

    #[test]
    fn test_report_without_embedder_marks_vector_features_unavailable() {
        let report = capability_report(&None);
        let always_on: Vec<_> = report.iter().filter(|c| !c.available).collect();
        assert_eq!(always_on.len(), 2);
        assert!(
            always_on
                .iter()
                .all(|c| c.name.contains("向量") || c.name.contains("hybrid"))
        );
        // Baseline features never degrade.
        assert!(report.iter().any(|c| c.name.contains("关键词检索") && c.available));
        assert!(report.iter().any(|c| c.name.contains("Jaccard") && c.available));
    }

    #[test]
    fn test_report_with_embedder_marks_all_available() {
        let embedder: Option<Arc<dyn EmbeddingProvider>> = Some(Arc::new(FakeEmbedder));
        let report = capability_report(&embedder);
        assert!(report.iter().all(|c| c.available));
    }
}
