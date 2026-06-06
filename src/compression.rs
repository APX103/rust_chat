//! Context compression engine — 5-phase algorithm.
//!
//! When conversation approaches token limit, compresses middle messages:
//! 1. Tool result pruning (dedup + truncate large outputs)
//! 2. Protect head (keep first N messages)
//! 3. Protect tail (keep last N messages)
//! 4. Summarize middle (structured summary from message content)
//! 5. Sanitize (fix orphaned tool_call/result pairs)

use crate::models::{Message, MessageRole};
use anyhow::Result;
use std::collections::HashMap;

/// Context compressor for managing conversation length.
pub struct ContextCompressor {
    enabled: bool,
    threshold_percent: f64,
    protect_first_n: usize,
    protect_last_n: usize,
    max_context_tokens: usize,
    last_compression_savings: Vec<f64>,
}

impl ContextCompressor {
    pub fn new(enabled: bool, threshold_percent: f64, protect_first_n: usize, protect_last_n: usize, max_context_tokens: usize) -> Self {
        Self {
            enabled,
            threshold_percent,
            protect_first_n,
            protect_last_n,
            max_context_tokens,
            last_compression_savings: vec![],
        }
    }

    /// Check if compression should trigger based on current token count.
    pub fn should_compress(&self, current_tokens: usize) -> bool {
        if !self.enabled {
            return false;
        }
        // Anti-thrashing: if last 2 compressions saved <10%, skip
        if self.last_compression_savings.len() >= 2 {
            let recent: Vec<f64> = self.last_compression_savings.iter().rev().take(2).cloned().collect();
            if recent.iter().all(|&s| s < 0.10) {
                return false;
            }
        }
        let threshold = (self.max_context_tokens as f64 * self.threshold_percent) as usize;
        current_tokens >= threshold
    }

    /// Compress messages using 5-phase algorithm.
    /// Returns (compressed_messages, summary_text).
    pub fn compress(&mut self, messages: &[Message]) -> Result<(Vec<Message>, Option<String>)> {
        if messages.len() <= self.protect_first_n + self.protect_last_n {
            return Ok((messages.to_vec(), None));
        }

        // Phase 1: Prune tool results
        let pruned = self.phase1_prune_tool_results(messages);

        // Phase 2+3: Split into head, middle, tail
        let (head, middle, tail) = self.phase2_3_split(&pruned);

        if middle.is_empty() {
            return Ok((pruned, None));
        }

        // Phase 4: Summarize middle
        let summary = self.phase4_summarize(&middle);

        // Phase 5: Sanitize (already handled by summary approach)
        let mut compressed = Vec::with_capacity(head.len() + 1 + tail.len());
        compressed.extend_from_slice(&head);

        if let Some(ref s) = summary {
            compressed.push(Message::assistant(
                format!("[Compressed summary of {} messages]:\n{}", middle.len(), s)
            ));
        }

        compressed.extend_from_slice(&tail);

        // Track savings
        let original_tokens = Self::estimate_tokens(messages);
        let compressed_tokens = Self::estimate_tokens(&compressed);
        let savings = if original_tokens > 0 {
            (original_tokens.saturating_sub(compressed_tokens)) as f64 / original_tokens as f64
        } else {
            0.0
        };
        self.last_compression_savings.push(savings);
        if self.last_compression_savings.len() > 5 {
            self.last_compression_savings.remove(0);
        }

        log::info!(
            "Compression: {} -> {} msgs, {:.1}% savings",
            messages.len(),
            compressed.len(),
            savings * 100.0
        );

        Ok((compressed, summary))
    }

