use std::time::Duration;

use anyhow::{Context, bail};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::model::EmbeddingVector;

#[derive(Clone, Debug, Serialize)]
struct EmbedRequest {
    model_id: String,
    texts: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    normalize: Option<bool>,
}

#[derive(Clone, Debug, Deserialize)]
struct EmbedResponse {
    model_id: String,
    device: String,
    embeddings: Vec<Vec<f32>>,
}

#[async_trait]
pub trait Embedder: Send + Sync {
    async fn embed_texts(
        &self,
        model_id: &str,
        texts: &[String],
        normalize: bool,
    ) -> anyhow::Result<Vec<EmbeddingVector>>;
}

#[derive(Clone, Debug)]
pub struct HttpEmbedder {
    base_url: String,
    http: Client,
}

impl HttpEmbedder {
    pub fn new(base_url: impl Into<String>, timeout: Duration) -> anyhow::Result<Self> {
        let base_url = normalize_base_url(&base_url.into())?;
        let http = Client::builder()
            .timeout(timeout)
            .build()
            .context("failed to build embedder HTTP client")?;
        Ok(Self { base_url, http })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }
}

#[async_trait]
impl Embedder for HttpEmbedder {
    async fn embed_texts(
        &self,
        model_id: &str,
        texts: &[String],
        normalize: bool,
    ) -> anyhow::Result<Vec<EmbeddingVector>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let response = self
            .http
            .post(self.url("/v1/embed"))
            .json(&EmbedRequest {
                model_id: model_id.to_string(),
                texts: texts.to_vec(),
                normalize: Some(normalize),
            })
            .send()
            .await
            .with_context(|| "failed to call embedding service")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!("embedding service returned {status}: {body}");
        }

        let payload: EmbedResponse = response
            .json()
            .await
            .with_context(|| "failed to decode embedding response")?;

        if payload.embeddings.len() != texts.len() {
            bail!(
                "embedding service returned {} embeddings for {} texts",
                payload.embeddings.len(),
                texts.len()
            );
        }

        Ok(payload
            .embeddings
            .into_iter()
            .map(|values| EmbeddingVector {
                values,
                model: Some(payload.model_id.clone()),
                device: Some(payload.device.clone()),
                source: Some("ancilla-embedder".to_string()),
            })
            .collect())
    }
}

fn normalize_base_url(value: &str) -> anyhow::Result<String> {
    let trimmed = value.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        bail!("embedder base URL cannot be empty");
    }
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        Ok(trimmed.to_string())
    } else {
        Ok(format!("http://{trimmed}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_base_url() {
        assert_eq!(
            normalize_base_url("http://127.0.0.1:4000/").unwrap(),
            "http://127.0.0.1:4000"
        );
        assert_eq!(
            normalize_base_url("127.0.0.1:4000").unwrap(),
            "http://127.0.0.1:4000"
        );
    }
}
