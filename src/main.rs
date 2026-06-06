//! Mini-Agent — A minimal but powerful AI agent.
//!
//! Features:
//! - Multi-layer memory (Working, Episodic, Semantic, Procedural)
//! - MCP (Model Context Protocol) server integration
//! - Self-managing skills (create/update/delete)
//! - ReAct reasoning-acting loop
//! - Hook system for lifecycle interception
//!
//! Target: ARM64 (Cortex-A57) + Debian 7 (static musl build)

// Re-exported from lib.rs — modules declared there.
use mini_agent::agent::{Agent, build_system_prompt};
use mini_agent::config;
use mini_agent::config::{ensure_dirs, get_data_dir, get_skills_dir, load_config, load_config_from, write_default_config, write_default_identity};
use mini_agent::file_memory::{FileMemoryStore, MemoryTarget};
use mini_agent::heartbeat::Heartbeat;
use mini_agent::hooks::HookRunner;
use mini_agent::identity::{default_identity, Identity};
use mini_agent::llm::LlmClient;
use mini_agent::memory::{BuiltinMemoryProvider, MemoryManager, SqliteMemory};
use mini_agent::memory_reviewer::MemoryReviewer;
use mini_agent::mcp::{McpClientManager, register_mcp_tools};
use mini_agent::observer::{Event, LogObserver, MultiObserver, NoopObserver, Observer};
use mini_agent::session_search::SessionDB;
use mini_agent::skill::{SkillManager, get_skill_tools, handle_skill_manage, handle_skill_view, handle_skills_list};
use mini_agent::tool_registry::ToolRegistry;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[tokio::main]
async fn main() {
    env_logger::init();

    let args: Vec<String> = std::env::args().collect();
    let mut config_override: Option<String> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--version" | "-v" => {
                println!("mini-agent {}", VERSION);
                return;
            }
            "--help" | "-h" => {
                print_help();
                return;
            }
            "--setup" | "--onboard" => {
                if let Err(e) = run_setup_wizard() {
                    eprintln!("Setup failed: {}", e);
                    std::process::exit(1);
                }
                return;
            }
            "--config" | "-c" => {
                if i + 1 < args.len() {
                    config_override = Some(args[i + 1].clone());
                    i += 1;
                } else {
                    eprintln!("Error: --config requires a path argument");
                    std::process::exit(1);
                }
            }
            _ => {}
        }
        i += 1;
    }

    println!("╔══════════════════════════════════════╗");
    println!("║         Mini-Agent v{}            ║", VERSION);
    println!("║  Multi-layer memory + MCP + Skills   ║");
    println!("╚══════════════════════════════════════╝");
    println!();

    if let Err(e) = run(config_override).await {
        eprintln!("Fatal error: {}", e);
        std::process::exit(1);
    }
}

fn print_help() {
    println!("mini-agent {} — A minimal but powerful AI agent", VERSION);
    println!();
    println!("Usage: mini-agent [OPTIONS]");
    println!();
    println!("Options:");
    println!("  -h, --help            Print this help message");
    println!("  -v, --version         Print version information");
    println!("  -c, --config <PATH>   Use specific config file");
    println!("  --setup               Run interactive setup wizard");
    println!();
    println!("Environment:");
    println!("  OPENAI_API_KEY        Default API key for OpenAI provider");
    println!();
    println!("Config directory: {}", config::get_config_dir().display());
}

