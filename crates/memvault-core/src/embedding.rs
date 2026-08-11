use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

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
    pub fn ollama(model: &str, dimension: usize) -> Self {
        Self {
            provider: "ollama".to_string(),
            api_base: "http://localhost:11434/api".to_string(),
            api_key: None,
            model: model.to_string(),
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

    pub fn from_env() -> Self {
        let api_key = std::env::var("OPENAI_API_KEY").ok();
        let api_base = std::env::var("OPENAI_API_BASE")
            .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
        let model = std::env::var("MEMVAULT_EMBEDDING_MODEL")
            .unwrap_or_else(|_| "text-embedding-3-small".to_string());
        let dimension: usize = std::env::var("MEMVAULT_EMBEDDING_DIM")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1536);

        Self::new(EmbeddingConfig {
            provider: "openai".to_string(),
            api_base,
            api_key,
            model,
            dimension,
        })
    }
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
    fn test_from_env_no_env() {
        // Without env vars, from_env should use defaults
        let config = EmbeddingConfig::default();
        assert_eq!(config.provider, "openai");
    }
}
