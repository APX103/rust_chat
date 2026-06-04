//! Skill management system for mini-agent.
//!
//! Skills are self-contained units of capability that the agent can
//! create, update, delete, and invoke. Inspired by Hermes' skill system.

use crate::models::SkillManifest;
use anyhow::{anyhow, Context, Result};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

pub struct SkillManager {
    skills_dir: PathBuf,
}

impl SkillManager {
    pub fn new(skills_dir: PathBuf) -> Self {
        Self { skills_dir }
    }

    pub fn ensure_dir(&self) -> Result<()> {
        if !self.skills_dir.exists() {
            fs::create_dir_all(&self.skills_dir)?;
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // CRUD operations
    // -----------------------------------------------------------------------

    pub fn create_skill(&self, manifest: &SkillManifest, body: &str) -> Result<PathBuf> {
        self.ensure_dir()?;
        let skill_dir = self.skills_dir.join(&manifest.name);
        fs::create_dir_all(&skill_dir)?;

        let manifest_toml = toml::to_string_pretty(manifest)?;
        let content = format!("---\n{}---\n\n{}", manifest_toml, body);
        let skill_path = skill_dir.join("SKILL.md");
        fs::write(&skill_path, content)
            .with_context(|| format!("Failed to write skill: {}", skill_path.display()))?;

        log::info!("Created skill: {} at {}", manifest.name, skill_path.display());
        Ok(skill_path)
    }

    pub fn update_skill(&self, name: &str, body: &str) -> Result<PathBuf> {
        let skill_path = self.find_skill_path(name)?;
        let existing = fs::read_to_string(&skill_path)?;
        
        // Preserve frontmatter, replace body
        let mut new_content = existing.clone();
        if let Some(body_start) = existing.find("\n---\n") {
            let frontmatter_end = body_start + 5;
            new_content = format!("{}\n{}", &existing[..frontmatter_end], body);
        } else {
            new_content = body.to_string();
        }
        
        fs::write(&skill_path, new_content)?;
        log::info!("Updated skill: {}", name);
        Ok(skill_path)
    }

    pub fn patch_skill(&self, name: &str, old_string: &str, new_string: &str) -> Result<PathBuf> {
        let skill_path = self.find_skill_path(name)?;
        let content = fs::read_to_string(&skill_path)?;
        
        if !content.contains(old_string) {
            return Err(anyhow!("old_string not found in skill '{}'", name));
        }
        
        let patched = content.replace(old_string, new_string);
        fs::write(&skill_path, patched)?;
        log::info!("Patched skill: {}", name);
        Ok(skill_path)
    }

    pub fn delete_skill(&self, name: &str) -> Result<()> {
        let skill_path = self.find_skill_path(name)?;
        let skill_dir = skill_path.parent()
            .ok_or_else(|| anyhow!("Invalid skill path"))?;
        fs::remove_dir_all(skill_dir)?;
        log::info!("Deleted skill: {}", name);
        Ok(())
    }

    pub fn read_skill(&self, name: &str) -> Result<(SkillManifest, String, PathBuf)> {
        let skill_path = self.find_skill_path(name)?;
        let content = fs::read_to_string(&skill_path)?;
        parse_skill_md(&content, &skill_path)
    }

    // -----------------------------------------------------------------------
    // Discovery
    // -----------------------------------------------------------------------

    pub fn list_skills(&self) -> Result<Vec<(String, String)>> {
        self.ensure_dir()?;
        let mut skills = vec![];
        
        for entry in fs::read_dir(&self.skills_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                let skill_md = path.join("SKILL.md");
                if skill_md.exists() {
                    if let Ok(content) = fs::read_to_string(&skill_md) {
                        if let Ok((manifest, _, _)) = parse_skill_md(&content, &skill_md) {
                            skills.push((manifest.name, manifest.description));
                        }
                    }
                }
            }
        }
        
        Ok(skills)
    }

    pub fn find_skill_path(&self, name: &str) -> Result<PathBuf> {
        self.ensure_dir()?;
        
        // Direct match
        let direct = self.skills_dir.join(name).join("SKILL.md");
        if direct.exists() {
            return Ok(direct);
        }
        
        // Recursive search
        for entry in walkdir::WalkDir::new(&self.skills_dir).max_depth(3) {
            let entry = entry?;
            if entry.file_name() == Some(std::ffi::OsStr::new("SKILL.md")) {
                let parent = entry.path().parent().unwrap_or(Path::new(""));
                if parent.file_name().map(|f| f == name).unwrap_or(false) {
                    return Ok(entry.path().to_path_buf());
                }
            }
        }
        
        Err(anyhow!("Skill '{}' not found", name))
    }

    pub fn build_skill_index_prompt(&self) -> Result<String> {
        let skills = self.list_skills()?;
        if skills.is_empty() {
            return Ok(String::new());
        }
        
        let mut lines = vec!["## Available Skills".to_string()];
        lines.push("Invoke a skill by name with a leading slash (e.g., /skill-name).".to_string());
        lines.push("".to_string());
        
        for (name, desc) in skills {
            lines.push(format!("- /{}: {}", name, desc));
        }
        
        Ok(lines.join("\n"))
    }

    pub fn invoke_skill(&self, name: &str, user_instruction: &str) -> Result<String> {
        let (manifest, body, skill_dir) = self.read_skill(name)?;
        
        let mut message = format!(
            "[IMPORTANT: The user has invoked the \"{}\" skill]\n\n",
            manifest.name
        );
        message.push_str(&body);
        message.push_str(&format!("\n\n[Skill directory: {}]\n", skill_dir.parent().unwrap_or(Path::new("")).display()));
        
        if !user_instruction.is_empty() {
            message.push_str(&format!("\n\n[User instruction: {}]\n", user_instruction));
        }
        
        Ok(message)
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

pub fn parse_skill_md(content: &str, path: &Path) -> Result<(SkillManifest, String, PathBuf)> {
    let content = content.trim_start();
    
    if !content.starts_with("---") {
        return Err(anyhow!("Missing frontmatter in {}", path.display()));
    }
    
    let end = content[3..].find("---")
        .ok_or_else(|| anyhow!("Unterminated frontmatter in {}", path.display()))?;
    
    let frontmatter = &content[3..end + 3];
    let body = content[end + 6..].trim_start();
    
    let manifest: SkillManifest = toml::from_str(frontmatter)
        .with_context(|| format!("Failed to parse frontmatter in {}", path.display()))?;
    
    Ok((manifest, body.to_string(), path.to_path_buf()))
}

// ---------------------------------------------------------------------------
// Built-in skill tools
// ---------------------------------------------------------------------------

pub fn get_skill_tools() -> Vec<crate::models::ToolSchema> {
    vec![
        crate::models::ToolSchema {
            name: "skill_manage".to_string(),
            description: "Create, update, patch, or delete a skill. Actions: create, update, patch, delete.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["create", "update", "patch", "delete"] },
                    "name": { "type": "string", "description": "Skill name (lowercase, kebab-case)" },
                    "description": { "type": "string", "description": "Short description (required for create)" },
                    "content": { "type": "string", "description": "Full SKILL.md content (for create/update)" },
                    "old_string": { "type": "string", "description": "Text to replace (for patch)" },
                    "new_string": { "type": "string", "description": "Replacement text (for patch)" }
                },
                "required": ["action", "name"]
            }),
        },
        crate::models::ToolSchema {
            name: "skills_list".to_string(),
            description: "List all available skills with their descriptions.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        crate::models::ToolSchema {
            name: "skill_view".to_string(),
            description: "View the full content of a specific skill.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Name of the skill to view" }
                },
                "required": ["name"]
            }),
        },
    ]
}

