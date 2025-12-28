use crate::config::RerankerConfig;
use anyhow::{Context, Result};
use log::debug;
use serde::{Deserialize, Serialize};

pub struct RerankerClient {
    config: RerankerConfig,
    client: reqwest::Client,
}

#[derive(Serialize)]
struct RerankRequest {
    model: String,
    query: String,
    documents: Vec<String>,
}

#[derive(Deserialize, Debug)]
struct RerankResult {
    index: usize,
    relevance_score: f32,
}

#[derive(Deserialize, Debug)]
struct RerankResponse {
    results: Vec<RerankResult>,
}

impl RerankerClient {
    pub fn new(config: RerankerConfig) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .context("Failed to create HTTP client")?;

        Ok(Self { config, client })
    }

    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    pub fn top_n(&self) -> usize {
        self.config.top_n
    }

    /// Rerank documents by relevance to query.
    /// Returns Vec of (original_index, relevance_score) sorted by score descending.
    pub async fn rerank(&self, query: &str, documents: Vec<String>) -> Result<Vec<(usize, f32)>> {
        if documents.is_empty() {
            return Ok(vec![]);
        }

        debug!("rerank: sending {} documents to {}", documents.len(), self.config.endpoint);

        let request = RerankRequest {
            model: self.config.model.clone(),
            query: query.to_string(),
            documents,
        };

        let response = self.client
            .post(&self.config.endpoint)
            .json(&request)
            .send()
            .await
            .context("Failed to send rerank request")?;

        let status = response.status();
        debug!("rerank: received status {}", status);

        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            anyhow::bail!("Rerank request failed with status {}: {}", status, error_text);
        }

        let rerank_response: RerankResponse = response
            .json()
            .await
            .context("Failed to parse rerank response")?;

        debug!("rerank: got {} results", rerank_response.results.len());

        // Convert to (index, score) pairs, already sorted by score from the API
        let mut results: Vec<(usize, f32)> = rerank_response
            .results
            .into_iter()
            .map(|r| (r.index, r.relevance_score))
            .collect();

        // Sort by score descending (in case API doesn't)
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        Ok(results)
    }
}
