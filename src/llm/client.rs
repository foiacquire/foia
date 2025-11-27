//! LLM client for document summarization and tagging.
//!
//! Supports Ollama API for local LLM inference.

#![allow(dead_code)]

use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

/// Default prompt for generating document synopsis.
pub const DEFAULT_SYNOPSIS_PROMPT: &str = r#"You are analyzing a FOIA (Freedom of Information Act) document. Based on the content below, provide a brief synopsis (2-3 sentences) summarizing the key information in this document.

Document Title: {title}

Document Content:
{content}

Respond with ONLY the synopsis, no additional text or formatting."#;

/// Default prompt for generating document tags.
pub const DEFAULT_TAGS_PROMPT: &str = r#"You are analyzing a FOIA (Freedom of Information Act) document. Generate 3-5 high-quality, REUSABLE tags that would help categorize this document alongside thousands of other FOIA documents.

Use this hierarchical format with prefixes:
- agency: for government agencies (e.g., agency:fbi, agency:doj, agency:state-dept, agency:local-police)
- topic: for broad subject areas (e.g., topic:surveillance, topic:immigration, topic:environment, topic:civil-rights, topic:public-safety)
- type: for document types (e.g., type:correspondence, type:report, type:memo, type:invoice, type:contract, type:complaint)
- entity: ONLY for major organizations or well-known people likely to appear in multiple documents (e.g., entity:aclu, entity:epa)

IMPORTANT RULES:
- Prefer BROAD, REUSABLE tags over specific one-off details
- Do NOT create tags for specific dates, case numbers, or minor details
- Do NOT create tags for obscure individuals or one-time events
- Choose tags that would likely match OTHER documents in a large FOIA archive
- Limit to 3-5 tags maximum

Document Title: {title}

Document Content:
{content}

Respond with ONLY a comma-separated list of tags. Example: agency:state-dept, topic:travel, type:correspondence"#;

/// Configuration for LLM client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    /// Whether LLM summarization is enabled
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Ollama API endpoint (default: http://localhost:11434)
    #[serde(default = "default_endpoint")]
    pub endpoint: String,
    /// Model to use for summarization (default: llama3.2:instruct)
    #[serde(default = "default_model")]
    pub model: String,
    /// Maximum tokens in response
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    /// Temperature for generation (0.0 - 1.0)
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    /// Custom prompt for synopsis generation (uses {title} and {content} placeholders)
    #[serde(default)]
    pub synopsis_prompt: Option<String>,
    /// Custom prompt for tag generation (uses {title} and {content} placeholders)
    #[serde(default)]
    pub tags_prompt: Option<String>,
    /// Maximum characters of document content to send to LLM
    #[serde(default = "default_max_content_chars")]
    pub max_content_chars: usize,
}

fn default_enabled() -> bool {
    true
}
fn default_endpoint() -> String {
    "http://localhost:11434".to_string()
}
fn default_model() -> String {
    "dolphin-llama3:8b".to_string()
}
fn default_max_tokens() -> u32 {
    512
}
fn default_temperature() -> f32 {
    0.3
}
fn default_max_content_chars() -> usize {
    12000
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            endpoint: default_endpoint(),
            model: default_model(),
            max_tokens: default_max_tokens(),
            temperature: default_temperature(),
            synopsis_prompt: None,
            tags_prompt: None,
            max_content_chars: default_max_content_chars(),
        }
    }
}

impl LlmConfig {
    pub fn with_endpoint(mut self, endpoint: &str) -> Self {
        self.endpoint = endpoint.to_string();
        self
    }

    pub fn with_model(mut self, model: &str) -> Self {
        self.model = model.to_string();
        self
    }

    /// Get the synopsis prompt, using custom or default.
    pub fn get_synopsis_prompt(&self) -> &str {
        self.synopsis_prompt
            .as_deref()
            .unwrap_or(DEFAULT_SYNOPSIS_PROMPT)
    }

    /// Get the tags prompt, using custom or default.
    pub fn get_tags_prompt(&self) -> &str {
        self.tags_prompt.as_deref().unwrap_or(DEFAULT_TAGS_PROMPT)
    }
}

/// Result of summarizing a document.
#[derive(Debug, Clone)]
pub struct SummarizeResult {
    /// Brief synopsis of the document content.
    pub synopsis: String,
    /// List of tags describing the document.
    pub tags: Vec<String>,
}

/// LLM client for document processing.
pub struct LlmClient {
    config: LlmConfig,
    client: Client,
}

/// Ollama API request format.
#[derive(Debug, Serialize)]
struct OllamaRequest {
    model: String,
    prompt: String,
    stream: bool,
    options: OllamaOptions,
}

