//! Observability system — inspired by ZeroClaw's Observer trait.
//!
//! Captures lifecycle events for debugging, metrics, and learning.
//! Default implementation logs via the `log` crate.

use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub enum Event {
    SessionStart { session_id: String },
    SessionEnd { session_id: String },
    TurnStart { turn_number: usize, user_message_preview: String },
    LlmRequest {
        model: String,
        messages_count: usize,
        tools_count: usize,
    },
    LlmResponse {
        model: String,
        prompt_tokens: i32,
        completion_tokens: i32,
        latency: Duration,
    },
    ToolCall {
        name: String,
        args: serde_json::Value,
    },
    ToolResult {
        name: String,
        success: bool,
        duration: Duration,
        output_len: usize,
    },
    MemoryWrite {
        key: String,
        category: String,
    },
    TurnComplete {
        turn_number: usize,
        api_calls: usize,
        total_tokens: i32,
    },
}

pub trait Observer: Send + Sync {
    fn on_event(&self, event: Event);
}

/// No-op observer — useful when observability is disabled.
pub struct NoopObserver;

impl Observer for NoopObserver {
    fn on_event(&self, _event: Event) {}
}

/// Default observer — logs all events at `debug` or `info` level.
pub struct LogObserver;

impl Observer for LogObserver {
    fn on_event(&self, event: Event) {
        match event {
            Event::SessionStart { session_id } => {
                log::info!("[observer] Session start: {}", session_id);
            }
            Event::SessionEnd { session_id } => {
                log::info!("[observer] Session end: {}", session_id);
            }
            Event::TurnStart {
                turn_number,
                user_message_preview,
            } => {
                let preview = if user_message_preview.len() > 60 {
                    format!("{}...", &user_message_preview[..60])
                } else {
                    user_message_preview
                };
                log::info!(
                    "[observer] Turn {} start: {}",
                    turn_number,
                    preview
                );
            }
            Event::LlmRequest {
                model,
                messages_count,
                tools_count,
            } => {
                log::debug!(
                    "[observer] LLM request → model={}, messages={}, tools={}",
                    model,
                    messages_count,
                    tools_count
                );
            }
            Event::LlmResponse {
                model,
                prompt_tokens,
                completion_tokens,
                latency,
            } => {
                log::info!(
                    "[observer] LLM response ← model={}, tokens={}+{} ({}ms)",
                    model,
                    prompt_tokens,
                    completion_tokens,
                    latency.as_millis()
                );
            }
            Event::ToolCall { name, args } => {
                log::debug!("[observer] Tool call: {} args={}", name, args);
            }
            Event::ToolResult {
                name,
                success,
                duration,
                output_len,
            } => {
                log::info!(
                    "[observer] Tool result: {} success={} {}ms len={}",
                    name,
                    success,
                    duration.as_millis(),
                    output_len
                );
            }
            Event::MemoryWrite { key, category } => {
                log::debug!(
                    "[observer] Memory write: key='{}' category='{}'",
                    key,
                    category
                );
            }
            Event::TurnComplete {
                turn_number,
                api_calls,
                total_tokens,
            } => {
                log::info!(
                    "[observer] Turn {} complete: {} API calls, {} total tokens",
                    turn_number,
                    api_calls,
                    total_tokens
                );
            }
        }
    }
}

/// Combines multiple observers — dispatches every event to all of them.
pub struct MultiObserver {
    observers: Vec<Arc<dyn Observer>>,
}

impl MultiObserver {
    pub fn new(observers: Vec<Arc<dyn Observer>>) -> Self {
        Self { observers }
    }

    pub fn push(&mut self, observer: Arc<dyn Observer>) {
        self.observers.push(observer);
    }
}

impl Observer for MultiObserver {
    fn on_event(&self, event: Event) {
        for obs in &self.observers {
            obs.on_event(event.clone());
        }
    }
}

/// Helper for timing tool calls.
pub struct Timer {
    start: Instant,
}

impl Timer {
    pub fn start() -> Self {
        Self {
            start: Instant::now(),
        }
    }

    pub fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }
}