async fn run(config_override: Option<String>) -> anyhow::Result<()> {
    // Setup directories
    ensure_dirs()?;
    write_default_config()?;
    write_default_identity()?;

    // Load identity
    let identity = match Identity::load(&config::get_identity_path()) {
        Ok(id) => id,
        Err(e) => {
            log::warn!("Failed to load identity: {}. Using default.", e);
            default_identity()
        }
    };

    // Load config
    let cfg = if let Some(path) = config_override {
        let p = std::path::Path::new(&path);
        log::info!("Using config override: {}", p.display());
        load_config_from(p)?
    } else {
        load_config()?
    };

    // Initialize SQLite memory
    let db_path = get_data_dir().join("memory.db");
    let sqlite_memory = Arc::new(SqliteMemory::new(&db_path)?);
    sqlite_memory.set_session_id("default");

    // Initialize file memory store
    let file_memory = Arc::new(FileMemoryStore::new(
        get_data_dir().join("MEMORY.md"),
        get_data_dir().join("USER.md"),
        cfg.file_memory.memory_char_limit,
        cfg.file_memory.user_char_limit,
    )?);

    // Initialize memory manager with builtin provider + file memory
    let mut memory_manager = MemoryManager::new()
        .with_file_memory(file_memory.clone());
    let builtin_memory = Arc::new(BuiltinMemoryProvider::new(
        sqlite_memory.clone(),
        cfg.memory.semantic_search_top_k,
        cfg.memory.episodic_summary_threshold,
        cfg.memory.hybrid_search,
    ));
    memory_manager.add_provider(builtin_memory);
    let memory_manager = Arc::new(memory_manager);

    // Initialize skill manager
    let skill_manager = Arc::new(SkillManager::new(get_skills_dir()));

    // Initialize tool registry
    let registry = Arc::new(ToolRegistry::new());

    // Initialize session search DB
    let session_db: Option<Arc<SessionDB>> = {
        let session_db_path = config::get_data_dir().join("sessions.db");
        match SessionDB::new(&session_db_path) {
            Ok(db) => Some(Arc::new(db)),
            Err(e) => {
                log::warn!("Failed to initialize session search: {}", e);
                None
            }
        }
    };

    // Register built-in memory tools
    register_builtin_tools(&registry, sqlite_memory.clone(), skill_manager.clone(), Some(file_memory.clone()), session_db.clone());

    // Register skill management tools
    register_skill_tools(&registry, skill_manager.clone());

    // Connect MCP servers
    let mcp_manager = Arc::new(McpClientManager::new());
    if !cfg.mcp_servers.is_empty() {
        println!("🔌 Connecting to MCP servers...");
        match mcp_manager.connect_servers(&cfg.mcp_servers) {
            Ok(tool_names) => {
                println!("✅ MCP connected. Discovered {} tools.", tool_names.len());
                if let Err(e) = register_mcp_tools(&registry, &mcp_manager, &cfg.mcp_servers) {
                    log::warn!("Failed to register some MCP tools: {}", e);
                }
            }
            Err(e) => {
                log::warn!("MCP connection failed: {}", e);
            }
        }
    }

    // Build system prompt
    let system_prompt = build_system_prompt(&identity, &skill_manager, &memory_manager, cfg.agent.enable_reasoning)?;

    // Initialize LLM client
    let client = LlmClient::new(
        cfg.model.api_key.clone(),
        cfg.model.base_url.clone(),
        cfg.model.model.clone(),
    )
    .with_max_tokens(cfg.model.max_tokens)
    .with_temperature(cfg.model.temperature)
    .with_top_p(cfg.model.top_p)
    .with_extra_headers(cfg.model.extra_headers.clone())
    .with_timeout(cfg.model.timeout);

    // Initialize review client (may use cheaper model for cost control)
    let review_client = LlmClient::new(
        cfg.model.api_key,
        cfg.model.base_url,
        cfg.review.model_override.unwrap_or_else(|| cfg.model.model.clone()),
    )
    .with_max_tokens(cfg.review.max_tokens)
    .with_temperature(0.3)
    .with_top_p(1.0)  // keep 1.0 for Zhipu compatibility (some providers reject top_p < 1)
    .with_extra_headers(cfg.model.extra_headers)
    .with_timeout(cfg.model.timeout);

    // Initialize memory reviewer
    let memory_reviewer: Option<Arc<MemoryReviewer>> = if cfg.review.enabled {
        Some(Arc::new(MemoryReviewer::new(
            review_client,
            file_memory.clone(),
            cfg.review.interval,
            cfg.review.window_size,
        )))
    } else {
        None
    };

    // Initialize observer (terminal + log combined)
    let observer: Arc<dyn Observer> = if cfg.observer.enabled {
        let mut multi = MultiObserver::new(vec![]);
        multi.push(Arc::new(LogObserver));
        multi.push(Arc::new(TerminalObserver));
        Arc::new(multi)
    } else {
        Arc::new(NoopObserver)
    };

    // Initialize heartbeat
    let mut heartbeat = Heartbeat::new();
    if cfg.heartbeat.enabled {
        heartbeat.start(
            cfg.heartbeat.interval_secs,
            cfg.heartbeat.tasks.clone(),
            sqlite_memory.clone(),
        );
    }

    // Initialize hook runner and register audit hook
    let mut hooks = HookRunner::new();
    hooks.register(Box::new(AuditHook::new()));
    let hooks = Arc::new(hooks);

    // Initialize agent
    let mut agent = Agent::new(
        client,
        registry.clone(),
        memory_manager.clone(),
        skill_manager.clone(),
        cfg.agent.max_iterations,
        cfg.compression.enabled,
        cfg.model.max_tokens as usize,
    );
    agent.set_system_prompt(system_prompt);
    agent.set_observer(Some(observer));
    agent.set_hooks(hooks.clone());
    agent.set_session_db(session_db.clone());
    agent.set_memory_reviewer(memory_reviewer.clone());
    agent.emit_session_start();

    // Fire session start hooks
    hooks.fire_session_start(&agent.session_id).await;

    println!("💡 Type /help for commands, /quit to exit.\n");

    // REPL loop
    loop {
        print!("\n🧠 You: ");
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let input = input.trim();

        if input.is_empty() {
            continue;
        }

        // Slash commands
        if input.starts_with('/') {
            match handle_slash(input, &skill_manager, &mut agent).await {
                Ok(should_quit) => {
                    if should_quit {
                        break;
                    }
                    continue;
                }
                Err(e) => {
                    eprintln!("Command error: {}", e);
                    continue;
                }
            }
        }

        // Normal conversation with streaming
        let mut spinner = Spinner::start("Thinking...");
        let spinner_flag = spinner.running();

        agent.set_on_token(move |token| {
            spinner_flag.store(false, Ordering::Relaxed);
            print!("{}", token);
            io::stdout().flush().unwrap();
        });

        match agent.chat(input).await {
            Ok(_response) => {
                spinner.stop();
                println!(); // newline after stream
            }
            Err(e) => {
                spinner.stop();
                eprintln!("Error: {}", e);
            }
        }

        // Turn-based background review
        if let Some(ref reviewer) = memory_reviewer {
            if reviewer.should_review(agent.turn_count) {
                let snapshot = agent.build_review_snapshot(reviewer.window_size());
                let reviewer_clone = reviewer.clone();
                std::thread::spawn(move || {
                    match reviewer_clone.review_turns(&snapshot) {
                        Ok(result) => {
                            if result.actions_executed > 0 {
                                log::info!("📝 Background review saved {} memories: {}", result.actions_executed, result.summary);
                            } else {
                                log::debug!("Background review: {}", result.summary);
                            }
                        }
                        Err(e) => log::warn!("Background review failed: {}", e),
                    }
                });
            }
        }
    }

    // Cleanup
    println!("\n👋 Goodbye!");

    // Session-end deep review before exit
    if let Some(result) = agent.run_session_review() {
        println!("Running session-end review...");
        match result {
            Ok(result) => {
                if result.actions_executed > 0 {
                    println!("📝 Session review saved {} memories: {}", result.actions_executed, result.summary);
                } else {
                    println!("📝 Session review: {}", result.summary);
                }
            }
            Err(e) => println!("⚠️ Session review failed: {}", e),
        }
    }

    // Fire session end hooks
    hooks.fire_session_end(&agent.session_id).await;

    agent.emit_session_end();
    heartbeat.stop();
    mcp_manager.shutdown_all();
    memory_manager.on_session_end(&agent.session_id);

    Ok(())
}