#[derive(Debug, Serialize)]
struct OllamaOptions {
    temperature: f32,
    num_predict: u32,
}

/// Ollama API response format.
#[derive(Debug, Deserialize)]
struct OllamaResponse {
    response: String,
    #[allow(dead_code)]
    done: bool,
}

impl LlmClient {
    /// Create a new LLM client with the given configuration.
    pub fn new(config: LlmConfig) -> Self {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(300)) // 5 min timeout for slow models
            .build()
            .expect("Failed to create HTTP client");

        Self { config, client }
    }

    /// Get the config.
    pub fn config(&self) -> &LlmConfig {
        &self.config
    }

    /// Check if the LLM service is available.
    pub async fn is_available(&self) -> bool {
        if !self.config.enabled {
            return false;
        }
        let url = format!("{}/api/tags", self.config.endpoint);
        match self.client.get(&url).send().await {
            Ok(resp) => resp.status().is_success(),
            Err(_) => false,
        }
    }

    /// List available models.
    pub async fn list_models(&self) -> Result<Vec<String>, LlmError> {
        let url = format!("{}/api/tags", self.config.endpoint);
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| LlmError::Connection(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(LlmError::Api(format!("HTTP {}", resp.status())));
        }

        #[derive(Deserialize)]
        struct TagsResponse {
            models: Vec<ModelInfo>,
        }

        #[derive(Deserialize)]
        struct ModelInfo {
            name: String,
        }

        let tags: TagsResponse = resp
            .json()
            .await
            .map_err(|e| LlmError::Parse(e.to_string()))?;

        Ok(tags.models.into_iter().map(|m| m.name).collect())
    }

    /// Generate synopsis for a document.
    pub async fn generate_synopsis(&self, text: &str, title: &str) -> Result<String, LlmError> {
        let truncated = self.truncate_content(text);
        let prompt = self
            .config
            .get_synopsis_prompt()
            .replace("{title}", title)
            .replace("{content}", truncated);

        debug!("Generating synopsis for: {}", title);
        let response = self.call_ollama(&prompt).await?;

        // Clean up the response
        let synopsis = response.trim().to_string();
        if synopsis.is_empty() {
            return Err(LlmError::Parse("Empty synopsis response".to_string()));
        }

        Ok(synopsis)
    }

    /// Generate tags for a document.
    pub async fn generate_tags(&self, text: &str, title: &str) -> Result<Vec<String>, LlmError> {
        let truncated = self.truncate_content(text);
        let prompt = self
            .config
            .get_tags_prompt()
            .replace("{title}", title)
            .replace("{content}", truncated);

        debug!("Generating tags for: {}", title);
        let response = self.call_ollama(&prompt).await?;

        // Parse tags from response
        let tags = self.parse_tags(&response);
        if tags.is_empty() {
            return Err(LlmError::Parse("No tags parsed from response".to_string()));
        }

        Ok(tags)
    }

    /// Summarize a document (generates both synopsis and tags sequentially).
    pub async fn summarize(&self, text: &str, title: &str) -> Result<SummarizeResult, LlmError> {
        info!("Summarizing document: {}", title);

        // Run synopsis and tags generation sequentially to avoid memory pressure
        let synopsis = self.generate_synopsis(text, title).await?;
        let tags = self.generate_tags(text, title).await?;

        Ok(SummarizeResult { synopsis, tags })
    }

    /// Expand search terms using LLM to generate related terms.
    /// Takes seed terms and a domain description, returns expanded list.
    pub async fn expand_search_terms(
        &self,
        seed_terms: &[String],
        domain: &str,
    ) -> Result<Vec<String>, LlmError> {
        if seed_terms.is_empty() {
            return Ok(Vec::new());
        }

        let seeds = seed_terms.join(", ");
        let prompt = format!(
            r#"You are helping to expand search terms for finding declassified government documents related to: {domain}

Given these seed search terms: {seeds}

Generate an exhaustive comma-separated list of related search terms that would help find more relevant documents. Include:
- Synonyms and alternative phrasings
- Related programs, operations, or projects
- Key people, agencies, or organizations involved
- Related events, locations, or time periods
- Technical terms and code names
- Broader and narrower terms

Focus on terms specifically relevant to {domain}. Return ONLY a comma-separated list of terms, no explanations or categories. Aim for 50-100 terms."#,
            domain = domain,
            seeds = seeds
        );

        debug!("Expanding search terms for: {}", domain);
        let response = self.call_ollama(&prompt).await?;

        // Parse the response into individual terms
        let expanded: Vec<String> = response
            .split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty() && t.len() <= 100)
            .collect();

        info!(
            "Expanded {} seed terms to {} total terms",
            seed_terms.len(),
            expanded.len()
        );
        Ok(expanded)
    }

    /// Truncate content to configured maximum (UTF-8 safe).
    fn truncate_content<'a>(&self, text: &'a str) -> &'a str {
        if text.len() <= self.config.max_content_chars {
            return text;
        }
        // Find a valid UTF-8 boundary at or before max_content_chars
        let mut end = self.config.max_content_chars;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        &text[..end]
    }

    /// Call Ollama API with a prompt.
    async fn call_ollama(&self, prompt: &str) -> Result<String, LlmError> {
        let request = OllamaRequest {
            model: self.config.model.clone(),
            prompt: prompt.to_string(),
            stream: false,
            options: OllamaOptions {
                temperature: self.config.temperature,
                num_predict: self.config.max_tokens,
            },
        };

        let url = format!("{}/api/generate", self.config.endpoint);
        let resp = self
            .client
            .post(&url)
            .json(&request)
            .send()
            .await
            .map_err(|e| LlmError::Connection(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(LlmError::Api(format!("HTTP {}: {}", status, body)));
        }

        let ollama_resp: OllamaResponse = resp
            .json()
            .await
            .map_err(|e| LlmError::Parse(e.to_string()))?;

        Ok(ollama_resp.response)
    }

    /// Parse tags from LLM response.
    fn parse_tags(&self, response: &str) -> Vec<String> {
        // Remove common prefixes/formatting
        let cleaned = response
            .trim()
            .trim_start_matches("Tags:")
            .trim_start_matches("TAGS:")
            .trim_start_matches('[')
            .trim_end_matches(']')
            .trim();

        cleaned
            .split(',')
            .map(|t| {
                t.trim()
                    .to_lowercase()
                    // Allow colons for hierarchical tags (agency:fbi, topic:surveillance)
                    .trim_matches(|c: char| {
                        !c.is_alphanumeric() && c != '-' && c != '_' && c != ':'
                    })
                    .to_string()
            })
            .filter(|t| !t.is_empty() && t.len() <= 50)
            .take(10) // Max 10 tags
            .collect()
    }
}

