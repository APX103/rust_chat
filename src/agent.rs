//! ReAct agent core loop — inspired by Hermes Agent.
//!
//! Simplified but faithful reproduction of Hermes' conversation loop:
//! - Budget-controlled iteration
//! - Tool call validation and execution
//! - Memory prefetch / sync
//! - Hook system for lifecycle interception
//! - Grace call on budget exhaustion

use crate::hooks::HookRunner;
use crate::identity::Identity;
use crate::llm::{LlmClient, Usage};
use crate::memory::{build_memory_context_block, MemoryManager};
use crate::models::{Message, MessageRole, ToolCall, ToolSchema};
use crate::observer::{Event, Observer, Timer};
use crate::session_search::SessionDB;
use crate::skill::SkillManager;
use crate::tool_registry::ToolRegistry;
use crate::tool::ToolOutput;
use anyhow::Result;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use uuid::Uuid;

pub struct Agent {
    pub client: LlmClient,
    pub registry: Arc<ToolRegistry>,
    pub memory: Arc<MemoryManager>,
    pub skill_manager: Arc<SkillManager>,
    pub session_id: String,
    pub max_iterations: usize,
    pub system_prompt: String,
    pub conversation_history: Vec<Message>,
    pub turn_count: usize,
    pub api_call_count: usize,
    pub iteration_budget: IterationBudget,
    pub budget_grace_call: bool,
    compression_enabled: bool,
    max_context_tokens: usize,
    session_db: Option<Arc<SessionDB>>,
    observer: Option<Arc<dyn Observer>>,
    on_token: Option<Mutex<Box<dyn FnMut(&str) + Send>>>,
    hooks: Arc<HookRunner>,
}

pub struct IterationBudget {
    pub max_total: usize,
    pub used: usize,
}

impl IterationBudget {
    pub fn new(max: usize) -> Self {
        Self { max_total: max, used: 0 }
    }

    pub fn consume(&mut self) -> bool {
        if self.used < self.max_total {
            self.used += 1;
            true
        } else {
            false
        }
    }

    pub fn refund(&mut self) {
        if self.used > 0 {
            self.used -= 1;
        }
    }

    pub fn remaining(&self) -> usize {
        self.max_total.saturating_sub(self.used)
    }
}

impl Agent {
    pub fn new(
        client: LlmClient,
        registry: Arc<ToolRegistry>,
        memory: Arc<MemoryManager>,
        skill_manager: Arc<SkillManager>,
        max_iterations: usize,
        compression_enabled: bool,
        max_context_tokens: usize,
    ) -> Self {
        let session_id = Uuid::new_v4().to_string();
        Self {
            client,
            registry,
            memory,
            skill_manager,
            session_id: session_id.clone(),
            max_iterations,
            system_prompt: String::new(),
            conversation_history: vec![],
            turn_count: 0,
            api_call_count: 0,
            iteration_budget: IterationBudget::new(max_iterations),
            budget_grace_call: false,
            compression_enabled,
            max_context_tokens,
            session_db: None,
            observer: None,
            on_token: None,
            hooks: Arc::new(HookRunner::new()),
        }
    }

    /// Set the hook runner for lifecycle interception.
    pub fn set_hooks(&mut self, hooks: Arc<HookRunner>) {
        self.hooks = hooks;
    }

    pub fn set_system_prompt(&mut self, prompt: String) {
        self.system_prompt = prompt;
    }

    pub fn set_observer(&mut self, observer: Option<Arc<dyn Observer>>) {
        self.observer = observer;
    }

    pub fn set_on_token(&mut self, f: impl FnMut(&str) + Send + 'static) {
        self.on_token = Some(Mutex::new(Box::new(f)));
    }

    pub fn set_session_db(&mut self, db: Option<Arc<SessionDB>>) {
        self.session_db = db;
    }

    fn emit(&self, event: Event) {
        if let Some(ref obs) = self.observer {
            obs.on_event(event);
        }
    }