async fn handle_slash(input: &str, skill_manager: &SkillManager, agent: &mut Agent) -> anyhow::Result<bool> {
    let parts: Vec<&str> = input.split_whitespace().collect();
    let cmd = parts.get(0).unwrap_or(&"");

    match *cmd {
        "/quit" | "/exit" | "/q" => return Ok(true),
        "/help" | "/h" => {
            println!("Commands:");
            println!("  /quit, /q       Exit");
            println!("  /help, /h       Show this help");
            println!("  /new            Start a new session");
            println!("  /review         Run memory review on current conversation");
            println!("  /skills         List available skills");
            println!("  /memory         Show memory status");
            println!("  /clear          Clear conversation history");
            println!("  /model <name>   Switch model");
            println!("");
            println!("Invoke a skill: /skill-name");
        }
        "/new" => {
            // Session-end review before starting new session
            if let Some(result) = agent.run_session_review() {
                println!("Running session-end review...");
                match result {
                    Ok(result) => {
                        if result.actions_executed > 0 {
                            println!("📝 Session review saved {} memories: {}", result.actions_executed, result.summary);
                        } else {
                            println!("📝 Session review: {}", result.summary);
                        }
                    }
                    Err(e) => println!("⚠️ Session review failed: {}", e),
                }
            }
            let old_session = agent.session_id[..8].to_string();
            agent.new_session();
            println!("🆕 New session started. Old: {}..., New: {}...", old_session, &agent.session_id[..8]);
        }
        "/review" => {
            match agent.run_manual_review() {
                Some(Ok(result)) => {
                    if result.actions_executed > 0 {
                        println!("📝 Review saved {} memories: {}", result.actions_executed, result.summary);
                    } else {
                        println!("📝 Review: {}", result.summary);
                    }
                }
                Some(Err(e)) => eprintln!("Review failed: {}", e),
                None => println!("Memory reviewer not enabled."),
            }
        }
        "/skills" => {
            match handle_skills_list(skill_manager) {
                Ok(output) => println!("{}", output),
                Err(e) => eprintln!("Error: {}", e),
            }
        }
        "/memory" => {
            println!("Memory status:");
            println!("  Session: {}", agent.session_id);
            println!("  Turns: {}", agent.turn_count);
            println!("  API calls: {}", agent.api_call_count);
            println!("  Budget: {}/{}", agent.iteration_budget.used, agent.iteration_budget.max_total);
        }
        "/clear" => {
            agent.conversation_history.clear();
            println!("Conversation history cleared.");
        }
        "/model" => {
            if let Some(model) = parts.get(1) {
                agent.switch_model(model);
                println!("🔄 Model switched to: {}", model);
            } else {
                println!("Usage: /model <model-name>");
            }
        }
        _ => {
            // Try skill invocation
            let skill_name = &cmd[1..];
            if !skill_name.is_empty() {
                let user_instruction = parts[1..].join(" ");
                match skill_manager.invoke_skill(skill_name, &user_instruction) {
                    Ok(skill_msg) => {
                        println!("📋 Skill loaded: /{}", skill_name);
                        match agent.chat(&skill_msg).await {
                            Ok(response) => println!("\n🤖 Agent: {}", response),
                            Err(e) => eprintln!("Error: {}", e),
                        }
                    }
                    Err(_) => {
                        println!("Unknown command or skill: {}. Type /help for available commands.", cmd);
                    }
                }
            }
        }
    }

    Ok(false)
}

