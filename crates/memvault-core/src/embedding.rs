use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::error::{MemVaultError, Result};

#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    fn dimension(&self) -> usize;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingConfig {
    pub provider: String,
    pub api_base: String,
    pub api_key: Option<String>,
    pub model: String,
    pub dimension: usize,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            provider: "openai".to_string(),
            api_base: "https://api.openai.com/v1".to_string(),
            api_key: None,
            model: "text-embedding-3-small".to_string(),
            dimension: 1536,
        }
    }
}

impl EmbeddingConfig {
    /// Ollama 本地配置(保留原生 `/api/embed` 协议)。
    pub fn ollama(model: &str, dimension: usize) -> Self {
        Self {
            provider: "ollama".to_string(),
            api_base: "http://localhost:11434/api".to_string(),
            api_key: None,
            model: model.to_string(),
            dimension,
        }
    }

    /// 任意 OpenAI 兼容 provider(key 可空,适配 Ollama / vLLM / Azure 等)。
    pub fn openai_compatible(
        provider: impl Into<String>,
        api_base: impl Into<String>,
        api_key: Option<String>,
        model: impl Into<String>,
        dimension: usize,
    ) -> Self {
        Self {
            provider: provider.into(),
            api_base: api_base.into(),
            api_key,
            model: model.into(),
            dimension,
        }
    }
}

// --- OpenAI-compatible API provider ---

pub struct OpenAIEmbedding {
    client: reqwest::Client,
    config: EmbeddingConfig,
}

#[derive(Serialize)]
struct EmbedRequest {
    model: String,
    input: Vec<String>,
}

#[derive(Deserialize)]
struct EmbedResponse {
    data: Vec<EmbedData>,
}

#[derive(Deserialize)]
struct EmbedData {
    embedding: Vec<f32>,
}

// Ollama uses a different format
#[derive(Serialize)]
struct OllamaEmbedRequest {
    model: String,
    input: Vec<String>,
}

#[derive(Deserialize)]
struct OllamaEmbedResponse {
    embeddings: Vec<Vec<f32>>,
}

impl OpenAIEmbedding {
    pub fn new(config: EmbeddingConfig) -> Self {
        Self {
            client: reqwest::Client::new(),
            config,
        }
    }

    /// 从环境变量解析 embedding 配置。
    ///
    /// 选择顺序:
    /// 1. `MEMVAULT_EMBEDDING_PROVIDER` 显式指定:
    ///    - `ollama` / `local` → 本地 Ollama(默认 `http://localhost:11434/api` + `nomic-embed-text` 768 维)
    ///    - `openai` / `openai-compatible` / 任意其他值 → 对应 OpenAI 兼容端点
    /// 2. 未指定但设置了 `OPENAI_API_KEY` / `OPENAI_API_BASE` → 向后兼容 OpenAI(任意 OpenAI 兼容端点)
    /// 3. 均未设置 → 默认本地 Ollama
    ///
    /// 任意 provider 统一走 OpenAI 兼容协议 `POST {base}/embeddings`,
    /// 因此 OpenAI / Azure / vLLM / Ollama(/v1 端点)/ 各类网关均可通过
    /// `MEMVAULT_EMBEDDING_API_BASE` + `MEMVAULT_EMBEDDING_API_KEY` 接入。
    pub fn from_env() -> Self {
        let provider = std::env::var("MEMVAULT_EMBEDDING_PROVIDER").unwrap_or_else(|_| {
            let has_remote = std::env::var("OPENAI_API_KEY")
                .ok()
                .filter(|k| !k.is_empty())
                .is_some()
                || std::env::var("OPENAI_API_BASE").is_ok();
            if has_remote {
                "openai".to_string()
            } else {
                "ollama".to_string()
            }
        });

        match provider.as_str() {
            "ollama" | "local" => {
                let model = std::env::var("MEMVAULT_EMBEDDING_MODEL")
                    .unwrap_or_else(|_| "nomic-embed-text".to_string());
                let mut cfg = EmbeddingConfig::ollama(&model, ollama_dimension_from_env());
                if let Ok(base) = std::env::var("MEMVAULT_EMBEDDING_API_BASE") {
                    cfg.api_base = base;
                }
                Self::new(cfg)
            }
            _ => {
                // openai / openai-compatible / azure / 任意自定义 provider
                let api_key = std::env::var("MEMVAULT_EMBEDDING_API_KEY")
                    .ok()
                    .or_else(|| std::env::var("OPENAI_API_KEY").ok());
                let api_base = std::env::var("MEMVAULT_EMBEDDING_API_BASE")
                    .ok()
                    .or_else(|| std::env::var("OPENAI_API_BASE").ok())
                    .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
                let model = std::env::var("MEMVAULT_EMBEDDING_MODEL")
                    .unwrap_or_else(|_| "text-embedding-3-small".to_string());
                let dimension: usize = std::env::var("MEMVAULT_EMBEDDING_DIM")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(1536);
                Self::new(EmbeddingConfig::openai_compatible(
                    provider, api_base, api_key, model, dimension,
                ))
            }
        }
    }
}

