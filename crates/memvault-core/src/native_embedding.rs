use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
use tracing::{info, warn};

use crate::embedding::EmbeddingProvider;
use crate::error::{MemVaultError, Result};

/// 内嵌本地 embedding 推理(fastembed + ONNX Runtime 进程内运行)。
///
/// 不需要任何外部服务(与 `ollama` / `openai-compatible` 不同),
/// 首次 `try_new` 会自动从 HuggingFace 下载模型权重到缓存目录。
pub struct NativeEmbedding {
    model: Mutex<TextEmbedding>,
    dimension: usize,
}

impl NativeEmbedding {
    /// 初始化内嵌模型。`cache_dir` 指定模型下载目录(缺省由调用方决定)。
    ///
    /// `TextEmbedding::embed` 是同步方法且需要 `&mut self`,因此内部用 `Mutex` 串行化,
    /// 保证 `EmbeddingProvider::embed(&self)` 的并发安全。
    pub fn try_new(model: EmbeddingModel, cache_dir: Option<PathBuf>) -> Result<Self> {
        let (dimension, description) = {
            let model_info = TextEmbedding::get_model_info(&model).map_err(|e| {
                MemVaultError::Storage(format!(
                    "failed to look up native embedding model info: {e}"
                ))
            })?;
            (model_info.dim, model_info.description.clone())
        };

        let mut opts = TextInitOptions::new(model).with_show_download_progress(true);
        if let Some(dir) = cache_dir {
            opts = opts.with_cache_dir(dir);
        }
        let text_model = TextEmbedding::try_new(opts).map_err(|e| {
            MemVaultError::Storage(format!("failed to init native embedding model: {e}"))
        })?;

        info!(model = %description, dimension, "native embedding provider initialized");
        Ok(Self {
            model: Mutex::new(text_model),
            dimension,
        })
    }
}

#[async_trait]
impl EmbeddingProvider for NativeEmbedding {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let mut guard = self
            .model
            .lock()
            .map_err(|_| MemVaultError::Storage("embedding model lock poisoned".to_string()))?;
        let embeddings = guard
            .embed(texts, None)
            .map_err(|e| MemVaultError::Storage(format!("native embedding error: {e}")))?;
        Ok(embeddings)
    }

    fn dimension(&self) -> usize {
        self.dimension
    }
}

/// 将 `MEMVAULT_EMBEDDING_MODEL` 的值映射到内嵌模型。
///
/// - `multilingual` / `multi` / `e5` / `e5-base` → BGE 多语言替代:Multilingual-E5-base
/// - 其他 / 缺省 → BGE 中文小模型 `bge-small-zh-v1.5`(体积小、中文效果好)
pub fn resolve_native_model(name: Option<&str>) -> EmbeddingModel {
    match name.unwrap_or("").to_ascii_lowercase().as_str() {
        "multilingual" | "multi" | "e5" | "e5-base" | "multilingual-e5-base" => {
            EmbeddingModel::MultilingualE5Base
        }
        _ => EmbeddingModel::BGESmallZHV15,
    }
}

/// 从环境变量构建内嵌 provider;模型初始化失败(如首次下载失败)时降级返回 `None`。
pub async fn try_build_native_from_env() -> Option<Arc<dyn EmbeddingProvider>> {
    let model_name = std::env::var("MEMVAULT_EMBEDDING_MODEL").ok();
    let model = resolve_native_model(model_name.as_deref());
    let cache_dir = std::env::var("MEMVAULT_HOME")
        .map(PathBuf::from)
        .ok()
        .map(|home| home.join("models"))
        .or_else(|| Some(PathBuf::from(env_home()).join(".memvault/models")));

    match NativeEmbedding::try_new(model, cache_dir) {
        Ok(provider) => Some(Arc::new(provider)),
        Err(e) => {
            warn!(error = %e, "native embedding init failed — fallback to keyword-only");
            None
        }
    }
}

fn env_home() -> String {
    std::env::var("HOME").unwrap_or_else(|_| ".".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastembed::EmbeddingModel;

    #[test]
    fn resolve_default_is_chinese() {
        assert!(matches!(
            resolve_native_model(None),
            EmbeddingModel::BGESmallZHV15
        ));
        assert!(matches!(
            resolve_native_model(Some("")),
            EmbeddingModel::BGESmallZHV15
        ));
        assert!(matches!(
            resolve_native_model(Some("bge-small-zh")),
            EmbeddingModel::BGESmallZHV15
        ));
    }

    #[test]
    fn resolve_multilingual_model() {
        assert!(matches!(
            resolve_native_model(Some("multilingual")),
            EmbeddingModel::MultilingualE5Base
        ));
        assert!(matches!(
            resolve_native_model(Some("multi")),
            EmbeddingModel::MultilingualE5Base
        ));
        assert!(matches!(
            resolve_native_model(Some("e5-base")),
            EmbeddingModel::MultilingualE5Base
        ));
    }
}