fn register_builtin_tools(
    registry: &ToolRegistry,
    db: Arc<SqliteMemory>,
    _skill_manager: Arc<SkillManager>,
    file_memory: Option<Arc<FileMemoryStore>>,
    session_db: Option<Arc<SessionDB>>,
) {
    let db_clone = db.clone();
    let file_memory_clone = file_memory.clone();
    registry.register_tool_legacy(
        mini_agent::models::ToolSchema {
            name: "memory".to_string(),
            description: "Add a fact to long-term semantic memory.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["add", "recall", "profile_set", "profile_get", "replace", "remove"] },
                    "target": { "type": "string", "enum": ["memory", "user"], "description": "Target file (memory=MEMORY.md, user=USER.md)" },
                    "key": { "type": "string" },
                    "value": { "type": "string" },
                    "category": { "type": "string" },
                    "query": { "type": "string" },
                    "old_text": { "type": "string", "description": "Text to find for replace/remove" },
                    "content": { "type": "string", "description": "New content for replace" }
                },
                "required": ["action"]
            }),
        },
        Arc::new(move |_name: &str, args: &serde_json::Value| {
            let action = args["action"].as_str().unwrap_or("");
            match action {
                "add" => {
                    let key = args["key"].as_str().unwrap_or("");
                    let value = args["value"].as_str().unwrap_or("");
                    let content = args["content"].as_str().unwrap_or("");
                    let category = args["category"].as_str();
                    // LLM sometimes sends 'content' instead of 'value'; be tolerant
                    let actual_value = if !value.is_empty() { value } else { content };
                    let actual_key = if !key.is_empty() {
                        key.to_string()
                    } else {
                        // Derive key from first few words of value/content
                        actual_value.split_whitespace().take(3).collect::<Vec<_>>().join("_")
                    };
                    if actual_key.is_empty() || actual_value.is_empty() {
                        return Ok("Error: 'add' requires content or (key + value).".to_string());
                    }
                    db_clone.remember(&actual_key, actual_value, category)?;
                    Ok(format!("Remembered: {} = {}", actual_key, actual_value))
                }
                "recall" => {
                    let query = args["query"].as_str().unwrap_or("");
                    let results = db_clone.recall(query, 5)?;
                    if results.is_empty() {
                        Ok("No relevant memories found.".to_string())
                    } else {
                        let lines: Vec<String> = results.into_iter()
                            .map(|(k, v, s)| format!("- {} = {} (score: {:.1})", k, v, s))
                            .collect();
                        Ok(lines.join("\n"))
                    }
                }
                "profile_set" => {
                    let key = args["key"].as_str().unwrap_or("");
                    let value = args["value"].as_str().unwrap_or("");
                    db_clone.set_profile(key, value, 0.8)?;
                    Ok(format!("Set profile: {} = {}", key, value))
                }
                "profile_get" => {
                    let key = args["key"].as_str().unwrap_or("");
                    match db_clone.get_profile(key)? {
                        Some((v, c)) => Ok(format!("{} = {} (confidence: {:.0}%)", key, v, c * 100.0)),
                        None => Ok(format!("No profile entry for '{}'", key)),
                    }
                }
                "replace" => {
                    let target_str = args["target"].as_str().unwrap_or("memory");
                    let old_text = args["old_text"].as_str().unwrap_or("");
                    let new_content = args["content"].as_str().unwrap_or("");
                    let target = MemoryTarget::from_str(target_str).unwrap_or(MemoryTarget::Memory);
                    match file_memory_clone.as_ref()
                        .ok_or_else(|| anyhow::anyhow!("File memory not available"))?
                        .replace(target, old_text, new_content)
                    {
                        Ok(true) => Ok("Entry replaced successfully.".to_string()),
                        Ok(false) => Ok("No matching entry found to replace.".to_string()),
                        Err(e) => Err(e),
                    }
                }
                "remove" => {
                    let target_str = args["target"].as_str().unwrap_or("memory");
                    let old_text = args["old_text"].as_str().unwrap_or("");
                    let target = MemoryTarget::from_str(target_str).unwrap_or(MemoryTarget::Memory);
                    match file_memory_clone.as_ref()
                        .ok_or_else(|| anyhow::anyhow!("File memory not available"))?
                        .remove(target, old_text)
                    {
                        Ok(true) => Ok("Entry removed successfully.".to_string()),
                        Ok(false) => Ok("No matching entry found to remove.".to_string()),
                        Err(e) => Err(e),
                    }
                }
                _ => Err(anyhow::anyhow!("Unknown memory action: {}", action))
            }
        }),
        mini_agent::models::ToolSource::Builtin,
    );

    // session_search tool
    let session_db_clone = session_db.clone();
    registry.register_tool_legacy(
        mini_agent::models::ToolSchema {
            name: "session_search".to_string(),
            description: "Search past conversation sessions for relevant context.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "mode": { "type": "string", "enum": ["discover", "scroll", "browse"] },
                    "query": { "type": "string", "description": "Search query (for discover mode)" },
                    "session_id": { "type": "string" },
                    "message_id": { "type": "integer" },
                    "limit": { "type": "integer", "default": 10 }
                },
                "required": []
            }),
        },
        Arc::new(move |_name: &str, args: &serde_json::Value| {
            let mode = args["mode"].as_str().unwrap_or("browse");
            let db = match session_db_clone.as_ref() {
                Some(db) => db,
                None => return Ok("Session search is not enabled.".to_string()),
            };

            match mode {
                "discover" => {
                    let query = args["query"].as_str().unwrap_or("");
                    let limit = args["limit"].as_u64().unwrap_or(5) as usize;
                    let results = db.discover(query, limit)?;
                    if results.is_empty() {
                        Ok("No matching sessions found.".to_string())
                    } else {
                        let lines: Vec<String> = results.iter()
                            .map(|r| format!("- [{}] {} ({} msgs): {}",
                                &r.session_id[..8], r.title.as_deref().unwrap_or("untitled"), r.message_count, r.snippet))
                            .collect();
                        Ok(format!("Found {} sessions:\n{}", results.len(), lines.join("\n")))
                    }
                }
                "scroll" => {
                    let session_id = args["session_id"].as_str().unwrap_or("");
                    let message_id = args["message_id"].as_i64();
                    let limit = args["limit"].as_u64().unwrap_or(10) as usize;
                    let msgs = db.scroll(session_id, message_id, limit)?;
                    if msgs.is_empty() {
                        Ok("No messages found.".to_string())
                    } else {
                        let lines: Vec<String> = msgs.iter()
                            .map(|m| format!("[{}] {}: {}",
                                m.role, &m.timestamp[..19], m.content.as_deref().unwrap_or("")))
                            .collect();
                        Ok(lines.join("\n"))
                    }
                }
                "browse" => {
                    let limit = args["limit"].as_u64().unwrap_or(10) as usize;
                    let sessions = db.browse(limit)?;
                    if sessions.is_empty() {
                        Ok("No sessions recorded yet.".to_string())
                    } else {
                        let lines: Vec<String> = sessions.iter()
                            .map(|s| format!("- [{}] {} ({} msgs) — {}",
                                &s.id[..8], s.title.as_deref().unwrap_or("untitled"), s.message_count, s.started_at))
                            .collect();
                        Ok(format!("Recent sessions:\n{}", lines.join("\n")))
                    }
                }
                _ => Ok("Unknown mode. Use discover, scroll, or browse.".to_string()),
            }
        }),
        mini_agent::models::ToolSource::Builtin,
    );
}

