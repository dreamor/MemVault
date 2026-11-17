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

/// Resolve the actual model cache root: explicit cache dir >
/// `$MEMVAULT_HOME/models` > `<home>/.memvault/models`. Empty settings are unset.
/// Expand `~/` just like other path settings, with HOME supplied for pure tests.
fn resolve_model_dir(
    cache_dir: Option<&str>,
    memvault_home: Option<&str>,
    home: Option<&str>,
) -> PathBuf {
    let path = |value: Option<&str>| {
        value.filter(|v| !v.trim().is_empty()).map(|v| {
            let expanded = v
                .strip_prefix("~/")
                .zip(home)
                .map(|(rest, home)| format!("{home}/{rest}"));
            PathBuf::from(expanded.as_deref().unwrap_or(v))
        })
    };
    path(cache_dir)
        .or_else(|| path(memvault_home).map(|h| h.join("models")))
        .unwrap_or_else(|| PathBuf::from(home.unwrap_or(".")).join(".memvault/models"))
}

/// 从环境变量构建内嵌 provider;模型初始化失败(如首次下载失败)时返回 `None`,
/// 调用方应据此将语义检索降级为关键词模式(失败提示见 warn,下次构建会重试自动下载)。
pub async fn try_build_native_from_env() -> Option<Arc<dyn EmbeddingProvider>> {
    let model_name = std::env::var("MEMVAULT_EMBEDDING_MODEL").ok();
    let model = resolve_native_model(model_name.as_deref());
    let cache_dir = std::env::var("MEMVAULT_EMBEDDING_CACHE_DIR").ok();
    let memvault_home = std::env::var("MEMVAULT_HOME").ok();
    let home = std::env::var("HOME").ok();
    let cache_dir = resolve_model_dir(
        cache_dir.as_deref(),
        memvault_home.as_deref(),
        home.as_deref(),
    );

    match NativeEmbedding::try_new(model, Some(cache_dir)) {
        Ok(provider) => Some(Arc::new(provider)),
        Err(e) => {
            warn!(
                error = %e,
                "native embedding init failed — semantic search degraded to keyword-only; the model auto-downloads on first successful init, so a later save/status retry may succeed"
            );
            None
        }
    }
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
    #[test]
    fn resolve_model_dir_explicit_cache_overrides_vault_home() {
        assert_eq!(
            resolve_model_dir(
                Some("/shared/embedding-cache"),
                Some("/var/lib/memvault"),
                Some("/home/u")
            ),
            PathBuf::from("/shared/embedding-cache")
        );
        assert_eq!(
            resolve_model_dir(Some("relative-cache"), None, Some("/home/u")),
            PathBuf::from("relative-cache")
        );
    }

    #[test]
    fn resolve_model_dir_uses_memvault_home_when_set() {
        for cache in [None, Some(""), Some(" \t ")] {
            assert_eq!(
                resolve_model_dir(cache, Some("/var/lib/memvault"), Some("/home/u")),
                PathBuf::from("/var/lib/memvault/models")
            );
        }
    }

    #[test]
    fn resolve_model_dir_falls_back_to_home() {
        for vault_home in [None, Some(""), Some(" \t ")] {
            assert_eq!(
                resolve_model_dir(None, vault_home, Some("/home/user")),
                PathBuf::from("/home/user/.memvault/models")
            );
        }
        assert_eq!(
            resolve_model_dir(Some(""), Some(""), Some("/home/user")),
            PathBuf::from("/home/user/.memvault/models")
        );
    }

    #[test]
    fn resolve_model_dir_expands_tilde_for_both_settings() {
        assert_eq!(
            resolve_model_dir(Some("~/cache"), Some("~/vault"), Some("/home/u")),
            PathBuf::from("/home/u/cache")
        );
        assert_eq!(
            resolve_model_dir(None, Some("~/vault"), Some("/home/u")),
            PathBuf::from("/home/u/vault/models")
        );
        // Only the same leading `~/` form used by other path settings expands.
        assert_eq!(
            resolve_model_dir(Some("~other/cache"), None, Some("/home/u")),
            PathBuf::from("~other/cache")
        );
    }

    #[test]
    fn resolve_model_dir_handles_missing_home() {
        assert_eq!(
            resolve_model_dir(None, None, None),
            PathBuf::from("./.memvault/models")
        );
        assert_eq!(
            resolve_model_dir(Some("~/cache"), None, None),
            PathBuf::from("~/cache")
        );
    }
}
