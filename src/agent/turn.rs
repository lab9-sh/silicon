//! First-turn assembly, tool dispatch, budget/large-result decisions,
//! and the hydrogen-backed streaming agent loop.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use futures_util::StreamExt;
use hydrogen::types::ToolUseBlock;
use hydrogen::{
    AnthropicConfig, Client, ContentBlock, Conversation, Event, RequestOptions, Response, Role,
    StopReason, ThinkingEffort, ToolOutput, Usage,
};
use tokio::sync::{mpsc, oneshot};

use super::archive::{
    append_memory, one_sentence, write_archive_layout, ArchiveResult,
};
use super::events::{
    AgentEvent, BudgetDecision, BudgetPauseEvent, LargeResultDecision, LargeToolResultEvent,
    LargeToolResultReply, TextDeltaEvent, ToolRecord, ToolResultEvent, ToolStartEvent, UsageEvent,
};
use crate::tools::{
    estimate_tokens, run_bash, run_bash_cancellable, run_edit, session_tools, BashInput,
    BashOutcome, EditInput, LARGE_TOOL_RESULT_TOKENS,
};

pub const DEFAULT_BUDGET: u64 = 200_000;
pub const BUDGET_INCREMENT: u64 = 100_000;
pub const DEFAULT_MAX_TOKENS: u32 = 128_000;
pub const DEFAULT_MODEL: &str = "claude-sonnet-5";
pub const DEFAULT_EFFORT: &str = "medium";

const ARCHIVE_SUMMARY_SYSTEM: &str = "You summarize coding-agent sessions in exactly one sentence.\n\
Reply with a single plain sentence only — no quotes, no bullet points, no preamble.";

const ARCHIVE_SUMMARY_INSTRUCTION: &str = "Summarize this coding-agent session in exactly one sentence, written for your future self as a memory note: what was asked, what you changed, and the outcome.\n\
Reply with a single plain sentence only — no quotes, no bullet points, no preamble. Do not call any tools.";

const ARCHIVE_SUMMARY_MAX_TOKENS: u32 = 4096;
const CACHE_FRESH_WINDOW: Duration = Duration::from_secs(4 * 60);

/// One-shot model completion used by Archive. Tests inject a stub.
pub type CompleteFn = Arc<dyn Fn(String, String) -> Result<String, String> + Send + Sync>;

/// A decoded tool call ready to run (display already known for ToolStart).
#[derive(Debug, Clone)]
pub enum PreparedTool {
    Bash { command: String },
    Edit { input: EditInput },
}

/// Total context-window tokens from a hydrogen usage report (matches Oxygen).
pub fn context_tokens(u: &Usage) -> u64 {
    u.total_input_tokens() as u64 + u.output_tokens as u64
}

/// Whether the agent should pause for a budget decision.
pub fn decide_budget_pause(ctx_tokens: u64, budget: u64) -> bool {
    ctx_tokens >= budget
}

/// Apply the user's budget choice. `continue_turn == true` → +100k budget.
pub fn apply_budget_continue(budget: u64, continue_turn: bool) -> BudgetDecision {
    if continue_turn {
        BudgetDecision::Continue {
            new_budget: budget + BUDGET_INCREMENT,
        }
    } else {
        BudgetDecision::Stop
    }
}

/// Whether a tool result should pause for large-result approval.
pub fn is_large_tool_result(tokens: usize) -> bool {
    tokens > LARGE_TOOL_RESULT_TOKENS
}

/// Map the user's large-result reply into a pure decision.
pub fn decide_large_tool_result(reply: LargeToolResultReply) -> LargeResultDecision {
    if reply.approve {
        LargeResultDecision::Approve
    } else {
        LargeResultDecision::Deny {
            message: reply.message,
        }
    }
}

/// Optional first-turn context blocks: README.md then `.si/memory.md`.
pub fn first_turn_context_blocks(cwd: &Path) -> Vec<String> {
    let mut blocks = Vec::new();
    if let Some(readme) = read_optional_file(&cwd.join("README.md")) {
        blocks.push(format!(
            "Here is the project's README.md for context:\n\n{readme}"
        ));
    }
    if let Some(mem) = read_optional_file(&cwd.join(".si").join("memory.md")) {
        blocks.push(format!("Here is memory from prior sessions:\n\n{mem}"));
    }
    blocks
}

/// Shipped context-assembly path used by `RunTurn`.
///
/// When `history_len == 0` (first turn): README then memory (each if present)
/// followed by `prompt`. When `history_len > 0`: only the prompt.
pub fn assemble_user_message_texts(cwd: &Path, history_len: usize, prompt: &str) -> Vec<String> {
    if history_len == 0 {
        let mut parts = first_turn_context_blocks(cwd);
        parts.push(prompt.to_string());
        parts
    } else {
        vec![prompt.to_string()]
    }
}

/// Full ordered text parts of the first user message.
pub fn first_turn_user_texts(cwd: &Path, prompt: &str) -> Vec<String> {
    assemble_user_message_texts(cwd, 0, prompt)
}

