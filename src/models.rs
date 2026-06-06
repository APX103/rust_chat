//! Shared data models for the mini-agent.
//!
//! Uses OpenAI Chat Completions format as the internal lingua franca,
//! matching Hermes Agent's design philosophy.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: MessageRole,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Internal reasoning content (not sent to API)
    #[serde(skip_serializing)]
    pub reasoning: Option<String>,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::System,
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            reasoning: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::User,
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            reasoning: None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::Assistant,
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            reasoning: None,
        }
    }

    pub fn tool(tool_call_id: impl Into<String>, name: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::Tool,
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
            name: Some(name.into()),
            reasoning: None,
        }
    }

    pub fn with_reasoning(mut self, reasoning: impl Into<String>) -> Self {
        self.reasoning = Some(reasoning.into());
        self
    }

    pub fn with_tool_calls(mut self, tool_calls: Vec<ToolCall>) -> Self {
        self.tool_calls = Some(tool_calls);
        self
    }
}

// ---------------------------------------------------------------------------
// Tool definitions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone)]
pub struct ToolResult {
    pub tool_call_id: String,
    pub name: String,
    pub content: String,
    pub is_error: bool,
}

pub type ToolHandler = Arc<dyn Fn(&str, &serde_json::Value) -> anyhow::Result<String> + Send + Sync>;

pub struct Tool {
    pub schema: ToolSchema,
    pub handler: ToolHandler,
    pub source: ToolSource,
}

impl std::fmt::Debug for Tool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tool")
            .field("schema", &self.schema)
            .field("source", &self.source)
            .field("handler", &"<fn>")
            .finish()
    }
}

impl Clone for Tool {
    fn clone(&self) -> Self {
        Self {
            schema: self.schema.clone(),
            handler: Arc::clone(&self.handler),
            source: self.source.clone(),
        }
    }
}

// Implement the new Tool trait for the legacy Tool struct.
#[async_trait]
impl crate::tool::Tool for Tool {
    fn name(&self) -> &str {
        &self.schema.name
    }

