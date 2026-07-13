use crate::config::EmbeddingConfig;
use anyhow::{Context, Result};
use log::debug;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::time::{Duration, Instant};

/// Abstraction over embedding backends so indexing can be tested without a live API.
pub trait Embedder: Send + Sync {
    fn embed(
        &self,
        texts: Vec<String>,
    ) -> impl Future<Output = Result<Vec<Vec<f32>>>> + Send;
}

#[derive(Serialize)]
struct EmbeddingRequest {
    input: Vec<String>,
    model: String,
}

#[derive(Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingData>,
    usage: Usage,
}

#[derive(Deserialize)]
struct EmbeddingData {
    embedding: Vec<f32>,
    index: usize,
}

#[derive(Deserialize)]
struct Usage {
    prompt_tokens: usize,
    total_tokens: usize,
}

pub struct EmbeddingClient {
    client: Client,
    config: EmbeddingConfig,
}

impl EmbeddingClient {
    pub fn new(config: EmbeddingConfig) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(config.timeout_seconds))
            .build()
            .context("Failed to build reqwest client")?;

        Ok(Self { client, config })
    }

    pub async fn embed(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        let url = self.config.endpoint.clone();
        debug!(
            "embed: sending request to '{}' with model '{}'",
            url, self.config.model
        );
        debug!("embed: number of texts to embed: {}", texts.len());

        // Basic retry logic could be added here, simplified for POC
        let request_body = EmbeddingRequest {
            input: texts,
            model: self.config.model.clone(),
        };

        let mut req = self.client.post(&url).json(&request_body);

        if let Some(key) = &self.config.api_key {
            debug!("embed: using bearer auth");
            req = req.bearer_auth(key);
        }

        let start = Instant::now();
        let response = req
            .send()
            .await
            .context("Failed to send embedding request")?;
        let elapsed = start.elapsed();
        debug!(
            "embed: request sent, received status {} in {:?}",
            response.status(),
            elapsed
        );

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            debug!("embed: API error response body: {}", text);
            anyhow::bail!("Embedding API error: {} - {}", status, text);
        }

        let mut body: EmbeddingResponse = response
            .json()
            .await
            .context("Failed to parse embedding response")?;
        debug!("embed: got {} embeddings in response", body.data.len());
        debug!(
            "embed: prompt_tokens: {}, total_tokens: {}",
            body.usage.prompt_tokens, body.usage.total_tokens
        );

        // Ensure order is preserved
        body.data.sort_by_key(|d| d.index);

        let embeddings: Vec<Vec<f32>> = body.data.into_iter().map(|d| d.embedding).collect();
        debug!(
            "embed: returning {} embeddings, first dim: {}",
            embeddings.len(),
            embeddings.first().map(|e| e.len()).unwrap_or(0)
        );
        Ok(embeddings)
    }
}

impl Embedder for EmbeddingClient {
    fn embed(
        &self,
        texts: Vec<String>,
    ) -> impl Future<Output = Result<Vec<Vec<f32>>>> + Send {
        EmbeddingClient::embed(self, texts)
    }
}

/// Deterministic embedder for tests. Optionally fails after N successful calls.
#[cfg(test)]
pub struct FakeEmbedder {
    dim: usize,
    /// Fail on the call with this 0-based index (after this many prior successes).
    fail_at_call: Option<usize>,
    calls: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
impl FakeEmbedder {
    pub fn new(dim: usize) -> Self {
        Self {
            dim,
            fail_at_call: None,
            calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Fail starting at the given 0-based call index (0 = fail immediately).
    pub fn fail_at(dim: usize, call_index: usize) -> Self {
        Self {
            dim,
            fail_at_call: Some(call_index),
            calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

#[cfg(test)]
impl Embedder for FakeEmbedder {
    fn embed(
        &self,
        texts: Vec<String>,
    ) -> impl Future<Output = Result<Vec<Vec<f32>>>> + Send {
        use std::sync::atomic::Ordering;
        let dim = self.dim;
        let fail_at = self.fail_at_call;
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        async move {
            if fail_at == Some(call) {
                anyhow::bail!("fake embedder failure at call {call}");
            }
            Ok(texts
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    let mut v = vec![0.0_f32; dim];
                    // Distinct, stable vectors so cosine search can tell docs apart.
                    v[0] = (t.len() as f32) + (i as f32) * 0.01;
                    if dim > 1 {
                        v[1] = (call as f32) + 1.0;
                    }
                    v
                })
                .collect())
        }
    }
}