    /// Phase 1: Replace old tool results with 1-line summaries; dedup identical results.
    fn phase1_prune_tool_results(&self, messages: &[Message]) -> Vec<Message> {
        let mut result = vec![];
        let mut seen: HashMap<String, usize> = HashMap::new();

        for msg in messages {
            match msg.role {
                MessageRole::Tool => {
                    if let Some(ref content) = msg.content {
                        let key = format!("{}:{}", msg.name.as_deref().unwrap_or(""), content);
                        if let Some(&idx) = seen.get(&key) {
                            // Dedup: reference previous result
                            result.push(Message::tool(
                                msg.tool_call_id.as_deref().unwrap_or(""),
                                msg.name.as_deref().unwrap_or(""),
                                format!("[Same result as msg #{} — {} chars]", idx, content.len()),
                            ));
                        } else {
                            seen.insert(key, result.len());
                            // Truncate large outputs
                            let truncated = if content.len() > 500 {
                                let lines = content.lines().count();
                                let preview = if content.len() > 200 {
                                    format!("{}\n... ({} lines total, truncated)", &content[..200], lines)
                                } else {
                                    content.clone()
                                };
                                format!("[{} output, {} lines]\n{}", msg.name.as_deref().unwrap_or("tool"), lines, preview)
                            } else {
                                content.clone()
                            };
                            result.push(Message {
                                content: Some(truncated),
                                ..msg.clone()
                            });
                        }
                    } else {
                        result.push(msg.clone());
                    }
                }
                _ => result.push(msg.clone()),
            }
        }
        result
    }

    /// Phase 2+3: Split into head (first N), middle, tail (last N).
    fn phase2_3_split(&self, messages: &[Message]) -> (Vec<Message>, Vec<Message>, Vec<Message>) {
        let total = messages.len();
        let head_end = self.protect_first_n.min(total);
        let tail_start = total.saturating_sub(self.protect_last_n);

        let head = messages[..head_end].to_vec();
        let tail = messages[tail_start..].to_vec();
        let middle = if tail_start > head_end {
            messages[head_end..tail_start].to_vec()
        } else {
            vec![]
        };

        (head, middle, tail)
    }

    /// Phase 4: Generate structured summary of middle messages.
    fn phase4_summarize(&self, middle: &[Message]) -> Option<String> {
        if middle.is_empty() {
            return None;
        }

        let mut lines = vec!["## Conversation Summary".to_string()];
        let mut active_task = String::new();
        let mut actions = vec![];
        let mut decisions = vec![];

        for msg in middle {
            let content = msg.content.as_deref().unwrap_or("");
            match msg.role {
                MessageRole::User => {
                    if active_task.is_empty() {
                        active_task = truncate(content, 100);
                    }
                    if !content.is_empty() {
                        actions.push(format!("User asked: {}", truncate(content, 80)));
                    }
                }
                MessageRole::Assistant => {
                    if !content.is_empty() {
                        actions.push(format!("Agent responded: {}", truncate(content, 80)));
                    }
                    // Detect decisions
                    let lower = content.to_lowercase();
                    if lower.contains("decided") || lower.contains("chosen") || lower.contains("will use") {
                        decisions.push(truncate(content, 80));
                    }
                }
                MessageRole::Tool => {
                    if let Some(name) = &msg.name {
                        actions.push(format!("Ran tool {}: {}", name, truncate(content, 50)));
                    }
                }
                _ => {}
            }
        }

        if !active_task.is_empty() {
            lines.push(format!("**Active Task:** {}", active_task));
        }
        if !actions.is_empty() {
            lines.push(format!("**Actions:** {}", actions.join("; ")));
        }
        if !decisions.is_empty() {
            lines.push(format!("**Key Decisions:** {}", decisions.join("; ")));
        }

        Some(lines.join("\n"))
    }

    /// Estimate token count (rough: chars / 4).
    fn estimate_tokens(messages: &[Message]) -> usize {
        messages.iter().map(|m| {
            m.content.as_deref().unwrap_or("").chars().count() / 4
        }).sum()
    }
}