fn read_optional_file(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// Optional host-environment block from `cwd/.si/config/host.md`.
///
/// Injected into the system prompt between the Silicon intro and the
/// "You are chatting with…" line. Missing or empty files yield `None`.
pub fn load_host_config(cwd: &Path) -> Option<String> {
    let raw = read_optional_file(&cwd.join(".si").join("config").join("host.md"))?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Resolve model id from env (`SILICON_MODEL` or `OXYGEN_MODEL`) or default.
pub fn resolve_model() -> String {
    for key in ["SILICON_MODEL", "OXYGEN_MODEL"] {
        if let Ok(m) = std::env::var(key) {
            let m = m.trim().to_string();
            if !m.is_empty() {
                return m;
            }
        }
    }
    DEFAULT_MODEL.into()
}

/// Default model identity line when `SILICON_MODEL_INTRO` / `OXYGEN_MODEL_INTRO`
/// are unset.
pub const DEFAULT_MODEL_INTRO: &str = "You are Si, a coding agent.";

/// Resolve model identity intro from env (`SILICON_MODEL_INTRO` or
/// `OXYGEN_MODEL_INTRO`) or default.
///
/// Example override: `You are Claude, a large language model created by Anthropic.`
pub fn resolve_model_intro() -> String {
    for key in ["SILICON_MODEL_INTRO", "OXYGEN_MODEL_INTRO"] {
        if let Ok(s) = std::env::var(key) {
            let s = s.trim().to_string();
            if !s.is_empty() {
                return s;
            }
        }
    }
    DEFAULT_MODEL_INTRO.into()
}

/// Resolve effort from env (`SILICON_EFFORT` or `OXYGEN_EFFORT`) or default.
pub fn resolve_effort() -> String {
    for key in ["SILICON_EFFORT", "OXYGEN_EFFORT"] {
        if let Ok(e) = std::env::var(key) {
            let e = e.trim().to_lowercase();
            if matches!(e.as_str(), "low" | "medium" | "high") {
                return e;
            }
        }
    }
    DEFAULT_EFFORT.into()
}

fn thinking_effort(s: &str) -> ThinkingEffort {
    match s.trim().to_lowercase().as_str() {
        "low" => ThinkingEffort::Low,
        "high" => ThinkingEffort::High,
        _ => ThinkingEffort::Medium,
    }
}

/// Coding agent driven by hydrogen (Anthropic).
pub struct Agent {
    client: Client,
    model: String,
    max_tokens: u32,
    effort: String,
    cwd: PathBuf,
    conv: Conversation,
    budget: u64,
    tool_records: Vec<ToolRecord>,
    last_request_at: Option<Instant>,
    complete_fn: Option<CompleteFn>,
    /// Optional transcript texts for compact summary (when not using live conv).
    session_notes: Vec<String>,
}

impl Agent {
    pub fn new(api_key: &str, cwd: impl Into<PathBuf>, model: &str, effort: &str) -> Self {
        let model = if model.is_empty() {
            DEFAULT_MODEL.to_string()
        } else {
            model.to_string()
        };
        let effort = {
            let e = effort.trim().to_lowercase();
            if matches!(e.as_str(), "low" | "medium" | "high") {
                e
            } else {
                DEFAULT_EFFORT.into()
            }
        };
        let client = Client::anthropic(AnthropicConfig::new(api_key));
        Self {
            client,
            model,
            max_tokens: DEFAULT_MAX_TOKENS,
            effort,
            cwd: cwd.into(),
            conv: Conversation::new(),
            budget: DEFAULT_BUDGET,
            tool_records: Vec::new(),
            last_request_at: None,
            complete_fn: None,
            session_notes: Vec::new(),
        }
    }

    pub fn budget(&self) -> u64 {
        self.budget
    }
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }
    pub fn model(&self) -> &str {
        &self.model
    }
    pub fn effort(&self) -> &str {
        &self.effort
    }
    pub fn tool_records(&self) -> &[ToolRecord] {
        &self.tool_records
    }
    pub fn history_len(&self) -> usize {
        self.conv.messages().len()
    }

    /// Override archive completion (tests). Pass `None` to restore live API.
    pub fn set_complete_fn(&mut self, fn_: Option<CompleteFn>) {
        self.complete_fn = fn_;
    }

    /// Record a tool call (tests / internal).
    pub fn record_tool(
        &mut self,
        id: impl Into<String>,
        name: impl Into<String>,
        input: impl Into<String>,
        response: impl Into<String>,
        is_error: bool,
    ) {
        self.tool_records.push(ToolRecord {
            id: id.into(),
            name: name.into(),
            input: input.into(),
            response: response.into(),
            is_error,
        });
    }

    fn system_prompt(&self) -> String {
        let host = load_host_config(&self.cwd)
            .map(|s| format!("\n{s}\n"))
            .unwrap_or_default();
        format!(
            r#"
{}
You are running in Silicon, an agentic development environment.
Silicon is a small hobby project built by Randall.
Silicon may have some rough edges.
{}
You are chatting with: Randall
The current working directory is: {}"#,
            resolve_model_intro(),
            host,
            self.cwd.display()
        )
    }

    fn request_options(&self, max_tokens: u32) -> RequestOptions {
        RequestOptions {
            model: self.model.clone(),
            system: Some(self.system_prompt()),
            tools: session_tools(),
            max_tokens: Some(max_tokens),
            thinking: Some(thinking_effort(&self.effort)),
            ..Default::default()
        }
    }

    /// Decode a tool call into a display summary and a prepared call.
    /// Does **not** execute the tool — callers emit `ToolStart` then run.
    pub fn prepare_tool(&self, name: &str, raw: &str) -> Result<(String, PreparedTool), String> {
        match name {
            "bash" => {
                let input: BashInput =
                    serde_json::from_str(raw).map_err(|e| format!("invalid bash input: {e}"))?;
                let display = input.command.clone();
                Ok((display, PreparedTool::Bash { command: input.command }))
            }
            "edit_file" => {
                let input: EditInput = serde_json::from_str(raw)
                    .map_err(|e| format!("invalid edit_file input: {e}"))?;
                let display = input.display();
                Ok((display, PreparedTool::Edit { input }))
            }
            other => Err(format!("unknown tool: {other}")),
        }
    }

    /// Decode a tool call and return (display, runner). Errors become tool results.
    /// Runner is for tests; the turn loop uses [`Self::prepare_tool`] + execute.
    pub fn dispatch_tool(
        &self,
        name: &str,
        raw: &str,
    ) -> Result<(String, Box<dyn Fn() -> (String, bool) + Send>), String> {
        let (display, prepared) = self.prepare_tool(name, raw)?;
        let cwd = self.cwd.clone();
        Ok((
            display,
            Box::new(move || match prepared {
                PreparedTool::Bash { ref command } => {
                    let rt = tokio::runtime::Handle::try_current();
                    match rt {
                        Ok(h) => {
                            tokio::task::block_in_place(|| h.block_on(run_bash(&cwd, command)))
                        }
                        Err(_) => {
                            let rt = tokio::runtime::Builder::new_current_thread()
                                .enable_all()
                                .build()
                                .expect("runtime");
                            rt.block_on(run_bash(&cwd, command))
                        }
                    }
                }
                PreparedTool::Edit { ref input } => run_edit(&cwd, input),
            }),
        ))
    }

    /// Execute a prepared tool, racing bash against turn cancel.
    /// Returns `None` if the turn was cancelled mid-execution.
    async fn execute_prepared(
        &self,
        prepared: PreparedTool,
        cancel: &mut oneshot::Receiver<()>,
    ) -> Option<(String, bool)> {
        match prepared {
            PreparedTool::Bash { command } => {
                match run_bash_cancellable(&self.cwd, &command, Some(cancel)).await {
                    BashOutcome::Finished(out, is_err) => Some((out, is_err)),
                    BashOutcome::Cancelled => None,
                }
            }
            PreparedTool::Edit { input } => {
                // Edit is sync/fast; still honor a cancel that already fired.
                if cancel.try_recv().is_ok() {
                    return None;
                }
                Some(run_edit(&self.cwd, &input))
            }
        }
    }

    /// Drive one user turn until end_turn, budget stop, cancel, or error.
    /// Events are sent on `events`; the channel is not closed by this method.
    pub async fn run_turn(
        &mut self,
        prompt: &str,
        events: mpsc::Sender<AgentEvent>,
        mut cancel: oneshot::Receiver<()>,
    ) {
        self.budget = DEFAULT_BUDGET;
        let history_len = self.conv.messages().len();
        let parts = assemble_user_message_texts(&self.cwd, history_len, prompt);
        // Hydrogen accepts a single user text blob; join multi-block first turn.
        let user_text = parts.join("\n\n");
        self.conv.push_user(user_text);
        self.session_notes.push(format!("user: {prompt}"));

        loop {
            if cancel.try_recv().is_ok() {
                let _ = events
                    .send(AgentEvent::TurnDone {
                        err: None,
                        cancelled: true,
                    })
                    .await;
                return;
            }

            let opts = self.request_options(self.max_tokens);
            self.last_request_at = Some(Instant::now());

            let stream = match self.client.stream(&self.conv, &opts).await {
                Ok(s) => s,
                Err(e) => {
                    let _ = events
                        .send(AgentEvent::TurnDone {
                            err: Some(e.to_string()),
                            cancelled: false,
                        })
                        .await;
                    return;
                }
            };

            let mut stream = stream;
            let mut final_resp: Option<Response> = None;

            loop {
                tokio::select! {
                    _ = &mut cancel => {
                        let _ = events.send(AgentEvent::TurnDone {
                            err: None,
                            cancelled: true,
                        }).await;
                        return;
                    }
                    next = stream.next() => {
                        match next {
                            Some(Ok(Event::TextDelta(t))) if !t.is_empty() => {
                                let _ = events.send(AgentEvent::TextDelta(TextDeltaEvent { text: t })).await;
                            }
                            Some(Ok(Event::Done(resp))) => {
                                final_resp = Some(resp);
                                break;
                            }
                            Some(Ok(_)) => {}
                            Some(Err(e)) => {
                                let _ = events.send(AgentEvent::TurnDone {
                                    err: Some(e.to_string()),
                                    cancelled: false,
                                }).await;
                                return;
                            }
                            None => break,
                        }
                    }
                }
            }

            let resp = match final_resp {
                Some(r) => r,
                None => {
                    let _ = events
                        .send(AgentEvent::TurnDone {
                            err: Some("stream ended without Done event".into()),
                            cancelled: false,
                        })
                        .await;
                    return;
                }
            };

            let ctx = context_tokens(&resp.usage);
            let _ = events
                .send(AgentEvent::Usage(UsageEvent {
                    context_tokens: ctx,
                    budget: self.budget,
                    input_tokens: resp.usage.input_tokens as u64,
                    cache_create: resp.usage.cache_creation_input_tokens as u64,
                    cache_read: resp.usage.cache_read_input_tokens as u64,
                    output_tokens: resp.usage.output_tokens as u64,
                }))
                .await;

            let stop = resp.stop_reason.clone();
            let tool_uses: Vec<ToolUseBlock> = resp
                .message
                .content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::ToolUse(t) => Some(t.clone()),
                    _ => None,
                })
                .collect();

            // Persist assistant turn into conversation.
            self.conv.push_response(resp);

            match stop {
                StopReason::EndTurn | StopReason::Refusal => {
                    let _ = events
                        .send(AgentEvent::TurnDone {
                            err: None,
                            cancelled: false,
                        })
                        .await;
                    return;
                }
                StopReason::MaxTokens => {
                    let _ = events
                        .send(AgentEvent::TurnDone {
                            err: Some("stopped: max_tokens reached".into()),
                            cancelled: false,
                        })
                        .await;
                    return;
                }
                StopReason::ToolUse => {
                    // fall through
                }
                StopReason::Other(s) => {
                    if tool_uses.is_empty() {
                        let _ = events
                            .send(AgentEvent::TurnDone {
                                err: None,
                                cancelled: false,
                            })
                            .await;
                        return;
                    }
                    let _ = s;
                }
            }

            if tool_uses.is_empty() {
                let _ = events
                    .send(AgentEvent::TurnDone {
                        err: None,
                        cancelled: false,
                    })
                    .await;
                return;
            }

            // Budget pause before running tools (matches Oxygen order).
            if decide_budget_pause(ctx, self.budget) {
                let (tx, rx) = oneshot::channel();
                let _ = events
                    .send(AgentEvent::BudgetPause {
                        event: BudgetPauseEvent {
                            context_tokens: ctx,
                            budget: self.budget,
                        },
                        reply: tx,
                    })
                    .await;
                let cont = tokio::select! {
                    _ = &mut cancel => {
                        self.append_cancelled_tools(&tool_uses, "cancelled by user");
                        let _ = events.send(AgentEvent::TurnDone {
                            err: None,
                            cancelled: true,
                        }).await;
                        return;
                    }
                    r = rx => r.unwrap_or(false),
                };
                match apply_budget_continue(self.budget, cont) {
                    BudgetDecision::Stop => {
                        self.append_cancelled_tools(
                            &tool_uses,
                            "cancelled: user stopped at context budget",
                        );
                        let _ = events
                            .send(AgentEvent::TurnDone {
                                err: None,
                                cancelled: false,
                            })
                            .await;
                        return;
                    }
                    BudgetDecision::Continue { new_budget } => {
                        self.budget = new_budget;
                        let _ = events
                            .send(AgentEvent::Usage(UsageEvent {
                                context_tokens: ctx,
                                budget: self.budget,
                                input_tokens: 0,
                                cache_create: 0,
                                cache_read: 0,
                                output_tokens: 0,
                            }))
                            .await;
                    }
                }
            }

            for (idx, tu) in tool_uses.iter().enumerate() {
                // Match Oxygen: honor cancel before each tool in the batch.
                if cancel.try_recv().is_ok() {
                    self.append_cancelled_tools(&tool_uses[idx..], "cancelled by user");
                    let _ = events
                        .send(AgentEvent::TurnDone {
                            err: None,
                            cancelled: true,
                        })
                        .await;
                    return;
                }

                let raw = tu.input.to_string();
                let (display, prepared) = match self.prepare_tool(&tu.name, &raw) {
                    Ok(v) => v,
                    Err(e) => {
                        self.record_tool(&tu.id, &tu.name, &raw, &e, true);
                        self.conv
                            .push_tool_result(&tu.id, ToolOutput::Error(e.clone()));
                        let _ = events
                            .send(AgentEvent::ToolResult(ToolResultEvent {
                                output: e,
                                is_error: true,
                            }))
                            .await;
                        continue;
                    }
                };

                // ToolStart before execution so the TUI shows activity while running.
                let _ = events
                    .send(AgentEvent::ToolStart(ToolStartEvent {
                        name: tu.name.clone(),
                        command: display.clone(),
                    }))
                    .await;

                let Some((out, is_err)) = self.execute_prepared(prepared, &mut cancel).await
                else {
                    // Cancelled mid-tool (e.g. long bash). Cancel this + remaining.
                    self.append_cancelled_tools(&tool_uses[idx..], "cancelled by user");
                    let _ = events
                        .send(AgentEvent::TurnDone {
                            err: None,
                            cancelled: true,
                        })
                        .await;
                    return;
                };

                let tokens = estimate_tokens(&out);
                let final_out = out;
                let final_err = is_err;

                if is_large_tool_result(tokens) {
                    let (tx, rx) = oneshot::channel();
                    let _ = events
                        .send(AgentEvent::LargeToolResult {
                            event: LargeToolResultEvent {
                                command: display.clone(),
                                tokens,
                                bytes: final_out.len(),
                            },
                            reply: tx,
                        })
                        .await;
                    let reply = tokio::select! {
                        _ = &mut cancel => {
                            self.append_cancelled_tools(&tool_uses[idx..], "cancelled by user");
                            let _ = events.send(AgentEvent::TurnDone {
                                err: None,
                                cancelled: true,
                            }).await;
                            return;
                        }
                        r = rx => r.unwrap_or(LargeToolResultReply {
                            approve: false,
                            message: "cancelled by user".into(),
                        }),
                    };
                    match decide_large_tool_result(reply) {
                        LargeResultDecision::Deny { message } => {
                            let _ = events
                                .send(AgentEvent::ToolResult(ToolResultEvent {
                                    output: message.clone(),
                                    is_error: true,
                                }))
                                .await;
                            self.record_tool(&tu.id, &tu.name, &raw, &message, true);
                            self.conv
                                .push_tool_result(&tu.id, ToolOutput::Error(message));
                            continue;
                        }
                        LargeResultDecision::Approve => {}
                    }
                }

                let _ = events
                    .send(AgentEvent::ToolResult(ToolResultEvent {
                        output: final_out.clone(),
                        is_error: final_err,
                    }))
                    .await;
                self.record_tool(&tu.id, &tu.name, &raw, &final_out, final_err);
                let output = if final_err {
                    ToolOutput::Error(final_out)
                } else {
                    ToolOutput::Text(final_out)
                };
                self.conv.push_tool_result(&tu.id, output);
            }
            // Continue the tool loop: stream again with tool results in conv.
        }
    }

    fn append_cancelled_tools(&mut self, tools: &[ToolUseBlock], reason: &str) {
        for tu in tools {
            // Only record tools not yet answered. Hydrogen coalesces tool results;
            // if we already pushed some, still ok to push cancel for remaining.
            let raw = tu.input.to_string();
            self.record_tool(&tu.id, &tu.name, &raw, reason, true);
            self.conv
                .push_tool_result(&tu.id, ToolOutput::Error(reason.into()));
        }
    }

    /// Archive: summarize, append memory, write tool logs.
    pub async fn archive(&mut self) -> Result<ArchiveResult, String> {
        let summary = self.summarize_session().await?;
        let summary = one_sentence(&summary);
        if summary.is_empty() {
            return Err("empty session summary from model".into());
        }
        let now = SystemTime::now();
        let mem_path = append_memory(&self.cwd, now, &summary)?;
        let log_dir = write_archive_layout(&self.cwd, now, &summary, &self.tool_records)?;
        Ok(ArchiveResult {
            summary,
            log_dir,
            memory: mem_path,
        })
    }

    async fn summarize_session(&mut self) -> Result<String, String> {
        if self.can_summarize_from_history() {
            match self.summarize_from_history().await {
                Ok(s) if !s.trim().is_empty() => return Ok(s),
                Ok(_) => {}
                Err(_) => {}
            }
        }
        self.summarize_compact().await
    }

    fn can_summarize_from_history(&self) -> bool {
        if self.complete_fn.is_some() || self.conv.messages().is_empty() {
            return false;
        }
        match self.last_request_at {
            Some(t) => t.elapsed() < CACHE_FRESH_WINDOW,
            None => false,
        }
    }

    async fn summarize_from_history(&mut self) -> Result<String, String> {
        // Clone conversation, append instruction, send non-streaming.
        let mut conv = self.conv.clone();
        conv.push_user(ARCHIVE_SUMMARY_INSTRUCTION);
        let opts = self.request_options(ARCHIVE_SUMMARY_MAX_TOKENS);
        self.last_request_at = Some(Instant::now());
        let resp = self
            .client
            .send(&conv, &opts)
            .await
            .map_err(|e| e.to_string())?;
        Ok(response_text(&resp))
    }

    async fn summarize_compact(&self) -> Result<String, String> {
        let user = self.session_transcript_for_summary();
        let system = ARCHIVE_SUMMARY_SYSTEM.to_string();
        if let Some(ref fn_) = self.complete_fn {
            return fn_(system, user);
        }
        // Live compact completion via a throwaway conversation.
        let mut conv = Conversation::new();
        conv.push_user(user);
        let opts = RequestOptions {
            model: self.model.clone(),
            system: Some(system),
            max_tokens: Some(256),
            ..Default::default()
        };
        let resp = self
            .client
            .send(&conv, &opts)
            .await
            .map_err(|e| e.to_string())?;
        Ok(response_text(&resp))
    }

    fn session_transcript_for_summary(&self) -> String {
        let mut b = String::from("Summarize this coding-agent session in exactly one sentence.\n\n");
        if self.conv.messages().is_empty() && self.tool_records.is_empty() {
            b.push_str("(empty session — no user turns yet)\n");
            return b;
        }
        for (i, msg) in self.conv.messages().iter().enumerate() {
            let role = match msg.role {
                Role::User => "user",
                Role::Assistant => "assistant",
            };
            b.push_str(&format!("--- message {i} ({role}) ---\n"));
            for block in &msg.content {
                match block {
                    ContentBlock::Text(t) => {
                        let text = &t.text;
                        if text.starts_with("Here is the project's README.md for context:")
                            || text.starts_with("Here is memory from prior sessions:")
                        {
                            b.push_str("[injected context omitted]\n");
                            continue;
                        }
                        let text = if text.len() > 2000 {
                            format!("{}…", &text[..2000])
                        } else {
                            text.clone()
                        };
                        b.push_str(&text);
                        b.push('\n');
                    }
                    ContentBlock::ToolUse(t) => {
                        b.push_str(&format!("[tool_use name={} id={}]\n", t.name, t.id));
                    }
                    ContentBlock::ToolResult(t) => {
                        b.push_str(&format!("[tool_result id={}]\n", t.id));
                    }
                    ContentBlock::Reasoning(_) => {}
                }
            }
        }
        if !self.tool_records.is_empty() {
            b.push_str(&format!(
                "\nTool calls this session: {}\n",
                self.tool_records.len()
            ));
            for rec in &self.tool_records {
                b.push_str(&format!("- {} ({})\n", rec.name, rec.id));
            }
        }
        b
    }
}