fn register_skill_tools(registry: &ToolRegistry, skill_manager: Arc<SkillManager>) {
    for schema in get_skill_tools() {
        let name = schema.name.clone();
        let sm = skill_manager.clone();

        registry.register_tool_legacy(
            schema,
            Arc::new(move |_name: &str, args: &serde_json::Value| {
                match name.as_str() {
                    "skill_manage" => handle_skill_manage(&sm, args),
                    "skills_list" => handle_skills_list(&sm),
                    "skill_view" => handle_skill_view(&sm, args),
                    _ => Err(anyhow::anyhow!("Unknown skill tool: {}", name)),
                }
            }),
            mini_agent::models::ToolSource::Builtin,
        );
    }
}

// ---------------------------------------------------------------------------
// AuditHook — logs all tool calls for debugging/auditing
// ---------------------------------------------------------------------------

struct AuditHook {
    name: String,
}

impl AuditHook {
    fn new() -> Self {
        Self {
            name: "audit_hook".to_string(),
        }
    }
}

#[async_trait::async_trait]
impl mini_agent::hooks::HookHandler for AuditHook {
    fn name(&self) -> &str {
        &self.name
    }

    fn priority(&self) -> i32 {
        100 // High priority: run first for modifying hooks
    }

    async fn on_tool_call(&self, name: &str, args: &serde_json::Value) {
        log::info!("[audit] Tool called: {} args={}", name, args);
    }