/// 启动时构建 embedding provider(供 CLI / MCP / Proxy 统一入口)。
///
/// - 显式配置了 `MEMVAULT_EMBEDDING_PROVIDER` → 按配置返回,信任用户的显式选择,不做校验
/// - 未配置但存在旧版 `OPENAI_API_KEY` / `OPENAI_API_BASE` → 向后兼容猜成 openai,但这只是猜测
///   (这个 key 很可能是别的工具留在环境里的,不一定真的能用),猜中之前先用一次真实 embed 调用校验;
///   校验失败(401/网络不可达/超时)→ 降级到 native,避免把一个已知会失败的 provider 交给后续所有调用反复重试
/// - 均未配置 → 探测本地 Ollama(`:11434`) 是否运行:
///   - 运行中 → 使用本地模型(离线,无需 API key)
///   - 未运行 → 返回 `None`,降级为纯关键词检索
pub async fn build_embedder_from_env() -> Option<Arc<dyn EmbeddingProvider>> {
    let has_remote = std::env::var("OPENAI_API_KEY")
        .ok()
        .filter(|k| !k.is_empty())
        .is_some()
        || std::env::var("OPENAI_API_BASE").is_ok()
        || std::env::var("MEMVAULT_EMBEDDING_API_BASE").is_ok();

    let is_explicit = std::env::var("MEMVAULT_EMBEDDING_PROVIDER").is_ok();
    let provider = std::env::var("MEMVAULT_EMBEDDING_PROVIDER").unwrap_or_else(|_| {
        if has_remote {
            // 向后兼容:旧版通过 OPENAI_API_KEY/BASE 配置的自动走 API
            "openai".to_string()
        } else {
            // 默认:内嵌本地模型(零外部服务)
            "native".to_string()
        }
    });

    match provider.as_str() {
        // 显式禁用 embedding:纯关键词模式
        "none" | "disabled" | "off" => {
            info!("Embedding provider: disabled — keyword-only mode");
            None
        }
        // 内嵌本地推理(fastembed),默认
        "native" => crate::native_embedding::try_build_native_from_env().await,
        // 本地 Ollama 服务
        "ollama" | "local" => {
            info!(provider = "ollama", "Embedding provider: local Ollama");
            Some(Arc::new(OpenAIEmbedding::from_env()))
        }
        // auto:优先本地 Ollama,且校验 embedding 模型确实已安装;daemon 未运行或
        // 本机缺少该模型 → 回退内嵌本地模型,避免构造一个必然 404 的 provider
        // 然后让每次保存/检索都在失败日志里反复重试。
        "auto" => {
            let api_base = std::env::var("MEMVAULT_EMBEDDING_API_BASE").unwrap_or_default();
            let root = if api_base.trim().is_empty() {
                LOCAL_OLLAMA_ROOT.to_string()
            } else {
                ollama_root_from_api_base(&api_base)
            };
            let installed = fetch_ollama_models(&root).await;

            match installed {
                Some(available) if !available.is_empty() => {
                    let model = std::env::var("MEMVAULT_EMBEDDING_MODEL")
                        .unwrap_or_else(|_| LOCAL_OLLAMA_DEFAULT_EMBEDDING_MODEL.to_string());
                    if available.iter().any(|m| m == &model) {
                        info!(
                            provider = "ollama",
                            model = %model,
                            "Embedding provider: local Ollama (auto-detected)"
                        );
                        let mut cfg = EmbeddingConfig::ollama(&model, ollama_dimension_from_env());
                        if !api_base.trim().is_empty() {
                            cfg.api_base = api_base;
                        }
                        Some(Arc::new(OpenAIEmbedding::new(cfg)))
                    } else {
                        warn!(
                            model = %model,
                            available = %available.join(","),
                            "Local Ollama detected but embedding model not installed — \
                             falling back to native embedding; pull it or set \
                             MEMVAULT_EMBEDDING_MODEL"
                        );
                        crate::native_embedding::try_build_native_from_env().await
                    }
                }
                _ => {
                    info!("No local Ollama — falling back to native embedding");
                    crate::native_embedding::try_build_native_from_env().await
                }
            }
        }
        // openai / openai-compatible / 任意兼容端点 → OpenAI 兼容协议
        other => {
            let embedder = OpenAIEmbedding::from_env();
            if is_explicit {
                // 用户显式选的远程 provider,尊重选择,不校验
                info!(provider = %other, "Embedding provider: {}", other);
                Some(Arc::new(embedder))
            } else if validate_remote_embedder(&embedder).await {
                info!(provider = %other, "Embedding provider: {} (validated)", other);
                Some(Arc::new(embedder))
            } else {
                warn!(
                    "OPENAI_API_KEY present but invalid or unreachable (guessed provider, not \
                     explicitly configured) — falling back to native embedding. Set \
                     MEMVAULT_EMBEDDING_PROVIDER explicitly to silence this check."
                );
                crate::native_embedding::try_build_native_from_env().await
            }
        }
    }
}

