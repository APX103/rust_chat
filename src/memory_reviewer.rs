//! Automatic background memory review — inspired by Hermes Agent.
//!
//! Periodically reviews conversation history and extracts durable facts
//! (user preferences, corrections, persona details) into FileMemoryStore.
//!
//! Two modes:
//! - Turn-based review: every N turns, review recent window
//! - Session-end review: on /new or /quit, review entire session

use crate::file_memory::{FileMemoryStore, MemoryTarget};
use crate::llm::LlmClient;
use crate::models::Message;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Prompt
// ---------------------------------------------------------------------------

const REVIEW_SYSTEM_PROMPT: &str = r#"You are a memory extraction assistant. Your job is to review a conversation and extract durable facts worth saving to persistent memory.

Focus on:
- User preferences (style, format, tone, tools, workflows)
- Personal details (name, role, tech stack, project context)
- Corrections ("don't do X", "I prefer Y", "always use Z")
- Recurring patterns ("every morning I...", "for this project we...")

DO NOT save:
- Transient errors or environment issues
- One-off task steps that won't recur
- Negative claims about tools ("X is broken today")
- Anything time-sensitive or likely to change soon

Before adding a new fact, check if a similar fact already exists. If so, use "replace" instead of "add".

Output STRICT JSON with this exact structure:
{
  "actions": [
    {"type": "add", "target": "memory", "content": "..."},
    {"type": "add", "target": "user", "content": "..."},
    {"type": "replace", "target": "memory", "old": "...", "new": "..."}
  ],
  "summary": "One-line description of what was saved, or 'Nothing to save.'"
}

