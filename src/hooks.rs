//! Hook system — lifecycle hooks for tool calls and session/turn events.
//!
//! Provides two categories of hooks:
//! - **Void hooks** (fire-and-forget, run in parallel): session/turn lifecycle events
//! - **Modifying hooks** (serial by priority, can Cancel): before_tool_call, after_tool_result

use async_trait::async_trait;
use futures_util::FutureExt;
use serde_json::Value;
use std::time::Duration;

/// Result of a modifying hook — either continue with modified data, or cancel.
#[derive(Debug, Clone)]
pub enum HookResult<T> {
    Continue(T),
    Cancel(String),
}

impl<T> HookResult<T> {
    pub fn is_cancel(&self) -> bool {
        matches!(self, HookResult::Cancel(_))
    }
}

/// Hook handler trait — implement this to intercept agent lifecycle events.
///
/// Priority determines ordering for modifying hooks (higher = runs first).
/// Void hooks always fire in parallel regardless of priority.
#[async_trait]
pub trait HookHandler: Send + Sync {
    fn name(&self) -> &str;

    /// Higher priority runs first for modifying hooks. Default: 0.
    fn priority(&self) -> i32 {
        0
    }

    // ---- Void hooks (parallel, fire-and-forget) ----

    /// Called when a session starts.
    async fn on_session_start(&self, _session_id: &str) {}

    /// Called when a session ends.
    async fn on_session_end(&self, _session_id: &str) {}

    /// Called at the start of each agent turn (before LLM call).
    async fn on_turn_start(&self, _turn: usize, _message: &str) {}

    /// Called at the end of each agent turn (after final response).
    async fn on_turn_end(&self, _turn: usize, _response: &str) {}

    /// Called when a tool is invoked (before execution).
    async fn on_tool_call(&self, _name: &str, _args: &Value) {}

    /// Called when a tool result is available (after execution).
    async fn on_tool_result(&self, _name: &str, _success: bool, _duration: Duration) {}

    // ---- Modifying hooks (serial by priority, can Cancel) ----

    /// Called before tool execution. Can modify (name, args) or cancel.
    async fn before_tool_call(
        &self,
        _name: String,
        _args: Value,
    ) -> HookResult<(String, Value)> {
        HookResult::Continue((_name, _args))
    }

    /// Called after tool execution. Can modify output or cancel.
    async fn after_tool_result(
        &self,
        _name: String,
        _output: crate::tool::ToolOutput,
    ) -> HookResult<(String, crate::tool::ToolOutput)> {
        HookResult::Continue((_name, _output))
    }
}

/// Central hook dispatcher — manages registered handlers and fires events.
pub struct HookRunner {
    handlers: Vec<Box<dyn HookHandler>>,
}

impl HookRunner {
    pub fn new() -> Self {
        Self {
            handlers: Vec::new(),
        }
    }

    /// Register a new hook handler. Handlers are sorted by priority (descending).
    pub fn register(&mut self, handler: Box<dyn HookHandler>) {
        self.handlers.push(handler);
        self.handlers.sort_by_key(|h| std::cmp::Reverse(h.priority()));
    }

    pub fn is_empty(&self) -> bool {
        self.handlers.is_empty()
    }

    // ---- Void dispatchers (parallel fire-and-forget) ----

    pub async fn fire_session_start(&self, session_id: &str) {
        if self.handlers.is_empty() {
            return;
        }
        let futs: Vec<_> = self
            .handlers
            .iter()
            .map(|h| h.on_session_start(session_id))
            .collect();
        futures_util::future::join_all(futs).await;
    }

    pub async fn fire_session_end(&self, session_id: &str) {
        if self.handlers.is_empty() {
            return;
        }
        let futs: Vec<_> = self
            .handlers
            .iter()
            .map(|h| h.on_session_end(session_id))
            .collect();
        futures_util::future::join_all(futs).await;
    }

    pub async fn fire_turn_start(&self, turn: usize, message: &str) {
        if self.handlers.is_empty() {
            return;
        }
        let futs: Vec<_> = self
            .handlers
            .iter()
            .map(|h| h.on_turn_start(turn, message))
            .collect();
        futures_util::future::join_all(futs).await;
    }

    pub async fn fire_turn_end(&self, turn: usize, response: &str) {
        if self.handlers.is_empty() {
            return;
        }
        let futs: Vec<_> = self
            .handlers
            .iter()
            .map(|h| h.on_turn_end(turn, response))
            .collect();
        futures_util::future::join_all(futs).await;
    }

    pub async fn fire_tool_call(&self, name: &str, args: &Value) {
        if self.handlers.is_empty() {
            return;
        }
        let futs: Vec<_> = self
            .handlers
            .iter()
            .map(|h| h.on_tool_call(name, args))
            .collect();
        futures_util::future::join_all(futs).await;
    }

    pub async fn fire_tool_result(&self, name: &str, success: bool, duration: Duration) {
        if self.handlers.is_empty() {
            return;
        }
        let futs: Vec<_> = self
            .handlers
            .iter()
            .map(|h| h.on_tool_result(name, success, duration))
            .collect();
        futures_util::future::join_all(futs).await;
    }

    // ---- Modifying dispatchers (serial by priority, with panic recovery) ----

    /// Run `before_tool_call` hooks in priority order. Returns Cancel if any hook cancels.
    pub async fn run_before_tool_call(
        &self,
        mut name: String,
        mut args: Value,
    ) -> HookResult<(String, Value)> {
        for h in &self.handlers {
            use std::panic::AssertUnwindSafe;
            match AssertUnwindSafe(h.before_tool_call(name.clone(), args.clone()))
                .catch_unwind()
                .await
            {
                Ok(HookResult::Continue((n, a))) => {
                    name = n;
                    args = a;
                }
                Ok(HookResult::Cancel(reason)) => {
                    return HookResult::Cancel(reason);
                }
                Err(_) => {
                    eprintln!("[hook] {} panicked in before_tool_call, continuing", h.name());
                }
            }
        }
        HookResult::Continue((name, args))
    }

    /// Run `after_tool_result` hooks in priority order. Returns Cancel if any hook cancels.
    pub async fn run_after_tool_result(
        &self,
        mut name: String,
        mut output: crate::tool::ToolOutput,
    ) -> HookResult<(String, crate::tool::ToolOutput)> {
        for h in &self.handlers {
            use std::panic::AssertUnwindSafe;
            match AssertUnwindSafe(h.after_tool_result(name.clone(), output.clone()))
                .catch_unwind()
                .await
            {
                Ok(HookResult::Continue((n, o))) => {
                    name = n;
                    output = o;
                }
                Ok(HookResult::Cancel(reason)) => {
                    return HookResult::Cancel(reason);
                }
                Err(_) => {
                    eprintln!("[hook] {} panicked in after_tool_result, continuing", h.name());
                }
            }
        }
        HookResult::Continue((name, output))
    }
}

impl Default for HookRunner {
    fn default() -> Self {
        Self::new()
    }
}
