//! Configuration management for mini-agent.
//!
//! Config search order (first match wins):
//!   1. ./.mini-agent/config.toml      (project-local, current dir)
//!   2. ~/.mini-agent/config.toml      (global, user home)
//!   3. --config <path>                (CLI override, highest priority)
//!
//! When a local config exists, data directory also goes local:
//!   ./.mini-agent/data/  instead of  ~/.mini-agent/data/

use crate::models::AgentConfig;
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

pub const APP_NAME: &str = "mini-agent";
const CONFIG_FILENAME: &str = "config.toml";

/// Get the global config dir (~/.mini-agent or %USERPROFILE%\.mini-agent)
pub fn get_global_config_dir() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(format!(".{}", APP_NAME))
}

/// Check if a local project config exists in current directory.
pub fn has_local_config() -> bool {
    PathBuf::from(format!(".{}", APP_NAME))
        .join(CONFIG_FILENAME)
        .exists()
}

/// Get the active config directory.
/// If ./.mini-agent/config.toml exists, use ./.mini-agent/;
/// otherwise fall back to ~/.mini-agent/
pub fn get_config_dir() -> PathBuf {
    if has_local_config() {
        PathBuf::from(format!(".{}", APP_NAME))
    } else {
        get_global_config_dir()
    }
}

/// Get the active config file path (searches local first, then global).
pub fn get_config_path() -> PathBuf {
    // Check current directory first
    let local = PathBuf::from(format!(".{}", APP_NAME)).join(CONFIG_FILENAME);
    if local.exists() {
        return local;
    }
    // Fall back to global
    get_global_config_dir().join(CONFIG_FILENAME)
}

pub fn get_data_dir() -> PathBuf {
    get_config_dir().join("data")
}

pub fn get_skills_dir() -> PathBuf {
    get_config_dir().join("skills")
}

pub fn get_identity_path() -> PathBuf {
    get_config_dir().join("identity.md")
}

pub fn load_config() -> Result<AgentConfig> {
    load_config_from(&get_config_path())
}

pub fn load_config_from(path: &Path) -> Result<AgentConfig> {
    if path.exists() {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("Failed to read config: {}", path.display()))?;
        let config: AgentConfig = toml::from_str(&contents)
            .map_err(|e| anyhow::anyhow!("Failed to parse config TOML from {}: {}", path.display(), e))?;
        Ok(config)
    } else {
        log::warn!("Config not found at {}, using defaults", path.display());
        Ok(default_config())
    }
}

pub fn ensure_dirs() -> Result<()> {
    let dirs = [get_config_dir(), get_data_dir(), get_skills_dir()];
    for dir in &dirs {
        if !dir.exists() {
            fs::create_dir_all(dir)
                .with_context(|| format!("Failed to create dir: {}", dir.display()))?;
        }
    }
    Ok(())
}

pub fn default_config() -> AgentConfig {
    AgentConfig {
        model: crate::models::ModelConfig {
            provider: "openai".to_string(),
            model: "gpt-4o-mini".to_string(),
            api_key: std::env::var("OPENAI_API_KEY").unwrap_or_default(),
            base_url: "https://api.openai.com/v1".to_string(),
            max_tokens: 4096,
            temperature: 0.7,
            top_p: 1.0,
            extra_headers: std::collections::HashMap::new(),
            timeout: 120,
        },
        memory: crate::models::MemoryConfig {
            enabled: true,
            semantic_search_top_k: 5,
            episodic_summary_threshold: 10,
            provider: "builtin".to_string(),
            hybrid_search: true,
        },
        observer: crate::models::ObserverConfig {
            enabled: true,
            kind: "log".to_string(),
        },
        heartbeat: crate::models::HeartbeatConfig {
            enabled: false,
            interval_secs: 3600,
            tasks: vec!["auto_summarize".to_string(), "memory_cleanup".to_string()],
        },
        file_memory: crate::models::FileMemoryConfig::default(),
        compression: crate::models::CompressionConfig::default(),
        mcp_servers: std::collections::HashMap::new(),
        skills: crate::models::SkillsConfig {
            enabled: true,
            auto_create: true,
            external_dirs: vec![],
        },
        agent: crate::models::AgentBehaviorConfig {
            max_iterations: 30,
            enable_reasoning: true,
        },
    }
}

pub fn write_default_config() -> Result<()> {
    let config_path = get_config_path();
    if !config_path.exists() {
        ensure_dirs()?;
        let config = default_config();
        let toml = toml::to_string_pretty(&config)?;
        fs::write(&config_path, toml)
            .with_context(|| format!("Failed to write config: {}", config_path.display()))?;
        log::info!("Created default config at {}", config_path.display());
    }
    Ok(())
}

pub fn write_default_identity() -> Result<()> {
    let identity_path = get_identity_path();
    if !identity_path.exists() {
        ensure_dirs()?;
        let identity = crate::identity::default_identity();
        identity.save(&identity_path)?;
        log::info!("Created default identity at {}", identity_path.display());
    }
    Ok(())
}