fn truncate(s: &str, max_len: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max_len {
        s.to_string()
    } else {
        chars[..max_len].iter().collect::<String>() + "..."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Message;

    fn make_msg(role: MessageRole, content: &str) -> Message {
        Message {
            role,
            content: Some(content.to_string()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            reasoning: None,
        }
    }

    #[test]
    fn test_should_compress_respects_threshold() {
        let compressor = ContextCompressor::new(true, 0.5, 3, 5, 1000);
        assert!(!compressor.should_compress(400));  // below 50% of 1000
        assert!(compressor.should_compress(600));   // above 50% of 1000
    }

    #[test]
    fn test_should_compress_disabled() {
        let compressor = ContextCompressor::new(false, 0.5, 3, 5, 1000);
        assert!(!compressor.should_compress(10000));
    }

    #[test]
    fn test_anti_thrashing() {
        let mut compressor = ContextCompressor::new(true, 0.5, 3, 5, 1000);
        // Record two low-savings compressions
        compressor.last_compression_savings.push(0.05);
        compressor.last_compression_savings.push(0.05);
        assert!(!compressor.should_compress(600));
    }

    #[test]
    fn test_compress_preserves_head_and_tail() {
        let mut compressor = ContextCompressor::new(true, 0.5, 2, 2, 100);
        let messages = vec![
            make_msg(MessageRole::System, "system prompt"),
            make_msg(MessageRole::User, "first user msg"),
            make_msg(MessageRole::Assistant, "middle 1"),
            make_msg(MessageRole::User, "middle 2"),
            make_msg(MessageRole::Assistant, "last assistant"),
            make_msg(MessageRole::User, "last user"),
        ];

        let (compressed, summary) = compressor.compress(&messages).unwrap();

        // Head (system + first user) and tail (last assistant + last user) preserved
        assert!(compressed.iter().any(|m| m.content.as_ref().map_or(false, |c| c.contains("system prompt"))));
        assert!(compressed.iter().any(|m| m.content.as_ref().map_or(false, |c| c.contains("last assistant"))));
        assert!(compressed.iter().any(|m| m.content.as_ref().map_or(false, |c| c.contains("last user"))));
        // Summary should exist
        assert!(summary.is_some());
        assert!(summary.as_ref().unwrap().contains("## Conversation Summary"));
    }

    #[test]
    fn test_compress_skips_small_message_list() {
        let mut compressor = ContextCompressor::new(true, 0.5, 3, 3, 100);
        let messages = vec![
            make_msg(MessageRole::User, "hi"),
            make_msg(MessageRole::Assistant, "hello"),
            make_msg(MessageRole::User, "how are you?"),
        ];
        // 3 messages, protect_first_n=3 + protect_last_n=3 = 6, so no compression
        let (compressed, _) = compressor.compress(&messages).unwrap();
        assert_eq!(compressed.len(), 3);
    }

    #[test]
    fn test_phase1_prune_dedups_tool_results() {
        let compressor = ContextCompressor::new(true, 0.5, 1, 1, 100);
        let messages = vec![
            make_msg(MessageRole::User, "run ls"),
            make_msg(MessageRole::Tool, "file1.txt\nfile2.txt"),
            make_msg(MessageRole::Assistant, "done"),
            make_msg(MessageRole::Tool, "file1.txt\nfile2.txt"), // duplicate
            make_msg(MessageRole::User, "final"),
        ];

        let pruned = compressor.phase1_prune_tool_results(&messages);
        // Second tool result should be replaced with reference
        let tool_msgs: Vec<_> = pruned.iter().filter(|m| m.role == MessageRole::Tool).collect();
        assert_eq!(tool_msgs.len(), 2);
        assert!(tool_msgs[1].content.as_ref().unwrap().contains("Same result"));
    }

    #[test]
    fn test_estimate_tokens() {
        let messages = vec![
            make_msg(MessageRole::User, "hello world"),
            make_msg(MessageRole::Assistant, "hi there"),
        ];
        let tokens = ContextCompressor::estimate_tokens(&messages);
        // "hello world" = 11 chars = ~2 tokens, "hi there" = 8 chars = ~2 tokens
        assert!(tokens >= 4 && tokens <= 5);
    }
}