    pub async fn run_conversation(&mut self, user_message: &str) -> Result<String> {
        // Fire turn start hooks
        self.hooks
            .fire_turn_start(self.turn_count, user_message)
            .await;

        self.emit(Event::TurnStart {
            turn_number: self.turn_count,
            user_message_preview: user_message.to_string(),
        });

        let mut messages: Vec<Message> = self.conversation_history.clone();

        if messages.is_empty() && !self.system_prompt.is_empty() {
            messages.push(Message::system(&self.system_prompt));
        }

        // Add user message
        messages.push(Message::user(user_message));

        self.memory.on_turn_start(self.turn_count, user_message);

        let mut final_response = String::new();
        let mut total_usage: Option<Usage> = None;

        while (self.api_call_count < self.max_iterations && self.iteration_budget.remaining() > 0)
            || self.budget_grace_call
        {
            // Check budget / grace
            if self.budget_grace_call {
                self.budget_grace_call = false;
            } else if !self.iteration_budget.consume() {
                log::warn!("Iteration budget exhausted ({}/{})", self.iteration_budget.used, self.iteration_budget.max_total);
                break;
            }

            self.api_call_count += 1;
            log::info!("API call #{}/{}", self.api_call_count, self.max_iterations);

            // Prefetch memory and inject into messages
            let memory_context = self.memory.prefetch_all(user_message, &self.session_id);
            let mut api_messages = messages.clone();

            // Inject memory context into the last user message
            if let Some(last_user_idx) = api_messages.iter().rposition(|m| m.role == MessageRole::User) {
                let mem_block = build_memory_context_block(&memory_context);
                if !mem_block.is_empty() {
                    if let Some(content) = &api_messages[last_user_idx].content {
                        api_messages[last_user_idx].content = Some(format!("{}\n\n{}", content, mem_block));
                    }
                }
            }

            // Get available tools
            let tools = self.registry.list_tools();
            let tools_slice: Vec<ToolSchema> = if tools.is_empty() {
                vec![]
            } else {
                tools
            };

            self.emit(Event::LlmRequest {
                model: self.client.model().to_string(),
                messages_count: api_messages.len(),
                tools_count: tools_slice.len(),
            });

            let llm_start = Instant::now();

            // Call LLM (streaming if on_token is set)
            let (assistant_msg, usage) = if let Some(ref cb) = self.on_token {
                let mut guard = cb.lock().unwrap();
                self.client.chat_with_callback(
                    &api_messages,
                    if tools_slice.is_empty() { None } else { Some(&tools_slice) },
                    &mut **guard,
                )?
            } else {
                self.client.chat(
                    &api_messages,
                    if tools_slice.is_empty() { None } else { Some(&tools_slice) },
                )?
            };

            let llm_latency = llm_start.elapsed();

            if let Some(ref u) = usage {
                self.emit(Event::LlmResponse {
                    model: self.client.model().to_string(),
                    prompt_tokens: u.prompt_tokens,
                    completion_tokens: u.completion_tokens,
                    latency: llm_latency,
                });
            }

            if let Some(u) = usage {
                total_usage = Some(Usage {
                    prompt_tokens: u.prompt_tokens + total_usage.as_ref().map(|t| t.prompt_tokens).unwrap_or(0),
                    completion_tokens: u.completion_tokens + total_usage.as_ref().map(|t| t.completion_tokens).unwrap_or(0),
                    total_tokens: u.total_tokens + total_usage.as_ref().map(|t| t.total_tokens).unwrap_or(0),
                });
            }

            // Handle tool calls
            if let Some(tool_calls) = &assistant_msg.tool_calls {
                if !tool_calls.is_empty() {
                    log::info!("Processing {} tool call(s)...", tool_calls.len());

                    // Validate tool names
                    let mut valid_tool_calls = vec![];
                    let mut invalid_tools = vec![];

                    for tc in tool_calls {
                        if self.registry.has_tool(&tc.function.name) {
                            valid_tool_calls.push(tc.clone());
                        } else {
                            invalid_tools.push(tc.function.name.clone());
                        }
                    }

                    // Add assistant message with tool calls
                    let mut assistant_for_history = assistant_msg.clone();
                    assistant_for_history.content = assistant_for_history.content.filter(|c| !c.is_empty());
                    messages.push(assistant_for_history);

                    // Execute tools
                    if !invalid_tools.is_empty() {
                        log::warn!("Invalid tool calls: {:?}", invalid_tools);
                        for tc in tool_calls {
                            if invalid_tools.contains(&tc.function.name) {
                                messages.push(Message::tool(
                                    &tc.id,
                                    &tc.function.name,
                                    format!("Tool '{}' does not exist. Available: check skills_list or use built-in tools.", tc.function.name),
                                ));
                            }
                        }
                    }

                    for tc in &valid_tool_calls {
                        let args: serde_json::Value = if tc.function.arguments.trim().is_empty() {
                            serde_json::json!({})
                        } else {
                            serde_json::from_str(&tc.function.arguments).unwrap_or_else(|_| serde_json::json!({}))
                        };

                        // Fire tool_call notification hooks (parallel)
                        self.hooks.fire_tool_call(&tc.function.name, &args).await;

                        self.emit(Event::ToolCall {
                            name: tc.function.name.clone(),
                            args: args.clone(),
                        });

                        let timer = Timer::start();

                        // Run before_tool_call hooks (serial, can modify or cancel)
                        let hook_result = self.hooks
                            .run_before_tool_call(tc.function.name.clone(), args.clone())
                            .await;

                        let (tool_name, tool_args) = match hook_result {
                            crate::hooks::HookResult::Continue((n, a)) => (n, a),
                            crate::hooks::HookResult::Cancel(reason) => {
                                let err_msg = format!("Tool call cancelled by hook: {}", reason);
                                log::warn!("{}", err_msg);
                                let duration = timer.elapsed();
                                self.emit(Event::ToolResult {
                                    name: tc.function.name.clone(),
                                    success: false,
                                    duration,
                                    output_len: err_msg.len(),
                                });
                                self.hooks.fire_tool_result(&tc.function.name, false, duration).await;
                                messages.push(Message::tool(&tc.id, &tc.function.name, err_msg));
                                continue;
                            }
                        };

                        let result = self.execute_tool_call_with(&tool_name, &tool_args).await;

                        let duration = timer.elapsed();

                        match result {
                            Ok(output) => {
                                // Run after_tool_result hooks (serial, can modify output)
                                let hook_result = self.hooks
                                    .run_after_tool_result(tc.function.name.clone(), output.clone())
                                    .await;

                                let (final_name, final_output) = match hook_result {
                                    crate::hooks::HookResult::Continue((n, o)) => (n, o),
                                    crate::hooks::HookResult::Cancel(reason) => {
                                        let err_str = format!("Tool result cancelled by hook: {}", reason);
                                        log::warn!("{}", err_str);
                                        self.emit(Event::ToolResult {
                                            name: tc.function.name.clone(),
                                            success: false,
                                            duration,
                                            output_len: err_str.len(),
                                        });
                                        self.hooks.fire_tool_result(&tc.function.name, false, duration).await;
                                        messages.push(Message::tool(&tc.id, &tc.function.name, err_str));
                                        continue;
                                    }
                                };

                                self.emit(Event::ToolResult {
                                    name: final_name.clone(),
                                    success: final_output.success,
                                    duration,
                                    output_len: final_output.text.len(),
                                });
                                self.hooks.fire_tool_result(&final_name, final_output.success, duration).await;
                                messages.push(Message::tool(&tc.id, &final_name, final_output.text));
                            }
                            Err(e) => {
                                let err_str = format!("{{\"error\": \"{}\"}}", e);
                                self.emit(Event::ToolResult {
                                    name: tc.function.name.clone(),
                                    success: false,
                                    duration,
                                    output_len: err_str.len(),
                                });
                                self.hooks.fire_tool_result(&tc.function.name, false, duration).await;
                                messages.push(Message::tool(
                                    &tc.id,
                                    &tc.function.name,
                                    err_str,
                                ));
                            }
                        }
                    }

                    continue; // Loop back for next iteration
                }
            }

            // Check context size for potential compression
            if self.compression_enabled {
                let context_tokens = Self::estimate_context_tokens(&messages);
                if context_tokens > (self.max_context_tokens as f64 * 0.5) as usize {
                    log::warn!("Context approaching limit ({} tokens), consider /compress", context_tokens);
                }
            }

            // No tool calls — final response
            final_response = assistant_msg.content.clone().unwrap_or_default();

            // Preserve reasoning if present
            if let Some(reasoning) = &assistant_msg.reasoning {
                log::debug!("Reasoning: {}...", &reasoning[..reasoning.len().min(200)]);
            }

            messages.push(assistant_msg);
            break;
        }

        // Fire turn end hooks
        self.hooks
            .fire_turn_end(self.turn_count, &final_response)
            .await;

        // Sync memory
        self.memory.sync_all(user_message, &final_response, &self.session_id);

        // Store messages to session_db for long-term recall
        if let Some(ref db) = self.session_db {
            let user_content = user_message;
            let assistant_content = &final_response;
            if let Err(e) = db.store_message(&self.session_id, "user", Some(user_content), None, None) {
                log::debug!("Failed to store user message: {}", e);
            }
            if let Err(e) = db.store_message(&self.session_id, "assistant", Some(assistant_content), None, None) {
                log::debug!("Failed to store assistant message: {}", e);
            }
        }

        self.turn_count += 1;

        // Save conversation history (limit to last 50 messages to prevent bloat)
        // Preserve system prompt at the beginning if present
        const MAX_HISTORY: usize = 50;
        if messages.len() > MAX_HISTORY {
            let has_system = !messages.is_empty() && messages[0].role == MessageRole::System;
            let start = if has_system { 1 } else { 0 };
            let keep = MAX_HISTORY - start;
            if messages.len() - start > keep {
                let mut trimmed = vec![];
                if has_system {
                    trimmed.push(messages[0].clone());
                }
                trimmed.extend_from_slice(&messages[messages.len() - keep..]);
                messages = trimmed;
            }
        }
        self.conversation_history = messages;

        let total_tokens = total_usage.as_ref().map(|u| u.total_tokens).unwrap_or(0);
        self.emit(Event::TurnComplete {
            turn_number: self.turn_count,
            api_calls: self.api_call_count,
            total_tokens,
        });

        if let Some(u) = total_usage {
            log::info!("Turn complete. Tokens: prompt={}, completion={}, total={}",
                u.prompt_tokens, u.completion_tokens, u.total_tokens);
        }

        Ok(final_response)
    }

