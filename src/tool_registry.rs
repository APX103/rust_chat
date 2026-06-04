//! Tool registry — inspired by Hermes' tools/registry.py.
//!
//! Central dispatch for all tools (builtin, MCP, skill-registered).

use crate::models::{Tool, ToolSchema, ToolSource};
use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::sync::Mutex;

pub struct ToolRegistry {
    tools: Mutex<HashMap<String, Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: Mutex::new(HashMap::new()),
        }
    }

    pub fn register_tool(
        &self,
        schema: ToolSchema,
        handler: crate::models::ToolHandler,
        source: ToolSource,
    ) {
        let name = schema.name.clone();
        let tool = Tool {
            schema,
            handler,
            source,
        };
        self.tools.lock().unwrap().insert(name, tool);
    }

    pub fn get_tool(&self, name: &str) -> Option<Tool> {
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
            .map(|t| t.schema.clone())
            .collect()
    }

    pub fn dispatch(&self, name: &str, args: &serde_json::Value) -> Result<String> {
        let tool = self
            .tools
            .lock()
            .unwrap()
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow!("Tool '{}' not found", name))?;
        
        (tool.handler)(name, args)
    }

    pub fn remove_tools_from_source(&self, source: &ToolSource) {
        let mut tools = self.tools.lock().unwrap();
        tools.retain(|_k, v| &v.source != source);
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