    fn description(&self) -> &str {
        &self.schema.description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.schema.parameters.clone()
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<crate::tool::ToolOutput> {
        let result = (self.handler)(&self.schema.name, &args)?;
        Ok(crate::tool::ToolOutput::ok(result))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ToolSource {
    Builtin,
    Mcp { server: String },
    Skill { skill: String },
}

// ---------------------------------------------------------------------------
// LLM API types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolSchema>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<i32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChatResponse {
    pub id: Option<String>,
    pub model: Option<String>,
    pub choices: Vec<Choice>,
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Choice {
    pub index: i32,
    pub message: ResponseMessage,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResponseMessage {
    pub role: String,
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Usage {
    pub prompt_tokens: i32,
    pub completion_tokens: i32,
    pub total_tokens: i32,
}

// ---------------------------------------------------------------------------
// Config types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ObserverConfig {
    #[serde(default = "default_observer_enabled")]
    pub enabled: bool,
    #[serde(default = "default_observer_kind")]
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HeartbeatConfig {
    #[serde(default = "default_heartbeat_enabled")]
    pub enabled: bool,
    #[serde(default = "default_heartbeat_interval")]
    pub interval_secs: u64,
    #[serde(default)]
    pub tasks: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub model: ModelConfig,
    pub memory: MemoryConfig,
    pub mcp_servers: HashMap<String, McpServerConfig>,
    #[serde(default)]
    pub skills: SkillsConfig,
    pub agent: AgentBehaviorConfig,
    #[serde(default)]
    pub observer: ObserverConfig,
    #[serde(default)]
    pub heartbeat: HeartbeatConfig,
    #[serde(default)]
    pub file_memory: FileMemoryConfig,
    #[serde(default)]
    pub compression: CompressionConfig,
    #[serde(default)]
    pub review: ReviewConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: i32,
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    #[serde(default = "default_top_p")]
    pub top_p: f32,
    #[serde(default)]
    pub extra_headers: HashMap<String, String>,
    #[serde(default = "default_timeout")]
    pub timeout: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConfig {
    #[serde(default = "default_memory_enabled")]
    pub enabled: bool,
    #[serde(default = "default_semantic_search_top_k")]
    pub semantic_search_top_k: usize,
    #[serde(default = "default_episodic_summary_threshold")]
    pub episodic_summary_threshold: usize,
    #[serde(default = "default_memory_provider")]
    pub provider: String,
    #[serde(default = "default_hybrid_search")]
    pub hybrid_search: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileMemoryConfig {
    #[serde(default = "default_file_memory_enabled")]
    pub enabled: bool,
    #[serde(default = "default_memory_char_limit")]
    pub memory_char_limit: usize,
    #[serde(default = "default_user_char_limit")]
    pub user_char_limit: usize,
}

impl Default for FileMemoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            memory_char_limit: 2200,
            user_char_limit: 1375,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressionConfig {
    #[serde(default = "default_compression_enabled")]
    pub enabled: bool,
    #[serde(default = "default_compression_threshold")]
    pub threshold_percent: f64,
    #[serde(default = "default_compression_protect_first")]
    pub protect_first_n: usize,
    #[serde(default = "default_compression_protect_last")]
    pub protect_last_n: usize,
    #[serde(default = "default_compression_summary_ratio")]
    pub summary_target_ratio: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewConfig {
    #[serde(default = "default_review_enabled")]
    pub enabled: bool,
    #[serde(default = "default_review_interval")]
    pub interval: usize,
    #[serde(default = "default_review_window_size")]
    pub window_size: usize,
    #[serde(default = "default_review_max_tokens")]
    pub max_tokens: i32,
    #[serde(default)]
    pub model_override: Option<String>,
}

impl Default for CompressionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold_percent: 0.50,
            protect_first_n: 3,
            protect_last_n: 20,
            summary_target_ratio: 0.20,
        }
    }
}

impl Default for ReviewConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval: 10,
            window_size: 8,
            max_tokens: 2048,
            model_override: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default = "default_mcp_timeout")]
    pub timeout: u64,
    #[serde(default = "default_mcp_connect_timeout")]
    pub connect_timeout: u64,
    /// MCP transport type: "stdio" | "http" | "streamable-http" (default: inferred from command/url)
    #[serde(default, rename = "type")]
    pub transport_type: String,
}

impl McpServerConfig {
    pub fn is_streamable_http(&self) -> bool {
        self.transport_type.eq_ignore_ascii_case("streamable-http")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SkillsConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub auto_create: bool,
    #[serde(default)]
    pub external_dirs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentBehaviorConfig {
    #[serde(default = "default_max_iterations")]
    pub max_iterations: usize,
    #[serde(default = "default_enable_reasoning")]
    pub enable_reasoning: bool,
}

// ---------------------------------------------------------------------------
// Skill types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillManifest {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub triggers: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt_patch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_server: Option<McpServerConfig>,
    #[serde(default)]
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub usage_count: u64,
}

// ---------------------------------------------------------------------------
// Defaults
// ---------------------------------------------------------------------------

fn default_base_url() -> String {
    "https://api.openai.com/v1".to_string()
}

fn default_max_tokens() -> i32 {
    4096
}

fn default_temperature() -> f32 {
    0.7
}

fn default_memory_enabled() -> bool {
    true
}

fn default_semantic_search_top_k() -> usize {
    5
}

fn default_episodic_summary_threshold() -> usize {
    10
}

fn default_mcp_timeout() -> u64 {
    120
}

fn default_mcp_connect_timeout() -> u64 {
    60
}

fn default_max_iterations() -> usize {
    30
}

fn default_top_p() -> f32 {
    1.0
}

fn default_enable_reasoning() -> bool {
    true
}

fn default_timeout() -> u64 {
    120
}

fn default_memory_provider() -> String {
    "builtin".to_string()
}

fn default_hybrid_search() -> bool {
    true
}

fn default_observer_enabled() -> bool {
    true
}

fn default_observer_kind() -> String {
    "log".to_string()
}

fn default_heartbeat_enabled() -> bool {
    false
}

fn default_heartbeat_interval() -> u64 {
    3600
}

fn default_file_memory_enabled() -> bool {
    true
}

fn default_memory_char_limit() -> usize {
    2200
}

fn default_user_char_limit() -> usize {
    1375
}

fn default_compression_enabled() -> bool {
    true
}

fn default_compression_threshold() -> f64 {
    0.50
}

fn default_compression_protect_first() -> usize {
    3
}

fn default_compression_protect_last() -> usize {
    20
}

fn default_compression_summary_ratio() -> f64 {
    0.20
}

fn default_review_enabled() -> bool {
    true
}

fn default_review_interval() -> usize {
    10
}

fn default_review_window_size() -> usize {
    8
}

fn default_review_max_tokens() -> i32 {
    2048
}