// ---------------------------------------------------------------------------
// Skill tool handlers
// ---------------------------------------------------------------------------

pub fn handle_skill_manage(
    manager: &SkillManager,
    args: &serde_json::Value,
) -> anyhow::Result<String> {
    let action = args["action"].as_str().ok_or_else(|| anyhow!("Missing action"))?;
    let name = args["name"].as_str().ok_or_else(|| anyhow!("Missing name"))?;
    
    match action {
        "create" => {
            let description = args["description"].as_str()
                .ok_or_else(|| anyhow!("Missing description for create"))?;
            let content = args["content"].as_str().unwrap_or("");
            let manifest = SkillManifest {
                name: name.to_string(),
                description: description.to_string(),
                version: "1.0.0".to_string(),
                author: "mini-agent".to_string(),
                tags: vec![],
                triggers: vec![],
                system_prompt_patch: None,
                mcp_server: None,
                created_at: Some(chrono::Utc::now()),
                updated_at: Some(chrono::Utc::now()),
                usage_count: 0,
            };
            let path = manager.create_skill(&manifest, content)?;
            Ok(format!("Created skill '{}' at {}", name, path.display()))
        }
        "update" => {
            let content = args["content"].as_str().unwrap_or("");
            let path = manager.update_skill(name, content)?;
            Ok(format!("Updated skill '{}' at {}", name, path.display()))
        }
        "patch" => {
            let old = args["old_string"].as_str().ok_or_else(|| anyhow!("Missing old_string"))?;
            let new = args["new_string"].as_str().ok_or_else(|| anyhow!("Missing new_string"))?;
            let path = manager.patch_skill(name, old, new)?;
            Ok(format!("Patched skill '{}' at {}", name, path.display()))
        }
        "delete" => {
            manager.delete_skill(name)?;
            Ok(format!("Deleted skill '{}'", name))
        }
        _ => Err(anyhow!("Unknown action: {}", action)),
    }
}

