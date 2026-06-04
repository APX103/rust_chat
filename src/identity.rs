//! Agent identity configuration — inspired by ZeroClaw's IdentityConfig.
//!
//! Loads agent personality from a markdown or JSON file.
//! Default location: `~/.mini-agent/identity.md`
//!
//! Markdown format (OpenClaw-style):
//! ```markdown
//! # Identity
//! Mini-Agent
//!
//! ## Description
//! A helpful AI assistant that learns from every conversation.
//!
//! ## Personality
//! - Curious and thoughtful
//! - Concise but thorough
//!
//! ## Rules
//! - Always cite sources when recalling memories
//! - Ask clarifying questions when uncertain
//!
//! ## Notes
//! - The user prefers short answers
//! ```

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Identity {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub personality: Vec<String>,
    #[serde(default)]
    pub rules: Vec<String>,
    #[serde(default)]
    pub notes: Vec<String>,
}

impl Identity {
    /// Load identity from a file (markdown or JSON).
    pub fn load(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read identity file: {}", path.display()))?;

        if path.extension().and_then(|s| s.to_str()) == Some("json") {
            Self::from_json(&content)
        } else {
            Self::from_markdown(&content)
        }
    }

    /// Parse OpenClaw-style markdown.
    pub fn from_markdown(content: &str) -> Result<Self> {
        let mut identity = Identity::default();
        let mut current_section: Option<&str> = None;

        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("# Identity") {
                // Next non-empty line is the name
                continue;
            } else if trimmed.starts_with("## Description") {
                current_section = Some("description");
                continue;
            } else if trimmed.starts_with("## Personality") {
                current_section = Some("personality");
                continue;
            } else if trimmed.starts_with("## Rules") {
                current_section = Some("rules");
                continue;
            } else if trimmed.starts_with("## Notes") {
                current_section = Some("notes");
                continue;
            }

            if trimmed.is_empty() {
                continue;
            }

            match current_section {
                None if identity.name.is_empty() => {
                    identity.name = trimmed.to_string();
                }
                Some("description") => {
                    identity.description = trimmed.to_string();
                }
                Some("personality") => {
                    let item = trimmed.trim_start_matches("-").trim().to_string();
                    if !item.is_empty() {
                        identity.personality.push(item);
                    }
                }
                Some("rules") => {
                    let item = trimmed.trim_start_matches("-").trim().to_string();
                    if !item.is_empty() {
                        identity.rules.push(item);
                    }
                }
                Some("notes") => {
                    let item = trimmed.trim_start_matches("-").trim().to_string();
                    if !item.is_empty() {
                        identity.notes.push(item);
                    }
                }
                _ => {}
            }
        }

        if identity.name.is_empty() {
            identity.name = "Mini-Agent".to_string();
        }

        Ok(identity)
    }

    /// Parse JSON format.
    pub fn from_json(content: &str) -> Result<Self> {
        let identity: Identity = serde_json::from_str(content)
            .with_context(|| "Failed to parse identity JSON")?;
        Ok(identity)
    }

    /// Convert identity to a system prompt string.
    pub fn to_system_prompt(&self) -> String {
        let mut parts = vec![];
        parts.push(format!("You are {}. {}", self.name, self.description));

        if !self.personality.is_empty() {
            parts.push(format!(
                "Personality:\n{}",
                self.personality
                    .iter()
                    .map(|p| format!("- {}", p))
                    .collect::<Vec<_>>()
                    .join("\n")
            ));
        }

        if !self.rules.is_empty() {
            parts.push(format!(
                "Rules:\n{}",
                self.rules
                    .iter()
                    .map(|r| format!("- {}", r))
                    .collect::<Vec<_>>()
                    .join("\n")
            ));
        }

        if !self.notes.is_empty() {
            parts.push(format!(
                "Notes:\n{}",
                self.notes
                    .iter()
                    .map(|n| format!("- {}", n))
                    .collect::<Vec<_>>()
                    .join("\n")
            ));
        }

        parts.join("\n\n")
    }

    /// Save identity as markdown.
    pub fn save(&self, path: &Path) -> Result<()> {
        let mut lines = vec![];
        lines.push(format!("# Identity\n{}\n", self.name));

        if !self.description.is_empty() {
            lines.push(format!("## Description\n{}\n", self.description));
        }

        if !self.personality.is_empty() {
            lines.push("## Personality".to_string());
            for p in &self.personality {
                lines.push(format!("- {}", p));
            }
            lines.push(String::new());
        }

        if !self.rules.is_empty() {
            lines.push("## Rules".to_string());
            for r in &self.rules {
                lines.push(format!("- {}", r));
            }
            lines.push(String::new());
        }

        if !self.notes.is_empty() {
            lines.push("## Notes".to_string());
            for n in &self.notes {
                lines.push(format!("- {}", n));
            }
            lines.push(String::new());
        }

        fs::write(path, lines.join("\n"))
            .with_context(|| format!("Failed to write identity to {}", path.display()))?;
        Ok(())
    }
}

/// Default identity when no file exists.
pub fn default_identity() -> Identity {
    Identity {
        name: "Mini-Agent".to_string(),
        description: "A helpful AI assistant with tool-calling capabilities and multi-layer memory.".to_string(),
        personality: vec![
            "Curious and eager to learn from every conversation".to_string(),
            "Concise but thorough in explanations".to_string(),
        ],
        rules: vec![
            "Use reasoning before tool calls when helpful".to_string(),
            "Recall memories when relevant to the user's query".to_string(),
        ],
        notes: vec![],
    }
}
