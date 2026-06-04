//! LLM API client — OpenAI-compatible chat completions.
//!
//! Supports any OpenAI-compatible API:
//! - OpenAI (api.openai.com)
//! - OpenRouter (openrouter.ai/api/v1)
//! - Local vLLM / Ollama / llama.cpp
//! - Azure OpenAI
//! - Kimi / DeepSeek / Claude (via OpenRouter or native adapters)

use crate::models::{ChatResponse, FunctionCall, Message, ToolCall, ToolSchema};
use serde::Deserialize;
use anyhow::{anyhow, Context, Result};
use serde_json::json;
use std::collections::HashMap;
use std::time::Duration;

pub struct LlmClient {
    api_key: String,
    base_url: String,
    model: String,
    max_tokens: i32,
    temperature: f32,
    top_p: f32,
    max_retries: u32,
    extra_headers: HashMap<String, String>,
    client: reqwest::blocking::Client,
}

impl LlmClient {
    pub fn new(api_key: String, base_url: String, model: String) -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .expect("Failed to build HTTP client");
        
        Self {
            api_key,
            base_url,
            model,
            max_tokens: 4096,
            temperature: 0.7,
            top_p: 1.0,
            max_retries: 3,
            extra_headers: HashMap::new(),
            client,
        }
    }

    pub fn with_max_tokens(mut self, tokens: i32) -> Self {
        self.max_tokens = tokens;
        self
    }

    pub fn with_temperature(mut self, temp: f32) -> Self {
        self.temperature = temp;
        self
    }

    pub fn with_top_p(mut self, top_p: f32) -> Self {
        self.top_p = top_p;
        self
    }

    pub fn with_extra_headers(mut self, headers: HashMap<String, String>) -> Self {
        self.extra_headers = headers;
        self
    }

    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(secs))
            .build()
            .expect("Failed to rebuild HTTP client with timeout");
        self
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn chat(
        &self,
        messages: &[Message],
        tools: Option<&[ToolSchema]>,
    ) -> Result<(Message, Option<Usage>)> {
        // Convert tools to OpenAI format: {"type": "function", "function": {...}}
        let tools_for_api: Option<Vec<serde_json::Value>> = tools.map(|t| {
            t.iter().map(|schema| {
                json!({
                    "type": "function",
                    "function": {
                        "name": schema.name,
                        "description": schema.description,
                        "parameters": schema.parameters
                    }
                })
            }).collect()
        });

        let mut req_body = json!({
            "model": self.model,
            "messages": messages,
            "temperature": self.temperature,
            "top_p": self.top_p,
            "max_tokens": self.max_tokens
        });
        
        if let Some(tools) = tools_for_api {
            req_body["tools"] = json!(tools);
        }

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));

        let mut last_error = None;
        for attempt in 0..self.max_retries {
            match self.do_request(&url, &req_body) {
                Ok(resp) => {
                    let choice = resp.choices.into_iter().next()
                        .ok_or_else(|| anyhow!("Empty choices in response"))?;
                    
                    let mut msg = Message::assistant(choice.message.content.unwrap_or_default());
                    
                    // Handle reasoning content (DeepSeek, Kimi, etc.)
                    if let Some(reasoning) = choice.message.reasoning_content {
                        msg = msg.with_reasoning(reasoning);
                    }
                    
                    // Handle tool calls
                    if let Some(tool_calls) = choice.message.tool_calls {
                        msg = msg.with_tool_calls(tool_calls);
                    }
                    
                    return Ok((msg, resp.usage.map(|u| Usage {
                        prompt_tokens: u.prompt_tokens,
                        completion_tokens: u.completion_tokens,
                        total_tokens: u.total_tokens,
                    })));
                }
                Err(e) => {
                    log::warn!("LLM request attempt {}/{} failed: {}", attempt + 1, self.max_retries, e);
                    last_error = Some(e);
                    if attempt < self.max_retries - 1 {
                        std::thread::sleep(Duration::from_millis(500 * (attempt + 1) as u64));
                    }
                }
            }
        }

        Err(last_error.unwrap_or_else(|| anyhow!("All retries failed")))
    }

    fn do_request(&self, url: &str, body: &serde_json::Value) -> Result<ChatResponse> {
        let mut request = self.client
            .post(url)
            .header("Content-Type", "application/json");
        
        // Auth: support Bearer token, empty key (local models), or custom headers
        if !self.api_key.is_empty() {
            request = request.header("Authorization", format!("Bearer {}", self.api_key));
        }
        
        // Extra headers (e.g., OpenRouter needs HTTP-Referer, X-Title)
        for (k, v) in &self.extra_headers {
            request = request.header(k, v);
        }
        
        let resp = request
            .json(body)
            .send()
            .with_context(|| format!("Failed to send request to {}", url))?;

        let status = resp.status();
        if status.is_client_error() || status.is_server_error() {
            let err_text = resp.text().unwrap_or_default();
            return Err(anyhow!("HTTP {}: {}", status, err_text));
        }

        let chat_resp: ChatResponse = resp.json()
            .with_context(|| "Failed to parse LLM response JSON")?;
        Ok(chat_resp)
    }

    // ---------------------------------------------------------------------------
    // SSE Streaming
    // ---------------------------------------------------------------------------

    /// Stream chat completion token-by-token.
    /// `on_token` is called for each content delta.
    pub fn chat_with_callback(
        &self,
        messages: &[Message],
        tools: Option<&[ToolSchema]>,
        on_token: &mut dyn FnMut(&str),
    ) -> Result<(Message, Option<Usage>)> {
        let tools_for_api: Option<Vec<serde_json::Value>> = tools.map(|t| {
            t.iter().map(|schema| {
                json!({
                    "type": "function",
                    "function": {
                        "name": schema.name,
                        "description": schema.description,
                        "parameters": schema.parameters
                    }
                })
            }).collect()
        });

        let mut req_body = json!({
            "model": self.model,
            "messages": messages,
            "temperature": self.temperature,
            "top_p": self.top_p,
            "max_tokens": self.max_tokens,
            "stream": true,
        });

        if let Some(tools) = tools_for_api {
            req_body["tools"] = json!(tools);
        }

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));

        let mut last_error = None;
        for attempt in 0..self.max_retries {
            match self.do_stream_request(&url, &req_body, on_token) {
                Ok(result) => return Ok(result),
                Err(e) => {
                    log::warn!("LLM stream request attempt {}/{} failed: {}", attempt + 1, self.max_retries, e);
                    last_error = Some(e);
                    if attempt < self.max_retries - 1 {
                        std::thread::sleep(Duration::from_millis(500 * (attempt + 1) as u64));
                    }
                }
            }
        }

        Err(last_error.unwrap_or_else(|| anyhow!("All stream retries failed")))
    }

    fn do_stream_request(
        &self,
        url: &str,
        body: &serde_json::Value,
        on_token: &mut dyn FnMut(&str),
    ) -> Result<(Message, Option<Usage>)> {
        let mut request = self.client
            .post(url)
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream");

        if !self.api_key.is_empty() {
            request = request.header("Authorization", format!("Bearer {}", self.api_key));
        }

        for (k, v) in &self.extra_headers {
            request = request.header(k, v);
        }

        let resp = request
            .json(body)
            .send()
            .with_context(|| format!("Failed to send stream request to {}", url))?;

        let status = resp.status();
        if status.is_client_error() || status.is_server_error() {
            let err_text = resp.text().unwrap_or_default();
            return Err(anyhow!("HTTP {}: {}", status, err_text));
        }

        let body_text = resp.text()
            .with_context(|| "Failed to read stream response body")?;

        let mut full_content = String::new();
        let mut full_reasoning = String::new();
        let mut partial_tool_calls: std::collections::HashMap<usize, PartialToolCall> = std::collections::HashMap::new();

        for line in body_text.lines() {
            let line = line.trim();
            if !line.starts_with("data:") {
                continue;
            }
            let data = line["data:".len()..].trim();
            if data == "[DONE]" || data.is_empty() {
                continue;
            }

            match serde_json::from_str::<SseChunk>(data) {
                Ok(chunk) => {
                    if let Some(choice) = chunk.choices.into_iter().next() {
                        let delta = choice.delta;
                        if let Some(content) = delta.content {
                            if !content.is_empty() {
                                full_content.push_str(&content);
                                on_token(&content);
                            }
                        }
                        if let Some(reasoning) = delta.reasoning_content {
                            full_reasoning.push_str(&reasoning);
                        }
                        if let Some(tcs) = delta.tool_calls {
                            for tc in tcs {
                                let entry = partial_tool_calls.entry(tc.index)
                                    .or_insert_with(PartialToolCall::default);
                                if let Some(id) = tc.id { entry.id = id; }
                                if let Some(name) = tc.function.name { entry.name = name; }
                                if let Some(args) = tc.function.arguments {
                                    entry.arguments.push_str(&args);
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    log::debug!("Failed to parse SSE chunk: {} | error: {}", data, e);
                }
            }
        }

        let mut msg = Message::assistant(full_content);
        if !full_reasoning.is_empty() {
            msg = msg.with_reasoning(full_reasoning);
        }

        if !partial_tool_calls.is_empty() {
            let mut final_tool_calls: Vec<ToolCall> = partial_tool_calls
                .into_iter()
                .map(|(_idx, ptc)| ToolCall {
                    id: ptc.id,
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: ptc.name,
                        arguments: ptc.arguments,
                    },
                })
                .collect();
            final_tool_calls.sort_by(|a, b| a.id.cmp(&b.id));
            msg = msg.with_tool_calls(final_tool_calls);
        }

        Ok((msg, None))
    }
}

#[derive(Debug, Clone)]
pub struct Usage {
    pub prompt_tokens: i32,
    pub completion_tokens: i32,
    pub total_tokens: i32,
}

// ---------------------------------------------------------------------------
// SSE streaming types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct SseChunk {
    id: Option<String>,
    object: Option<String>,
    choices: Vec<SseChoice>,
}

#[derive(Debug, Deserialize)]
struct SseChoice {
    delta: SseDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct SseDelta {
    role: Option<String>,
    content: Option<String>,
    #[serde(default, rename = "reasoning_content")]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<SseToolCall>>,
}

#[derive(Debug, Deserialize)]
struct SseToolCall {
    index: usize,
    id: Option<String>,
    #[serde(rename = "type")]
    call_type: Option<String>,
    function: SseFunction,
}

#[derive(Debug, Deserialize, Default)]
struct SseFunction {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Default)]
struct PartialToolCall {
    id: String,
    name: String,
    arguments: String,
}