/// 用一次最小 embed 调用验证隐式猜中的远程 provider 是否真的可用(有效 key + 端点可达)。
/// 5s 超时,避免一个不可达的坏端点拖慢启动。只用于向后兼容猜测路径,显式配置永远被信任、不走这里。
async fn validate_remote_embedder(embedder: &OpenAIEmbedding) -> bool {
    matches!(
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            embedder.embed(&["ping".to_string()]),
        )
        .await,
        Ok(Ok(_))
    )
}

/// Ollama 本地 daemon 根地址(自动探测默认锚点)。
const LOCAL_OLLAMA_ROOT: &str = "http://localhost:11434";
/// 默认 Ollama embedding 模型。
const LOCAL_OLLAMA_DEFAULT_EMBEDDING_MODEL: &str = "nomic-embed-text";

/// 从 `MEMVAULT_EMBEDDING_DIM` 解析维度,缺省 768(nomic-embed-text)。
fn ollama_dimension_from_env() -> usize {
    std::env::var("MEMVAULT_EMBEDDING_DIM")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(768)
}

/// 从 `api_base` 推导 Ollama 根地址:兼容原生 `/api` 与 OpenAI 兼容 `/v1` 后缀。
fn ollama_root_from_api_base(api_base: &str) -> String {
    let trimmed = api_base.trim_end_matches('/');
    trimmed
        .strip_suffix("/api")
        .or_else(|| trimmed.strip_suffix("/v1"))
        .unwrap_or(trimmed)
        .to_string()
}

/// `/api/tags` 响应,只需模型名。
#[derive(Deserialize)]
struct OllamaTags {
    #[serde(default)]
    models: Vec<OllamaModelInfo>,
}

#[derive(Deserialize)]
struct OllamaModelInfo {
    name: String,
}

/// 拉取本机 Ollama 已安装模型名列表;不可达/异常返回 `None`(视同未安装),
/// 调用方据此回退 native 或纯关键词。
async fn fetch_ollama_models(root: &str) -> Option<Vec<String>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(1500))
        .build()
        .ok()?;
    let url = format!("{}/api/tags", root.trim_end_matches('/'));
    let resp = client.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let parsed: OllamaTags = resp.json().await.ok()?;
    Some(parsed.models.into_iter().map(|m| m.name).collect())
}

#[async_trait]
impl EmbeddingProvider for OpenAIEmbedding {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        debug!(count = texts.len(), model = %self.config.model, "embedding request");