fn response_text(resp: &Response) -> String {
    let mut s = String::new();
    for block in &resp.message.content {
        if let ContentBlock::Text(t) = block {
            s.push_str(&t.text);
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn write_file(path: &Path, content: &str) {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn load_host_config_reads_si_config_host_md() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            &dir.path().join(".si").join("config").join("host.md"),
            "\n## Host Tools\n\n`rg`, `fd`\n\n",
        );
        let got = load_host_config(dir.path()).unwrap();
        assert_eq!(got, "## Host Tools\n\n`rg`, `fd`");
        assert!(load_host_config(tempfile::tempdir().unwrap().path()).is_none());
    }

    #[test]
    fn system_prompt_includes_host_config_when_present() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            &dir.path().join(".si").join("config").join("host.md"),
            "## Host Tools\n\n`rg`\n\n## Host Languages\n\nRust 1.97\n",
        );
        let a = Agent::new("", dir.path(), "", "");
        let sp = a.system_prompt();
        assert!(sp.contains("rough edges."), "{sp}");
        assert!(sp.contains("## Host Tools") && sp.contains("`rg`"), "{sp}");
        assert!(sp.contains("## Host Languages") && sp.contains("Rust 1.97"), "{sp}");
        assert!(sp.contains("You are chatting with: Randall"), "{sp}");
        // Host block sits between intro and chatting-with line.
        let rough = sp.find("rough edges.").unwrap();
        let host = sp.find("## Host Tools").unwrap();
        let chat = sp.find("You are chatting with:").unwrap();
        assert!(rough < host && host < chat, "{sp}");
    }

    #[test]
    fn system_prompt_omits_host_section_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let a = Agent::new("", dir.path(), "", "");
        let sp = a.system_prompt();
        assert!(sp.contains("rough edges."), "{sp}");
        assert!(!sp.contains("## Host Tools"), "{sp}");
        assert!(!sp.contains("## Host Languages"), "{sp}");
        assert!(sp.contains("You are chatting with: Randall"), "{sp}");
    }

    #[test]
    fn first_turn_readme_then_memory_then_prompt() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("README.md"), "README BODY");
        write_file(&dir.path().join(".si").join("memory.md"), "MEMORY BODY");

        let got = first_turn_user_texts(dir.path(), "user prompt here");
        assert_eq!(got.len(), 3, "{got:?}");
        assert!(got[0].contains("README.md") && got[0].contains("README BODY"), "{}", got[0]);
        assert!(got[1].contains("memory") && got[1].contains("MEMORY BODY"), "{}", got[1]);
        assert_eq!(got[2], "user prompt here");
    }

    #[test]
    fn first_turn_memory_only() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join(".si").join("memory.md"), "only memory");
        let got = first_turn_user_texts(dir.path(), "hi");
        assert_eq!(got.len(), 2);
        assert!(got[0].contains("only memory"));
        assert_eq!(got[1], "hi");
    }

    #[test]
    fn first_turn_neither() {
        let dir = tempfile::tempdir().unwrap();
        let got = first_turn_user_texts(dir.path(), "just the prompt");
        assert_eq!(got, vec!["just the prompt".to_string()]);
    }

    #[test]
    fn later_turn_does_not_reinject_context() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("README.md"), "README");
        write_file(&dir.path().join(".si").join("memory.md"), "MEM");

        let first = assemble_user_message_texts(dir.path(), 0, "first");
        assert_eq!(first.len(), 3);

        let later = assemble_user_message_texts(dir.path(), 2, "second turn");
        assert_eq!(later, vec!["second turn".to_string()]);
    }

    #[test]
    fn budget_pause_and_continue_stop() {
        assert!(decide_budget_pause(200_000, DEFAULT_BUDGET));
        assert!(decide_budget_pause(200_001, DEFAULT_BUDGET));
        assert!(!decide_budget_pause(199_999, DEFAULT_BUDGET));

        assert_eq!(
            apply_budget_continue(DEFAULT_BUDGET, true),
            BudgetDecision::Continue {
                new_budget: DEFAULT_BUDGET + BUDGET_INCREMENT
            }
        );
        assert_eq!(
            apply_budget_continue(DEFAULT_BUDGET, false),
            BudgetDecision::Stop
        );
    }

    #[test]
    fn large_result_approve_deny() {
        assert!(!is_large_tool_result(50_000));
        assert!(is_large_tool_result(50_001));

        assert_eq!(
            decide_large_tool_result(LargeToolResultReply {
                approve: true,
                message: String::new(),
            }),
            LargeResultDecision::Approve
        );
        assert_eq!(
            decide_large_tool_result(LargeToolResultReply {
                approve: false,
                message: "use head".into(),
            }),
            LargeResultDecision::Deny {
                message: "use head".into()
            }
        );
    }

    #[test]
    fn context_tokens_sums_usage() {
        let u = Usage {
            input_tokens: 10,
            output_tokens: 5,
            cache_creation_input_tokens: 2,
            cache_read_input_tokens: 3,
        };
        assert_eq!(context_tokens(&u), 20);
    }

    #[test]
    fn prepare_tool_returns_display_without_running() {
        // prepare_tool is the shipped pre-execution step: display is available
        // for ToolStart before any process or filesystem write.
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("should-not-exist-yet");
        let a = Agent::new("", dir.path(), "", "");

        let (display, prepared) = a
            .prepare_tool(
                "edit_file",
                r#"{"path":"should-not-exist-yet","old_string":"","new_string":"x"}"#,
            )
            .unwrap();
        assert_eq!(display, "should-not-exist-yet (create)");
        assert!(!marker.exists(), "prepare must not create the file");
        match prepared {
            PreparedTool::Edit { .. } => {}
            other => panic!("expected Edit, got {other:?}"),
        }

        let (display, prepared) = a
            .prepare_tool("bash", r#"{"command":"echo prepare-ok"}"#)
            .unwrap();
        assert_eq!(display, "echo prepare-ok");
        match prepared {
            PreparedTool::Bash { command } => assert_eq!(command, "echo prepare-ok"),
            other => panic!("expected Bash, got {other:?}"),
        }
    }

    #[test]
    fn tool_start_before_execute_order_on_shipped_path() {
        // Drives prepare_tool → ToolStart-shaped event → execute_prepared,
        // the same order run_turn uses, asserting start is recorded before
        // the tool mutates the filesystem.
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("a.txt"), "alpha\n");
        let a = Agent::new("", dir.path(), "", "");

        let raw = r#"{"path":"a.txt","old_string":"alpha","new_string":"beta"}"#;
        let (display, prepared) = a.prepare_tool("edit_file", raw).unwrap();

        let mut events: Vec<String> = Vec::new();
        // ToolStart (as run_turn emits) — before execute.
        events.push(format!("start:{display}"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "alpha\n",
            "file must be unchanged until after ToolStart"
        );

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (out, is_err) = rt.block_on(async {
            let (_tx, mut cancel) = oneshot::channel::<()>();
            a.execute_prepared(prepared, &mut cancel)
                .await
                .expect("not cancelled")
        });
        events.push(format!("result:{is_err}"));
        assert!(!is_err, "{out}");
        assert!(out.contains("edited"), "{out}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "beta\n"
        );
        assert_eq!(
            events,
            vec![
                "start:a.txt (replace)".to_string(),
                "result:false".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn execute_prepared_bash_respects_cancel() {
        let a = Agent::new("", tempfile::tempdir().unwrap().path(), "", "");
        let (tx, mut rx) = oneshot::channel();
        let prepared = PreparedTool::Bash {
            command: "sleep 30".into(),
        };
        let run = tokio::spawn(async move {
            // Need agent cwd inside async — rebuild prepared path via execute on new agent.
            let a2 = Agent::new("", std::env::temp_dir(), "", "");
            a2.execute_prepared(prepared, &mut rx).await
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        let _ = tx.send(());
        let result = run.await.unwrap();
        assert!(result.is_none(), "expected cancel mid-bash, got {result:?}");
        let _ = a; // keep agent construction in test surface
    }

    #[test]
    fn dispatch_tool_bash() {
        let dir = tempfile::tempdir().unwrap();
        let a = Agent::new("", dir.path(), "", "");
        let (display, run) = a
            .dispatch_tool("bash", r#"{"command":"echo dispatch-ok"}"#)
            .unwrap();
        assert_eq!(display, "echo dispatch-ok");
        let (out, is_err) = run();
        assert!(!is_err, "{out}");
        assert!(out.contains("dispatch-ok"), "{out}");
    }

    #[test]
    fn dispatch_tool_edit_file() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("a.txt"), "alpha\n");
        let a = Agent::new("", dir.path(), "", "");
        let (display, run) = a
            .dispatch_tool(
                "edit_file",
                r#"{"path":"a.txt","old_string":"alpha","new_string":"beta"}"#,
            )
            .unwrap();
        assert_eq!(display, "a.txt (replace)");
        let (out, is_err) = run();
        assert!(!is_err, "{out}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "beta\n"
        );
    }

    #[test]
    fn dispatch_tool_errors() {
        let a = Agent::new("", tempfile::tempdir().unwrap().path(), "", "");
        match a.dispatch_tool("nope", "{}") {
            Err(err) => assert!(err.contains("unknown tool"), "{err}"),
            Ok(_) => panic!("expected unknown tool error"),
        }
        match a.dispatch_tool("bash", "{not json") {
            Err(err) => assert!(err.contains("invalid bash input"), "{err}"),
            Ok(_) => panic!("expected bash decode error"),
        }
        match a.dispatch_tool("edit_file", "{not json") {
            Err(err) => assert!(err.contains("invalid edit_file input"), "{err}"),
            Ok(_) => panic!("expected edit decode error"),
        }
    }

    #[test]
    fn record_tool_accumulates() {
        let mut a = Agent::new("", tempfile::tempdir().unwrap().path(), "", "");
        a.record_tool("id1", "bash", r#"{"command":"ls"}"#, "out", false);
        a.record_tool("id2", "bash", r#"{"command":"pwd"}"#, "/tmp", false);
        assert_eq!(a.tool_records().len(), 2);
        assert_eq!(a.tool_records()[0].id, "id1");
        assert_eq!(a.tool_records()[1].response, "/tmp");
    }

    #[tokio::test]
    async fn archive_orchestration_with_stubbed_model() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join(".si").join("memory.md"), "old memory line\n");

        let mut a = Agent::new("", dir.path(), "test-model", "");
        a.record_tool(
            "call_1",
            "bash",
            r#"{"command":"echo hi"}"#,
            "hi",
            false,
        );
        // Plant session notes for compact summary path.
        a.session_notes.push("user: list the files".into());
        a.conv.push_user("list the files");

        let saw = Arc::new(Mutex::new((String::new(), String::new())));
        let saw2 = saw.clone();
        const SUMMARY: &str = "User listed repository files with bash.";
        a.set_complete_fn(Some(Arc::new(move |system, user| {
            *saw2.lock().unwrap() = (system, user);
            Ok(SUMMARY.into())
        })));

        let res = a.archive().await.unwrap();
        assert_eq!(res.summary, SUMMARY);

        let (sys, user) = saw.lock().unwrap().clone();
        assert!(!sys.is_empty() && !user.is_empty());
        assert!(user.contains("list the files"), "{user}");

        let mem = std::fs::read_to_string(&res.memory).unwrap();
        assert!(mem.contains("old memory line") && mem.contains(SUMMARY), "{mem}");
        // datetime prefix
        assert!(mem.lines().any(|l| {
            l.len() >= 16 && l.contains(SUMMARY) && l.as_bytes()[4] == b'-'
        }), "{mem}");

        assert!(res.log_dir.exists());
        let tool_path = res.log_dir.join("tool").join("call_1.md");
        let body = std::fs::read_to_string(&tool_path).unwrap();
        assert!(body.contains(r#"{"command":"echo hi"}"#) && body.contains("hi"), "{body}");

        let base = res.log_dir.file_name().unwrap().to_string_lossy();
        let parts: Vec<_> = base.splitn(3, '-').collect();
        assert!(parts.len() >= 3, "{base}");
        assert_eq!(parts[0].len(), 8);
        assert_eq!(parts[1].len(), 6);
        assert!(base.contains(&super::super::archive::sanitize_dir_name(SUMMARY)));
    }
}