/// Errors that can occur during LLM operations.
#[derive(Debug)]
pub enum LlmError {
    /// Failed to connect to LLM service
    Connection(String),
    /// API returned an error
    Api(String),
    /// Failed to parse response
    Parse(String),
    /// Model not available
    ModelNotFound(String),
    /// LLM is disabled
    Disabled,
}

impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LlmError::Connection(msg) => write!(f, "Connection error: {}", msg),
            LlmError::Api(msg) => write!(f, "API error: {}", msg),
            LlmError::Parse(msg) => write!(f, "Parse error: {}", msg),
            LlmError::ModelNotFound(msg) => write!(f, "Model not found: {}", msg),
            LlmError::Disabled => write!(f, "LLM is disabled"),
        }
    }
}

impl std::error::Error for LlmError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_tags() {
        let client = LlmClient::new(LlmConfig::default());

        // Simple comma-separated
        let tags = client.parse_tags("budget, policy, environmental, 2023");
        assert_eq!(tags, vec!["budget", "policy", "environmental", "2023"]);

        // With brackets
        let tags = client.parse_tags("[budget, policy, environmental]");
        assert_eq!(tags, vec!["budget", "policy", "environmental"]);

        // With prefix
        let tags = client.parse_tags("Tags: budget, policy, memo");
        assert_eq!(tags, vec!["budget", "policy", "memo"]);

        // Mixed case
        let tags = client.parse_tags("Budget, POLICY, Environmental");
        assert_eq!(tags, vec!["budget", "policy", "environmental"]);

        // Hierarchical tags with colons
        let tags = client.parse_tags("agency:fbi, topic:surveillance, type:memo");
        assert_eq!(tags, vec!["agency:fbi", "topic:surveillance", "type:memo"]);

        // Mixed hierarchical and simple
        let tags = client.parse_tags("agency:state-dept, topic:travel, type:correspondence");
        assert_eq!(
            tags,
            vec!["agency:state-dept", "topic:travel", "type:correspondence"]
        );
    }

    #[test]
    fn test_default_config() {
        let config = LlmConfig::default();
        assert!(config.enabled);
        assert!(config.model.contains("dolphin"));
        assert!(config.synopsis_prompt.is_none());
        assert!(config.get_synopsis_prompt().contains("{title}"));
    }
}