    /// Execute a tool call using its ToolCall struct (async version).
    async fn execute_tool_call(&self, tc: &ToolCall) -> Result<String> {
        let args: serde_json::Value = if tc.function.arguments.trim().is_empty() {
            serde_json::json!({})
        } else {
            serde_json::from_str(&tc.function.arguments)?
        };

        log::info!("Executing tool: {} with args: {}", tc.function.name, args);
        let output = self.registry.dispatch(&tc.function.name, &args).await?;
        Ok(output.text)
    }

    /// Execute a tool by name and args (used after hook modifications).
    async fn execute_tool_call_with(&self, name: &str, args: &serde_json::Value) -> Result<ToolOutput> {
        log::info!("Executing tool: {} with args: {}", name, args);
        self.registry.dispatch(name, args).await
    }

    fn estimate_context_tokens(messages: &[Message]) -> usize {
        messages.iter().map(|m| {
            m.content.as_deref().unwrap_or("").chars().count() / 4
        }).sum()
    }

    pub async fn chat(&mut self, message: &str) -> Result<String> {
        self.run_conversation(message).await
    }

    pub fn emit_session_start(&self) {
        self.emit(Event::SessionStart {
            session_id: self.session_id.clone(),
        });
    }

    pub fn emit_session_end(&self) {
        self.emit(Event::SessionEnd {
            session_id: self.session_id.clone(),
        });
    }