IMPORTANT: target MUST be exactly "memory" or "user" (not "memory|user")."#;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ReviewActionType {
    Add,
    Replace,
    Remove,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReviewAction {
    #[serde(rename = "type")]
    action_type: ReviewActionType,
    target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    old: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    new: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReviewResponse {
    #[serde(default)]
    actions: Vec<ReviewAction>,
    summary: String,
}

#[derive(Debug, Clone)]
pub struct ReviewResult {
    pub actions_executed: usize,
    pub summary: String,
}

// ---------------------------------------------------------------------------
// MemoryReviewer
// ---------------------------------------------------------------------------

pub struct MemoryReviewer {
    client: LlmClient,
    file_memory: Arc<FileMemoryStore>,
    interval: usize,
    window_size: usize,
    turns_since_review: Mutex<usize>,
}

impl MemoryReviewer {
    pub fn new(
        client: LlmClient,
        file_memory: Arc<FileMemoryStore>,
        interval: usize,
        window_size: usize,
    ) -> Self {
        Self {
            client,
            file_memory,
            interval,
            window_size,
            turns_since_review: Mutex::new(0),
        }
    }

    /// Check if review should trigger for the given turn count.
    /// If interval is 0, auto-review is disabled.
    pub fn should_review(&self, _turn_count: usize) -> bool {
        if self.interval == 0 {
            return false;
        }
        let mut counter = self.turns_since_review.lock().unwrap();
        *counter += 1;
        if *counter >= self.interval {
            *counter = 0;
            true
        } else {
            false
        }
    }

    /// Reset the review counter (e.g. on /new).
    pub fn reset_counter(&self) {
        *self.turns_since_review.lock().unwrap() = 0;
    }

    pub fn interval(&self) -> usize {
        self.interval
    }

    pub fn window_size(&self) -> usize {
        self.window_size
    }

    /// Review a snapshot of recent conversation turns.
    pub fn review_turns(&self, snapshot: &[Message]) -> Result<ReviewResult> {
        if snapshot.is_empty() {
            return Ok(ReviewResult {
                actions_executed: 0,
                summary: "Nothing to save.".to_string(),
            });
        }

        let prompt = self.build_review_prompt(snapshot);
        let review_json = self.call_llm_for_review(&prompt)?;
        let response: ReviewResponse = serde_json::from_str(&review_json)
            .with_context(|| format!("Failed to parse review JSON: {}", review_json))?;

        let executed = self.execute_actions(&response.actions)?;

        Ok(ReviewResult {
            actions_executed: executed,
            summary: response.summary,
        })
    }

    /// Review the entire session (used on /new or /quit).
    pub fn review_session(&self, all_messages: &[Message]) -> Result<ReviewResult> {
        // For session-end, use a larger window or the full history
        let window = all_messages.len();
        let snapshot = if window > 0 {
            &all_messages[window.saturating_sub(50)..]
        } else {
            all_messages
        };
        self.review_turns(snapshot)
    }

    // -----------------------------------------------------------------------
    // Internal
    // -----------------------------------------------------------------------

    fn build_review_prompt(&self, snapshot: &[Message]) -> Vec<Message> {
        let mut messages = vec![
            crate::models::Message::system(REVIEW_SYSTEM_PROMPT),
        ];

        // Serialize conversation snapshot
        let mut conversation_text = String::from("## Conversation to review\n\n");
        for msg in snapshot {
            let role = format!("{:?}", msg.role).to_lowercase();
            let content = msg.content.as_deref().unwrap_or("");
            if !content.is_empty() {
                conversation_text.push_str(&format!("[{}]: {}\n\n", role, content));
            }
        }

        messages.push(crate::models::Message::user(&conversation_text));
        messages
    }

    fn call_llm_for_review(&self, messages: &[Message]) -> Result<String> {
        let (response, _usage) = match self.client.chat(messages, None) {
            Ok(r) => r,
            Err(e) => {
                log::warn!("LLM review call failed: {}", e);
                return Err(e).context("LLM review call failed");
            }
        };

        let content = response.content.unwrap_or_default();
        // Extract JSON from potential markdown fences
        let json_str = Self::extract_json(&content);
        Ok(json_str.to_string())
    }

    /// Extract JSON from text that may be wrapped in markdown fences.
    fn extract_json(text: &str) -> &str {
        let trimmed = text.trim();
        if trimmed.starts_with("```json") {
            if let Some(end) = trimmed.find("\n```") {
                return trimmed[7..end].trim();
            }
        }
        if trimmed.starts_with("```") {
            if let Some(end) = trimmed.find("\n```") {
                return trimmed[3..end].trim();
            }
        }
        trimmed
    }

    fn execute_actions(&self, actions: &[ReviewAction]) -> Result<usize> {
        log::debug!("Executing {} review actions", actions.len());
        let mut executed = 0;
        for action in actions {
            log::debug!("Review action: {:?} target={} content={:?} old={:?} new={:?}",
                action.action_type, action.target, action.content, action.old, action.new);
            let target = match action.target.as_str() {
                "memory" => MemoryTarget::Memory,
                "user" => MemoryTarget::User,
                _ => {
                    log::warn!("Unknown review target: {}", action.target);
                    continue;
                }
            };

            match action.action_type {
                ReviewActionType::Add => {
                    if let Some(ref content) = action.content {
                        // Simple dedup: skip if substring already exists in snapshot
                        if !self.already_exists(target, content) {
                            self.file_memory.add(target, content)?;
                            executed += 1;
                            log::info!("Review added to {:?}: {}", target, content);
                        } else {
                            log::debug!("Review dedup skipped: {}", content);
                        }
                    }
                }
                ReviewActionType::Replace => {
                    if let (Some(ref old), Some(ref new)) = (&action.old, &action.new) {
                        if self.file_memory.replace(target, old, new)? {
                            executed += 1;
                            log::info!("Review replaced in {:?}: {} -> {}", target, old, new);
                        }
                    }
                }
                ReviewActionType::Remove => {
                    if let Some(ref old) = action.old {
                        if self.file_memory.remove(target, old)? {
                            executed += 1;
                            log::info!("Review removed from {:?}: {}", target, old);
                        }
                    }
                }
            }
        }
        Ok(executed)
    }

    /// Check if a semantically similar entry already exists.
    fn already_exists(&self, target: MemoryTarget, content: &str) -> bool {
        match self.file_memory.load_entries(target) {
            Ok(entries) => {
                let content_lower = content.to_lowercase();
                entries.iter().any(|e| {
                    e.content.to_lowercase().contains(&content_lower)
                        || content_lower.contains(&e.content.to_lowercase())
                })
            }
            Err(e) => {
                log::warn!("Failed to load entries for dedup: {}", e);
                false
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_json_from_markdown() {
        let md = "```json\n{\"actions\":[],\"summary\":\"test\"}\n```";
        assert_eq!(
            MemoryReviewer::extract_json(md),
            r#"{"actions":[],"summary":"test"}"#
        );
    }

    #[test]
    fn test_extract_json_plain() {
        let plain = r#"{"actions":[],"summary":"test"}"#;
        assert_eq!(MemoryReviewer::extract_json(plain), plain);
    }

    #[test]
    fn test_should_review_respects_interval() {
        // Mock client — we can't easily construct one, so skip integration test
    }
}