pub fn handle_skills_list(manager: &SkillManager) -> anyhow::Result<String> {
    let skills = manager.list_skills()?;
    if skills.is_empty() {
        return Ok("No skills available. Use skill_manage to create one.".to_string());
    }
    let lines: Vec<String> = skills.into_iter()
        .map(|(name, desc)| format!("- {}: {}", name, desc))
        .collect();
    Ok(lines.join("\n"))
}

pub fn handle_skill_view(manager: &SkillManager, args: &serde_json::Value) -> anyhow::Result<String> {
    let name = args["name"].as_str().ok_or_else(|| anyhow!("Missing name"))?;
    let (_manifest, body, _path) = manager.read_skill(name)?;
    Ok(body)
}

// Simple walkdir replacement
mod walkdir {
    use std::fs;
    use std::path::Path;
    
    pub struct WalkDir {
        root: String,
        max_depth: usize,
    }
    
    impl WalkDir {
        pub fn new<P: AsRef<Path>>(path: P) -> Self {
            Self {
                root: path.as_ref().to_string_lossy().to_string(),
                max_depth: usize::MAX,
            }
        }
        
        pub fn max_depth(mut self, depth: usize) -> Self {
            self.max_depth = depth;
            self
        }
    }
    
    impl IntoIterator for WalkDir {
        type Item = Result<DirEntry, std::io::Error>;
        type IntoIter = std::vec::IntoIter<Self::Item>;
        
        fn into_iter(self) -> Self::IntoIter {
            let mut results = vec![];
            walk(Path::new(&self.root), 0, self.max_depth, &mut results);
            results.into_iter()
        }
    }
    
    fn walk(path: &Path, depth: usize, max_depth: usize, results: &mut Vec<Result<DirEntry, std::io::Error>>) {
        if depth > max_depth {
            return;
        }
        if let Ok(entries) = fs::read_dir(path) {
            for entry in entries.flatten() {
                results.push(Ok(DirEntry { path: entry.path() }));
                if entry.path().is_dir() {
                    walk(&entry.path(), depth + 1, max_depth, results);
                }
            }
        }
    }
    
    pub struct DirEntry {
        path: std::path::PathBuf,
    }
    
    impl DirEntry {
        pub fn path(&self) -> &std::path::Path {
            &self.path
        }
        
        pub fn file_name(&self) -> Option<&std::ffi::OsStr> {
            self.path.file_name()
        }
    }
}