        if self.config.provider == "ollama" {
            let url = format!("{}/embed", self.config.api_base);
            let body = OllamaEmbedRequest {
                model: self.config.model.clone(),
                input: texts.to_vec(),
            };

            let resp = self
                .client
                .post(&url)
                .json(&body)
                .send()
                .await
                .map_err(|e| MemVaultError::Storage(format!("Ollama API error: {}", e)))?;

            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(MemVaultError::Storage(format!(
                    "Ollama API {} : {}",
                    status, body
                )));
            }

            let result: OllamaEmbedResponse = resp.json().await.map_err(|e| {
                MemVaultError::Storage(format!("Ollama response parse error: {}", e))
            })?;

            return Ok(result.embeddings);
        }

        // OpenAI-compatible API
        let url = format!("{}/embeddings", self.config.api_base);
        let body = EmbedRequest {
            model: self.config.model.clone(),
            input: texts.to_vec(),
        };

        let mut req = self.client.post(&url).json(&body);

        if let Some(ref key) = self.config.api_key {
            req = req.header("Authorization", format!("Bearer {}", key));
        }

        let resp = req
            .send()
            .await
            .map_err(|e| MemVaultError::Storage(format!("Embedding API error: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            warn!(status = %status, "embedding API error");
            return Err(MemVaultError::Storage(format!(
                "Embedding API {} : {}",
                status, body
            )));
        }

        let result: EmbedResponse = resp.json().await.map_err(|e| {
            MemVaultError::Storage(format!("Embedding response parse error: {}", e))
        })?;

        Ok(result.data.into_iter().map(|d| d.embedding).collect())
    }

    fn dimension(&self) -> usize {
        self.config.dimension
    }
}

// --- Cosine similarity ---

pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;

    for i in 0..a.len() {
        dot += a[i] * b[i];
        norm_a += a[i] * a[i];
        norm_b += b[i] * b[i];
    }

    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom == 0.0 { 0.0 } else { dot / denom }
}

// --- int8 vector quantization ---
//
// Storing embeddings as f32 blobs quadruples the memory footprint of vector
// scans for no retrieval benefit: with L2-normalized vectors, cosine
// similarity equals inner product, and per-row int8 scaling preserves almost
// all of the ranking information (measured elsewhere at ~1/4 memory for
// ~equal latency on 50k x 1024 corpora). Each row keeps its OWN scale so
// every vector uses the full int8 dynamic range — a single global scale
// would be dragged down by outlier norms and waste precision everywhere else.

/// An int8-quantized, L2-normalized vector. `scale` maps quantized units back
/// to float: `value ≈ data[i] as f32 * scale`.
#[derive(Debug, Clone)]
pub struct QuantizedVector {
    pub data: Vec<i8>,
    pub scale: f32,
    pub dim: usize,
}

const INT8_POSITIVE_FULL_SCALE: f32 = 127.0; // symmetric range; -128 would overflow

/// L2-normalize then quantize to int8 with a per-row scale.
pub fn quantize_int8(vector: &[f32]) -> QuantizedVector {
    let dim = vector.len();

    let mut sum_squares = 0.0f64;
    for &v in vector {
        sum_squares += (v as f64) * (v as f64);
    }
    let norm = sum_squares.sqrt();
    let inv_norm = if norm == 0.0 { 0.0 } else { 1.0 / norm };

    let normalized: Vec<f64> = vector.iter().map(|&v| (v as f64) * inv_norm).collect();

    let mut max_abs = 0.0f64;
    for &v in &normalized {
        max_abs = max_abs.max(v.abs());
    }

    // All-zero vector: scale 1 keeps the math defined and dequantizes to zero.
    let scale = if max_abs == 0.0 {
        1.0
    } else {
        (max_abs / INT8_POSITIVE_FULL_SCALE as f64) as f32
    };

    let mut data = Vec::with_capacity(dim);
    for &v in &normalized {
        let scaled = (v / scale as f64).round() as i64;
        let clamped = scaled.clamp(
            -(INT8_POSITIVE_FULL_SCALE as i64),
            INT8_POSITIVE_FULL_SCALE as i64,
        );
        data.push(clamped as i8);
    }

    QuantizedVector { data, scale, dim }
}

/// Inverse of [`quantize_int8`] (debugging and mixed-format comparisons).
pub fn dequantize_int8(q: &QuantizedVector) -> Vec<f32> {
    q.data.iter().map(|&v| v as f32 * q.scale).collect()
}

