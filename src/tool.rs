//! Tool trait — abstraction layer for all tool implementations.
//!
//! Provides a unified interface for builtin tools, MCP tools, and
//! closure-based tools. All tools return `ToolOutput` for structured results.

use async_trait::async_trait;
use serde::Serialize;
use std::sync::Arc;

/// Structured tool execution result.
#[derive(Debug, Clone, Serialize)]
pub struct ToolOutput {
    pub success: bool,
    pub text: String,
    pub error: Option<String>,
}

impl ToolOutput {
    pub fn ok(text: impl Into<String>) -> Self {
        Self {
            success: true,
            text: text.into(),
            error: None,
        }
    }

    pub fn err(text: impl Into<String>) -> Self {
        Self {
            success: false,
            text: String::new(),
            error: Some(text.into()),
        }
    }
}

/// Async tool trait — all tools must implement this.
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> serde_json::Value;

    /// Execute the tool with the given JSON arguments.
    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolOutput>;

    /// Return the schema for LLM consumption.
    fn schema(&self) -> crate::models::ToolSchema {
        crate::models::ToolSchema {
            name: self.name().to_string(),
            description: self.description().to_string(),
            parameters: self.parameters_schema(),
        }
    }
}

/// Adapter: wrap a synchronous closure as a `Tool` implementation.
pub struct ClosureTool {
    pub name: String,
    pub description: String,
    pub parameters_schema: serde_json::Value,
    pub handler: Arc<dyn Fn(&str, &serde_json::Value) -> anyhow::Result<String> + Send + Sync>,
}

#[async_trait]
impl Tool for ClosureTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.parameters_schema.clone()
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolOutput> {
        let result = (self.handler)(self.name.as_str(), &args)?;
        Ok(ToolOutput::ok(result))
    }
}