    /// Start a new session: end current one, reset state, emit start.
    pub fn new_session(&mut self) {
        // End old session
        if let Some(ref db) = self.session_db {
            let _ = db.end_session(&self.session_id);
        }

        self.emit_session_end();
        self.memory.on_session_end(&self.session_id);

        self.session_id = Uuid::new_v4().to_string();
        self.conversation_history.clear();
        self.turn_count = 0;
        self.api_call_count = 0;
        self.iteration_budget = IterationBudget::new(self.iteration_budget.max_total);
        self.budget_grace_call = false;

        // Start new session
        if let Some(ref db) = self.session_db {
            let _ = db.upsert_session(&self.session_id, None);
        }

        self.emit_session_start();
    }
}

/// Build the system prompt for the agent
pub fn build_system_prompt(
    identity: &Identity,
    skill_manager: &SkillManager,
    memory_manager: &MemoryManager,
    enable_reasoning: bool,
) -> Result<String> {
    let mut parts = vec![];

    // Identity-derived base prompt
    parts.push(identity.to_system_prompt());

    if enable_reasoning {
        parts.push("Use reasoning before tool calls when helpful. Think step by step.".to_string());
    }

    // Add skill index
    if let Ok(skill_index) = skill_manager.build_skill_index_prompt() {
        if !skill_index.is_empty() {
            parts.push(skill_index);
        }
    }

    // Add memory system prompt blocks
    let memory_prompt = memory_manager.prefetch_all("system_prompt", "");
    if !memory_prompt.is_empty() {
        parts.push(format!("## Memory Context\n{}", memory_prompt));
    }

    parts.push("When you need to use a tool, respond with a tool call. Otherwise respond directly.".to_string());

    Ok(parts.join("\n\n"))
}