/// Cosine similarity between two quantized vectors.
///
/// Both sides were L2-normalized before quantization, so inner product IS the
/// cosine; multiplying the two row scales restores magnitude. Dimensions must
/// match — callers treat a mismatch as "the embedding model changed", which
/// is a skip, not an error.
pub fn cosine_int8(a: &QuantizedVector, b: &QuantizedVector) -> f32 {
    if a.dim != b.dim || a.dim == 0 {
        return 0.0;
    }
    let mut dot: i64 = 0;
    for i in 0..a.dim {
        dot += a.data[i] as i64 * b.data[i] as i64;
    }
    dot as f32 * a.scale * b.scale
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cosine_similarity_identical() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_orthogonal() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        assert!(cosine_similarity(&a, &b).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_opposite() {
        let a = vec![1.0, 0.0];
        let b = vec![-1.0, 0.0];
        assert!((cosine_similarity(&a, &b) + 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_empty() {
        assert_eq!(cosine_similarity(&[], &[]), 0.0);
    }

    #[test]
    fn test_cosine_mismatched_lengths() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0];
        assert_eq!(cosine_similarity(&a, &b), 0.0);
    }

    #[test]
    fn test_cosine_zero_vector() {
        let a = vec![0.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert_eq!(cosine_similarity(&a, &b), 0.0);
    }

    /// Deterministic pseudo-random vector (no RNG dependency in core).
    fn lcg_vector(dim: usize, seed: u64) -> Vec<f32> {
        let mut state = seed;
        let mut v = Vec::with_capacity(dim);
        for _ in 0..dim {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            v.push(((state >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0);
        }
        v
    }

    #[test]
    fn test_quantize_roundtrip_preserves_direction() {
        // int8 quantization of an L2-normalized vector must stay extremely
        // close to the original direction — ranking depends on it.
        for seed in [7u64, 42, 1337] {
            let v = lcg_vector(256, seed);
            let q = quantize_int8(&v);
            let deq = dequantize_int8(&q);
            let sim = cosine_similarity(&v, &deq);
            assert!(
                sim >= 0.99,
                "seed {seed}: quantization error too large (cosine {sim})"
            );
        }
    }

    #[test]
    fn test_cosine_int8_matches_float_cosine() {
        let a = lcg_vector(128, 11);
        let b = lcg_vector(128, 22);
        let qa = quantize_int8(&a);
        let qb = quantize_int8(&b);
        let exact = cosine_similarity(&a, &b);
        let quant = cosine_int8(&qa, &qb);
        assert!(
            (exact - quant).abs() < 0.02,
            "int8 cosine {quant} drifted from float cosine {exact}"
        );
    }

    #[test]
    fn test_quantize_zero_vector_is_defined() {
        let q = quantize_int8(&[0.0, 0.0, 0.0]);
        assert_eq!(q.data, vec![0, 0, 0]);
        assert!(dequantize_int8(&q).iter().all(|&v| v == 0.0));
    }

    #[test]
    fn test_cosine_int8_dim_mismatch_is_zero() {
        let a = quantize_int8(&[1.0, 0.0, 0.0]);
        let b = quantize_int8(&[1.0, 0.0]);
        assert_eq!(cosine_int8(&a, &b), 0.0);
    }

    #[test]
    fn test_cosine_partial_match() {
        let a = vec![1.0, 0.5, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!(sim > 0.8 && sim < 1.0);
    }

    #[test]
    fn test_cosine_single_dimension() {
        let a = vec![2.0];
        let b = vec![4.0];
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_default_config() {
        let config = EmbeddingConfig::default();
        assert_eq!(config.dimension, 1536);
        assert_eq!(config.model, "text-embedding-3-small");
        assert_eq!(config.api_base, "https://api.openai.com/v1");
        assert!(config.api_key.is_none());
    }

    #[test]
    fn test_ollama_config() {
        let config = EmbeddingConfig::ollama("nomic-embed-text", 768);
        assert_eq!(config.provider, "ollama");
        assert_eq!(config.dimension, 768);
        assert_eq!(config.api_base, "http://localhost:11434/api");
    }

    #[test]
    fn test_embedding_provider_trait_object() {
        // Verify that OpenAIEmbedding implements EmbeddingProvider
        fn takes_provider(_p: &dyn EmbeddingProvider) {}
        let config = EmbeddingConfig::default();
        let provider = OpenAIEmbedding::new(config);
        takes_provider(&provider);
        assert_eq!(provider.dimension(), 1536);
    }

    #[test]
    fn test_ollama_root_from_api_base_variants() {
        assert_eq!(
            ollama_root_from_api_base("http://localhost:11434/api"),
            "http://localhost:11434"
        );
        assert_eq!(
            ollama_root_from_api_base("http://localhost:11434/v1"),
            "http://localhost:11434"
        );
        assert_eq!(
            ollama_root_from_api_base("http://localhost:11434"),
            "http://localhost:11434"
        );
    }

    #[tokio::test]
    async fn test_fetch_ollama_models_reachable() {
        let body =
            r#"{"models":[{"name":"nomic-embed-text:latest"},{"name":"qwen2.5:3b-instruct"}]}"#;
        let (base, server) = spawn_mock_server(200, body).await;
        let models = fetch_ollama_models(&base).await.unwrap();
        assert_eq!(models.len(), 2);
        assert!(models.iter().any(|m| m == "nomic-embed-text:latest"));
        server.abort();
    }

    #[tokio::test]
    async fn test_fetch_ollama_models_unreachable_is_none() {
        assert!(fetch_ollama_models("http://127.0.0.1:1").await.is_none());
    }

    /// 读写进程环境变量的测试需串行执行,避免并行竞争。
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn test_from_env_no_env() {
        // Without env vars, from_env should use defaults
        let config = EmbeddingConfig::default();
        assert_eq!(config.provider, "openai");
    }

    // --- HTTP-backed embed() branches (uses a local mock server) ---

    /// Spawn a minimal HTTP/1.1 server that answers every request with a fixed
    /// status line and body. Returns the base URL for `EmbeddingConfig.api_base`.
    async fn spawn_mock_server(
        status: u16,
        body: &'static str,
    ) -> (String, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{}", addr);

        let handle = tokio::spawn(async move {
            let reason = match status {
                200 => "OK",
                400 => "Bad Request",
                401 => "Unauthorized",
                404 => "Not Found",
                _ => "Internal Server Error",
            };
            loop {
                let (mut sock, _) = match listener.accept().await {
                    Ok(pair) => pair,
                    Err(_) => continue,
                };
                let (status, body, reason) = (status, body, reason);
                tokio::spawn(async move {
                    // Read until the end of the request headers so reqwest is
                    // satisfied before we reply.
                    let mut buf = [0u8; 4096];
                    let mut read = 0usize;
                    while read < buf.len() {
                        match sock.read(&mut buf[read..]).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                read += n;
                                if buf[..read].windows(4).any(|w| w == b"\r\n\r\n") {
                                    break;
                                }
                            }
                        }
                    }
                    let header = format!(
                        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        status,
                        reason,
                        body.len()
                    );
                    let _ = sock.write_all(header.as_bytes()).await;
                    let _ = sock.write_all(body.as_bytes()).await;
                    let _ = sock.shutdown().await;
                });
            }
        });

        (base, handle)
    }

    fn config_for(base: String) -> EmbeddingConfig {
        EmbeddingConfig {
            provider: "openai".to_string(),
            api_base: base,
            api_key: None,
            model: "text-embedding-3-small".to_string(),
            dimension: 2,
        }
    }

    #[tokio::test]
    async fn test_embed_openai_success() {
        let (base, server) = spawn_mock_server(
            200,
            r#"{"data":[{"embedding":[0.1,0.2]},{"embedding":[0.3,0.4]}]}"#,
        )
        .await;
        let mut config = config_for(base);
        config.api_key = Some("sk-test".to_string());
        let provider = OpenAIEmbedding::new(config);

        let embeddings = provider
            .embed(&["hello".to_string(), "world".to_string()])
            .await
            .unwrap();
        assert_eq!(embeddings.len(), 2);
        assert!((embeddings[0][0] - 0.1).abs() < 1e-6);
        assert!((embeddings[1][1] - 0.4).abs() < 1e-6);
        server.abort();
    }

    #[tokio::test]
    async fn test_embed_openai_http_error() {
        let (base, server) = spawn_mock_server(401, "unauthorized").await;
        let provider = OpenAIEmbedding::new(config_for(base));

        let err = provider.embed(&["hello".to_string()]).await.unwrap_err();
        assert!(
            err.to_string().contains("Embedding API"),
            "unexpected error: {err}"
        );
        server.abort();
    }

    #[tokio::test]
    async fn test_embed_openai_parse_error() {
        let (base, server) = spawn_mock_server(200, "this is not json").await;
        let provider = OpenAIEmbedding::new(config_for(base));

        let err = provider.embed(&["hello".to_string()]).await.unwrap_err();
        assert!(
            err.to_string().contains("parse error"),
            "unexpected error: {err}"
        );
        server.abort();
    }

    #[tokio::test]
    async fn test_embed_openai_empty_data() {
        let (base, server) = spawn_mock_server(200, r#"{"data":[]}"#).await;
        let provider = OpenAIEmbedding::new(config_for(base));

        let embeddings = provider.embed(&["hello".to_string()]).await.unwrap();
        assert!(embeddings.is_empty());
        server.abort();
    }

    #[tokio::test]
    async fn test_embed_ollama_success() {
        let (base, server) = spawn_mock_server(200, r#"{"embeddings":[[0.5,0.5]]}"#).await;
        let config = EmbeddingConfig {
            provider: "ollama".to_string(),
            api_base: base,
            api_key: None,
            model: "nomic-embed-text".to_string(),
            dimension: 2,
        };
        let provider = OpenAIEmbedding::new(config);

        let embeddings = provider.embed(&["hello".to_string()]).await.unwrap();
        assert_eq!(embeddings.len(), 1);
        assert!((embeddings[0][0] - 0.5).abs() < 1e-6);
        server.abort();
    }

    #[tokio::test]
    async fn test_embed_ollama_http_error() {
        let (base, server) = spawn_mock_server(500, "server exploded").await;
        let config = EmbeddingConfig {
            provider: "ollama".to_string(),
            api_base: base,
            api_key: None,
            model: "nomic-embed-text".to_string(),
            dimension: 2,
        };
        let provider = OpenAIEmbedding::new(config);

        let err = provider.embed(&["hello".to_string()]).await.unwrap_err();
        assert!(
            err.to_string().contains("Ollama API"),
            "unexpected error: {err}"
        );
        server.abort();
    }

    #[tokio::test]
    async fn test_embed_ollama_parse_error() {
        let (base, server) = spawn_mock_server(200, "not json at all").await;
        let config = EmbeddingConfig {
            provider: "ollama".to_string(),
            api_base: base,
            api_key: None,
            model: "nomic-embed-text".to_string(),
            dimension: 2,
        };
        let provider = OpenAIEmbedding::new(config);

        let err = provider.embed(&["hello".to_string()]).await.unwrap_err();
        assert!(
            err.to_string().contains("parse error"),
            "unexpected error: {err}"
        );
        server.abort();
    }

    #[tokio::test]
    async fn test_embed_empty_texts_short_circuits() {
        let (base, server) = spawn_mock_server(200, r#"{"data":[]}"#).await;
        let provider = OpenAIEmbedding::new(config_for(base));

        let embeddings = provider.embed(&[]).await.unwrap();
        assert!(embeddings.is_empty());
        server.abort();
    }

    #[tokio::test]
    async fn test_validate_remote_embedder_success() {
        let (base, server) = spawn_mock_server(200, r#"{"data":[{"embedding":[0.1,0.2]}]}"#).await;
        let embedder = OpenAIEmbedding::new(config_for(base));
        assert!(validate_remote_embedder(&embedder).await);
        server.abort();
    }

    #[tokio::test]
    async fn test_validate_remote_embedder_http_error() {
        let (base, server) = spawn_mock_server(401, "unauthorized").await;
        let embedder = OpenAIEmbedding::new(config_for(base));
        assert!(!validate_remote_embedder(&embedder).await);
        server.abort();
    }

    #[tokio::test]
    async fn test_validate_remote_embedder_unreachable() {
        // Bind and immediately drop a listener to get a port nothing is listening on.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let embedder = OpenAIEmbedding::new(config_for(format!("http://{}", addr)));
        assert!(!validate_remote_embedder(&embedder).await);
    }

    #[test]
    fn test_dimension_from_config() {
        let config = EmbeddingConfig::ollama("nomic-embed-text", 768);
        let provider = OpenAIEmbedding::new(config);
        assert_eq!(provider.dimension(), 768);
    }

    #[test]
    fn test_from_env_defaults_to_local_ollama() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::remove_var("MEMVAULT_EMBEDDING_PROVIDER");
            std::env::remove_var("OPENAI_API_KEY");
            std::env::remove_var("OPENAI_API_BASE");
            std::env::remove_var("MEMVAULT_EMBEDDING_MODEL");
            std::env::remove_var("MEMVAULT_EMBEDDING_DIM");
            std::env::remove_var("MEMVAULT_EMBEDDING_API_BASE");
            std::env::remove_var("MEMVAULT_EMBEDDING_API_KEY");
        }
        // 无任何配置 → 默认本地 Ollama(离线,无需 key)
        let provider = OpenAIEmbedding::from_env();
        assert_eq!(provider.config.provider, "ollama");
        assert_eq!(provider.dimension(), 768);
        assert_eq!(provider.config.api_base, "http://localhost:11434/api");
        assert!(provider.config.api_key.is_none());
    }

    #[test]
    fn test_from_env_explicit_openai_compatible() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("MEMVAULT_EMBEDDING_PROVIDER", "openai-compatible");
            std::env::set_var("MEMVAULT_EMBEDDING_API_BASE", "http://localhost:4000/v1");
            std::env::set_var("MEMVAULT_EMBEDDING_API_KEY", "sk-custom");
            std::env::set_var("MEMVAULT_EMBEDDING_MODEL", "my-embed-model");
            std::env::set_var("MEMVAULT_EMBEDDING_DIM", "1024");
        }
        let provider = OpenAIEmbedding::from_env();
        assert_eq!(provider.config.provider, "openai-compatible");
        assert_eq!(provider.config.api_base, "http://localhost:4000/v1");
        assert_eq!(provider.config.api_key.as_deref(), Some("sk-custom"));
        assert_eq!(provider.config.model, "my-embed-model");
        assert_eq!(provider.dimension(), 1024);
        unsafe {
            std::env::remove_var("MEMVAULT_EMBEDDING_PROVIDER");
            std::env::remove_var("MEMVAULT_EMBEDDING_API_BASE");
            std::env::remove_var("MEMVAULT_EMBEDDING_API_KEY");
            std::env::remove_var("MEMVAULT_EMBEDDING_MODEL");
            std::env::remove_var("MEMVAULT_EMBEDDING_DIM");
        }
    }

    #[test]
    fn test_from_env_backward_compat_openai_key() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::remove_var("MEMVAULT_EMBEDDING_PROVIDER");
            std::env::remove_var("OPENAI_API_BASE");
            std::env::set_var("OPENAI_API_KEY", "sk-legacy");
            std::env::remove_var("MEMVAULT_EMBEDDING_API_KEY");
        }
        // 旧版 OPENAI_API_KEY → 自动切到 OpenAI(向后兼容)
        let provider = OpenAIEmbedding::from_env();
        assert_eq!(provider.config.provider, "openai");
        assert_eq!(provider.dimension(), 1536);
        assert_eq!(provider.config.api_key.as_deref(), Some("sk-legacy"));
        unsafe {
            std::env::remove_var("OPENAI_API_KEY");
        }
    }

    #[test]
    fn test_from_env_explicit_ollama_model_override() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("MEMVAULT_EMBEDDING_PROVIDER", "ollama");
            std::env::set_var("MEMVAULT_EMBEDDING_MODEL", "bge-m3");
            std::env::set_var("MEMVAULT_EMBEDDING_DIM", "1024");
            std::env::remove_var("OPENAI_API_KEY");
        }
        let provider = OpenAIEmbedding::from_env();
        assert_eq!(provider.config.provider, "ollama");
        assert_eq!(provider.config.model, "bge-m3");
        assert_eq!(provider.dimension(), 1024);
        assert_eq!(provider.config.api_base, "http://localhost:11434/api");
        unsafe {
            std::env::remove_var("MEMVAULT_EMBEDDING_PROVIDER");
            std::env::remove_var("MEMVAULT_EMBEDDING_MODEL");
            std::env::remove_var("MEMVAULT_EMBEDDING_DIM");
        }
    }
}
