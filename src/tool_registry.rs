//! Tool registry — inspired by Hermes' tools/registry.py.
//!
//! Central dispatch for all tools (builtin, MCP, skill-registered).
//! Stores tools as `Arc<dyn Tool>` trait objects for polymorphic dispatch.

use crate::models::{Tool as LegacyTool, ToolSchema, ToolSource};
use crate::tool::{Tool, ToolOutput};
use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub struct ToolRegistry {
    tools: Arc<Mutex<HashMap<String, Arc<dyn Tool>>>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Register a tool from any type that implements the `Tool` trait.
    pub fn register_tool(&self, tool: Arc<dyn Tool>) {
        let name = tool.name().to_string();
        self.tools.lock().unwrap().insert(name, tool);
    }

    /// Legacy registration: build a ClosureTool from (schema, handler, source).
    /// This preserves backward compatibility with existing callers.
    pub fn register_tool_legacy(
        &self,
        schema: ToolSchema,
        handler: crate::models::ToolHandler,
        source: ToolSource,
    ) {
        let legacy = LegacyTool {
            schema,
            handler,
            source,
        };
        self.register_tool(Arc::new(legacy));
    }

    /// Get a tool by name (returns a clone of the Arc).
    pub fn get_tool(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.lock().unwrap().get(name).cloned()
    }

    pub fn has_tool(&self, name: &str) -> bool {
        self.tools.lock().unwrap().contains_key(name)
    }

    pub fn list_tools(&self) -> Vec<ToolSchema> {
        self.tools
            .lock()
            .unwrap()
            .values()
            .map(|t| t.schema())
            .collect()
    }

    /// Async dispatch: execute a tool by name with the given arguments.
    /// Blocking handlers are offloaded to tokio's blocking thread pool.
    pub async fn dispatch(&self, name: &str, args: &serde_json::Value) -> Result<ToolOutput> {
        let tool = self
            .tools
            .lock()
            .unwrap()
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow!("Tool '{}' not found", name))?;

        tool.execute(args.clone()).await
    }

    pub fn remove_tools_from_source(&self, source: &ToolSource) {
        let mut tools = self.tools.lock().unwrap();
        tools.retain(|_k, v| {
            // For Arc<dyn Tool>, we check if the name matches the source pattern.
            // Legacy tools store source info; trait-only tools keep their name.
            // Since we can't easily get source from Arc<dyn Tool>, we compare by
            // looking up the legacy registry. For simplicity, we iterate and keep.
            true
        });
        // NOTE: Source-based removal requires tracking source in the trait or
        // maintaining a parallel index. Deferred to a future enhancement.
        let _ = source; // suppress unused warning until implemented
    }

    pub fn len(&self) -> usize {
        self.tools.lock().unwrap().len()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}
