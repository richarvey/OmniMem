//! The Anthropic Messages API, for the features that ask Claude Haiku one
//! question: fact extraction, query expansion, contradiction tier 2 and, in
//! the RSS port, article summaries.
//!
//! 6.x used the Anthropic Python SDK. This keeps the parts of its behaviour
//! the features relied on: two retries on connection failures, 408, 409, 429
//! and 5xx, honouring `retry-after`, and otherwise an exponential backoff
//! from half a second capped at eight. The SDK's ten-minute request timeout
//! is two minutes here; no Haiku reply these features ask for comes close.

use std::time::Duration;

use omnimem_core::{LanguageModel, LlmError};
use serde_json::{Value, json};
use thiserror::Error;
use tracing::{debug, warn};

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";
const PLACEHOLDER_KEY: &str = "your_key_here";

#[derive(Debug, Clone)]
pub struct AnthropicConfig {
    pub api_key: String,
    /// `ANTHROPIC_BASE_URL`, as the SDK honoured it.
    pub base_url: String,
    pub timeout: Duration,
    pub max_retries: u32,
}

impl AnthropicConfig {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_owned(),
            timeout: Duration::from_secs(120),
            max_retries: 2,
        }
    }

    /// From `ANTHROPIC_API_KEY`. `None` when no usable key is set, which
    /// includes the `.env.example` placeholder 6.x also ignored.
    pub fn from_env() -> Option<Self> {
        let key = std::env::var("ANTHROPIC_API_KEY").ok()?;
        let key = key.trim();
        if key.is_empty() || key == PLACEHOLDER_KEY {
            return None;
        }
        let mut config = Self::new(key);
        if let Ok(base) = std::env::var("ANTHROPIC_BASE_URL")
            && !base.trim().is_empty()
        {
            config.base_url = base.trim().trim_end_matches('/').to_owned();
        }
        Some(config)
    }
}

#[derive(Debug, Error)]
pub enum AnthropicError {
    #[error("could not build the HTTP client: {0}")]
    Client(String),
    #[error("request failed: {0}")]
    Http(String),
    #[error("Anthropic API returned {status}: {message}")]
    Status { status: u16, message: String },
    #[error("unexpected response: {0}")]
    Response(String),
}

pub struct AnthropicClient {
    config: AnthropicConfig,
    http: reqwest::blocking::Client,
}

/// One attempt's failure, and whether the SDK would have retried it.
struct Attempt {
    error: AnthropicError,
    retryable: bool,
    retry_after: Option<Duration>,
}

fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let millis = headers
        .get("retry-after-ms")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<f64>().ok())
        .map(|ms| ms / 1000.0);
    let seconds = headers
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<f64>().ok());
    millis
        .or(seconds)
        .filter(|s| s.is_finite() && (0.0..=60.0).contains(s))
        .map(Duration::from_secs_f64)
}

impl AnthropicClient {
    pub fn new(config: AnthropicConfig) -> Result<Self, AnthropicError> {
        let http = reqwest::blocking::Client::builder()
            .user_agent(concat!("omnimem/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(30))
            .timeout(config.timeout)
            .build()
            .map_err(|e| AnthropicError::Client(e.to_string()))?;
        Ok(Self { config, http })
    }

    fn attempt(&self, body: &Value) -> Result<String, Attempt> {
        let response = self
            .http
            .post(format!("{}/v1/messages", self.config.base_url))
            .header("x-api-key", &self.config.api_key)
            .header("anthropic-version", API_VERSION)
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
            .map_err(|e| Attempt {
                retryable: e.is_connect() || e.is_timeout() || e.is_request(),
                error: AnthropicError::Http(e.to_string()),
                retry_after: None,
            })?;
        let status = response.status();
        let headers = response.headers().clone();
        let text = response.text().map_err(|e| Attempt {
            retryable: true,
            error: AnthropicError::Http(e.to_string()),
            retry_after: None,
        })?;
        if !status.is_success() {
            let message = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v["error"]["message"].as_str().map(str::to_owned))
                .unwrap_or_else(|| text.chars().take(200).collect());
            let should_retry = headers
                .get("x-should-retry")
                .and_then(|v| v.to_str().ok())
                .map(|v| v == "true");
            let code = status.as_u16();
            return Err(Attempt {
                retryable: should_retry.unwrap_or(matches!(code, 408 | 409 | 429) || code >= 500),
                error: AnthropicError::Status {
                    status: code,
                    message,
                },
                retry_after: retry_after(&headers),
            });
        }
        let reply: Value = serde_json::from_str(&text).map_err(|e| Attempt {
            retryable: false,
            error: AnthropicError::Response(e.to_string()),
            retry_after: None,
        })?;
        // `message.content[0].text`, as 6.x read it.
        reply["content"][0]["text"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| Attempt {
                retryable: false,
                error: AnthropicError::Response("no text in the first content block".into()),
                retry_after: None,
            })
    }
}

impl LanguageModel for AnthropicClient {
    fn complete(&self, model: &str, prompt: &str, max_tokens: u32) -> Result<String, LlmError> {
        let body = json!({
            "model": model,
            "max_tokens": max_tokens,
            "messages": [{"role": "user", "content": prompt}],
        });
        let mut attempt = 0;
        loop {
            match self.attempt(&body) {
                Ok(text) => return Ok(text),
                Err(failed) if failed.retryable && attempt < self.config.max_retries => {
                    let backoff = failed.retry_after.unwrap_or_else(|| {
                        Duration::from_secs_f64((0.5 * 2f64.powi(attempt as i32)).min(8.0))
                    });
                    debug!(attempt, error = %failed.error, ?backoff, "retrying Anthropic request");
                    std::thread::sleep(backoff);
                    attempt += 1;
                }
                Err(failed) => {
                    warn!(model, error = %failed.error, "Anthropic request failed");
                    return Err(Box::new(failed.error));
                }
            }
        }
    }
}