    async fn on_tool_result(&self, name: &str, success: bool, duration: std::time::Duration) {
        log::info!(
            "[audit] Tool result: {} success={} duration={}ms",
            name,
            success,
            duration.as_millis()
        );
    }

    async fn on_session_start(&self, session_id: &str) {
        log::info!("[audit] Session started: {}", session_id);
    }

    async fn on_session_end(&self, session_id: &str) {
        log::info!("[audit] Session ended: {}", session_id);
    }

    async fn on_turn_start(&self, turn: usize, message: &str) {
        log::debug!("[audit] Turn {} start: {}", turn, message);
    }

    async fn on_turn_end(&self, turn: usize, response: &str) {
        log::debug!("[audit] Turn {} end: {} chars", turn, response.len());
    }
}

// ---------------------------------------------------------------------------
// Spinner — animated waiting indicator
// ---------------------------------------------------------------------------

struct Spinner {
    running: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Spinner {
    fn start(message: &str) -> Self {
        let running = Arc::new(AtomicBool::new(true));
        let r = running.clone();
        let msg = message.to_string();
        let handle = std::thread::spawn(move || {
            let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let mut i = 0;
            while r.load(Ordering::Relaxed) {
                eprint!("\r{} {} ", frames[i % frames.len()], msg);
                std::io::stderr().flush().ok();
                std::thread::sleep(std::time::Duration::from_millis(80));
                i += 1;
            }
            eprint!("\r{:width$}\r", "", width = msg.len() + 4);
            std::io::stderr().flush().ok();
        });
        Self { running, handle: Some(handle) }
    }

    fn running(&self) -> Arc<AtomicBool> {
        self.running.clone()
    }

    fn stop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            h.join().ok();
        }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---------------------------------------------------------------------------
// TerminalObserver — prints tool calls to stdout
// ---------------------------------------------------------------------------

struct TerminalObserver;

impl Observer for TerminalObserver {
    fn on_event(&self, event: Event) {
        match event {
            Event::ToolCall { name, args } => {
                println!("\n🔧 Calling tool: {}({})", name, args);
            }
            Event::ToolResult { name, success, duration, .. } => {
                let icon = if success { "✅" } else { "❌" };
                println!("{} Tool {} finished in {:?}", icon, name, duration);
            }
            _ => {}
        }
    }
}

fn run_setup_wizard() -> anyhow::Result<()> {
    use std::io::{stdin, stdout, Write};

    println!("╔══════════════════════════════════════╗");
    println!("║      Mini-Agent Setup Wizard         ║");
    println!("╚══════════════════════════════════════╝");
    println!();

    ensure_dirs()?;

    let mut input = String::new();

    // 1. Provider
    println!("Choose your LLM provider:");
    println!("  [1] OpenAI        (api.openai.com)");
    println!("  [2] OpenRouter    (openrouter.ai)");
    println!("  [3] Ollama        (localhost:11434)");
    println!("  [4] Custom");
    print!("> ");
    stdout().flush()?;
    input.clear();
    stdin().read_line(&mut input)?;
    let provider_choice = input.trim();

    let (provider, default_base, default_model) = match provider_choice {
        "2" => ("openrouter", "https://openrouter.ai/api/v1", "openai/gpt-4o-mini"),
        "3" => ("ollama", "http://localhost:11434/v1", "llama3.2"),
        "4" => ("custom", "", ""),
        _ => ("openai", "https://api.openai.com/v1", "gpt-4o-mini"),
    };

    // 2. Base URL (if custom)
    let mut base_url = default_base.to_string();
    if provider_choice == "4" {
        print!("Enter base URL: ");
        stdout().flush()?;
        input.clear();
        stdin().read_line(&mut input)?;
        base_url = input.trim().to_string();
    }

    // 3. API Key
    print!("Enter API key (or leave empty for local models): ");
    stdout().flush()?;
    input.clear();
    stdin().read_line(&mut input)?;
    let api_key = input.trim().to_string();

    // 4. Model
    print!("Enter model name [default: {}]: ", default_model);
    stdout().flush()?;
    input.clear();
    stdin().read_line(&mut input)?;
    let model = if input.trim().is_empty() {
        default_model.to_string()
    } else {
        input.trim().to_string()
    };

    // 5. Test connection?
    print!("Test connection? (y/N): ");
    stdout().flush()?;
    input.clear();
    stdin().read_line(&mut input)?;
    if input.trim().eq_ignore_ascii_case("y") {
        let client = LlmClient::new(api_key.clone(), base_url.clone(), model.clone())
            .with_max_tokens(10);
        match client.chat(
            &[mini_agent::models::Message::user("hi")],
            None,
        ) {
            Ok((msg, _)) => {
                println!("✅ Connection OK. Response: {}", msg.content.unwrap_or_default());
            }
            Err(e) => {
                println!("⚠️  Connection test failed: {}", e);
                println!("   You can still save the config and fix it later.");
            }
        }
    }

    // 6. Memory enabled?
    print!("Enable multi-layer memory? (Y/n): ");
    stdout().flush()?;
    input.clear();
    stdin().read_line(&mut input)?;
    let memory_enabled = !input.trim().eq_ignore_ascii_case("n");

    // 7. Heartbeat enabled?
    print!("Enable background heartbeat tasks? (y/N): ");
    stdout().flush()?;
    input.clear();
    stdin().read_line(&mut input)?;
    let heartbeat_enabled = input.trim().eq_ignore_ascii_case("y");

    // Build config
    let config = mini_agent::models::AgentConfig {
        model: mini_agent::models::ModelConfig {
            provider: provider.to_string(),
            model,
            api_key,
            base_url,
            max_tokens: 4096,
            temperature: 0.7,
            top_p: 1.0,
            extra_headers: std::collections::HashMap::new(),
            timeout: 120,
        },
        memory: mini_agent::models::MemoryConfig {
            enabled: memory_enabled,
            semantic_search_top_k: 5,
            episodic_summary_threshold: 10,
            provider: "builtin".to_string(),
            hybrid_search: true,
        },
        mcp_servers: std::collections::HashMap::new(),
        skills: mini_agent::models::SkillsConfig {
            enabled: true,
            auto_create: true,
            external_dirs: vec![],
        },
        agent: mini_agent::models::AgentBehaviorConfig {
            max_iterations: 30,
            enable_reasoning: true,
        },
        observer: mini_agent::models::ObserverConfig {
            enabled: true,
            kind: "log".to_string(),
        },
        heartbeat: mini_agent::models::HeartbeatConfig {
            enabled: heartbeat_enabled,
            interval_secs: 3600,
            tasks: vec!["auto_summarize".to_string(), "memory_cleanup".to_string()],
        },
        file_memory: mini_agent::models::FileMemoryConfig::default(),
        compression: mini_agent::models::CompressionConfig::default(),
        review: mini_agent::models::ReviewConfig {
            enabled: true,
            interval: 10,
            window_size: 8,
            max_tokens: 4096,
            model_override: None,
        },
    };

    let config_path = config::get_config_path();
    let toml = toml::to_string_pretty(&config)?;
    std::fs::write(&config_path, toml)?;
    println!();
    println!("✅ Config written to {}", config_path.display());
    println!("   Run `mini-agent` to start chatting.");

    Ok(())
}
