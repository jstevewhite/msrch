use crate::config::EmbeddingConfig;
use anyhow::{Context, Result};
use log::debug;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

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
        debug!("embed: sending request to '{}' with model '{}'", url, self.config.model);
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
        let response = req.send().await.context("Failed to send embedding request")?;
        let elapsed = start.elapsed();
        debug!("embed: request sent, received status {} in {:?}", response.status(), elapsed);

        if !response.status().is_success() {
             let status = response.status();
             let text = response.text().await.unwrap_or_default();
             debug!("embed: API error response body: {}", text);
             anyhow::bail!("Embedding API error: {} - {}", status, text);
        }

        let mut body: EmbeddingResponse = response.json().await.context("Failed to parse embedding response")?;
        debug!("embed: got {} embeddings in response", body.data.len());
        debug!("embed: prompt_tokens: {}, total_tokens: {}", body.usage.prompt_tokens, body.usage.total_tokens);

        // Ensure order is preserved
        body.data.sort_by_key(|d| d.index);

        let embeddings: Vec<Vec<f32>> = body.data.into_iter().map(|d| d.embedding).collect();
        debug!("embed: returning {} embeddings, first dim: {}", embeddings.len(), embeddings.first().map(|e| e.len()).unwrap_or(0));
        Ok(embeddings)
    }
}
