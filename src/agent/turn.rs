//! Agent struct, tool prepare/execute, and the streaming turn loop.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use futures_util::StreamExt;
use hydrogen::types::ToolUseBlock;
use hydrogen::{
    AnthropicConfig, Client, ContentBlock, Conversation, Event, RequestOptions, Response,
    StopReason, ThinkingEffort, ToolOutput,
};
use tokio::sync::{mpsc, oneshot};

use super::config::{
    load_host_config, resolve_model_intro, DEFAULT_BUDGET, DEFAULT_EFFORT, DEFAULT_MAX_TOKENS,
    DEFAULT_MODEL,
};
use super::context::assemble_user_message_texts;
use super::events::{
    AgentEvent, BudgetDecision, BudgetPauseEvent, LargeResultDecision, LargeToolResultEvent,
    LargeToolResultReply, TextDeltaEvent, ToolRecord, ToolResultEvent, ToolStartEvent, UsageEvent,
};
use super::policy::{
    apply_budget_continue, context_tokens, decide_budget_pause, decide_large_tool_result,
    is_large_tool_result,
};
use crate::tools::{
    estimate_tokens, run_bash_cancellable, run_edit, session_tools, BashInput, BashOutcome,
    EditInput,
};

/// One-shot model completion used by Archive. Tests inject a stub.
pub type CompleteFn = Arc<dyn Fn(String, String) -> Result<String, String> + Send + Sync>;

/// A decoded tool call ready to run (display already known for ToolStart).
#[derive(Debug, Clone)]
pub enum PreparedTool {
    Bash { command: String },
    Edit { input: EditInput },
}

/// Coding agent driven by hydrogen (Anthropic).
pub struct Agent {
    pub(crate) client: Client,
    pub(crate) model: String,
    pub(crate) max_tokens: u32,
    pub(crate) effort: String,
    pub(crate) cwd: PathBuf,
    pub(crate) conv: Conversation,
    /// Session context soft-cap (starts at [`DEFAULT_BUDGET`]; +100k on continue).
    /// Not reset per user turn — raised budgets persist until the process ends.
    pub(crate) budget: u64,
    pub(crate) tool_records: Vec<ToolRecord>,
    pub(crate) last_request_at: Option<Instant>,
    pub(crate) complete_fn: Option<CompleteFn>,
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

    pub(crate) fn system_prompt(&self) -> String {
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

    pub(crate) fn request_options(&self, max_tokens: u32) -> RequestOptions {
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

    /// Execute a prepared tool, racing bash against turn cancel.
    /// Returns `None` if the turn was cancelled mid-execution.
    pub(crate) async fn execute_prepared(
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
    ///
    /// Context `budget` is session-scoped (see [`Agent::budget`]): it is not
    /// reset here. Continue (+100k) from a prior turn still applies.
    pub async fn run_turn(
        &mut self,
        prompt: &str,
        events: mpsc::Sender<AgentEvent>,
        mut cancel: oneshot::Receiver<()>,
    ) {
        let history_len = self.conv.messages().len();
        let parts = assemble_user_message_texts(&self.cwd, history_len, prompt);
        // Hydrogen accepts a single user text blob; join multi-block first turn.
        let user_text = parts.join("\n\n");
        self.conv.push_user(user_text);

        loop {
            if cancel.try_recv().is_ok() {
                emit_done(&events, None, true).await;
                return;
            }

            let opts = self.request_options(self.max_tokens);
            self.last_request_at = Some(Instant::now());

            let stream = match self.client.stream(&self.conv, &opts).await {
                Ok(s) => s,
                Err(e) => {
                    emit_done(&events, Some(e.to_string()), false).await;
                    return;
                }
            };

            let mut stream = stream;
            let mut final_resp: Option<Response> = None;

            loop {
                tokio::select! {
                    _ = &mut cancel => {
                        emit_done(&events, None, true).await;
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
                                emit_done(&events, Some(e.to_string()), false).await;
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
                    emit_done(
                        &events,
                        Some("stream ended without Done event".into()),
                        false,
                    )
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
                    emit_done(&events, None, false).await;
                    return;
                }
                StopReason::MaxTokens => {
                    emit_done(
                        &events,
                        Some("stopped: max_tokens reached".into()),
                        false,
                    )
                    .await;
                    return;
                }
                StopReason::ToolUse => {
                    // fall through
                }
                StopReason::Other(s) => {
                    if tool_uses.is_empty() {
                        emit_done(&events, None, false).await;
                        return;
                    }
                    let _ = s;
                }
            }

            if tool_uses.is_empty() {
                emit_done(&events, None, false).await;
                return;
            }

            // Session context soft-cap: pause before tools so the user can
            // raise the budget (+100k) or stop and start a fresh session.
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
                        cancel_remaining(self, &tool_uses, "cancelled by user", &events).await;
                        return;
                    }
                    r = rx => r.unwrap_or(false),
                };
                match apply_budget_continue(self.budget, cont) {
                    BudgetDecision::Stop => {
                        self.append_cancelled_tools(
                            &tool_uses,
                            "cancelled: user stopped at session context budget",
                        );
                        emit_done(&events, None, false).await;
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
                    cancel_remaining(self, &tool_uses[idx..], "cancelled by user", &events).await;
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
                    cancel_remaining(self, &tool_uses[idx..], "cancelled by user", &events).await;
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
                            cancel_remaining(self, &tool_uses[idx..], "cancelled by user", &events).await;
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
}

async fn emit_done(events: &mpsc::Sender<AgentEvent>, err: Option<String>, cancelled: bool) {
    let _ = events
        .send(AgentEvent::TurnDone { err, cancelled })
        .await;
}

async fn cancel_remaining(
    agent: &mut Agent,
    tools: &[ToolUseBlock],
    reason: &str,
    events: &mpsc::Sender<AgentEvent>,
) {
    agent.append_cancelled_tools(tools, reason);
    emit_done(events, None, true).await;
}

fn thinking_effort(s: &str) -> ThinkingEffort {
    match s.trim().to_lowercase().as_str() {
        "low" => ThinkingEffort::Low,
        "high" => ThinkingEffort::High,
        _ => ThinkingEffort::Medium,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::time::Duration;

    fn write_file(path: &Path, content: &str) {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).unwrap();
        }
        std::fs::write(path, content).unwrap();
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
    fn prepare_tool_errors() {
        let a = Agent::new("", tempfile::tempdir().unwrap().path(), "", "");
        match a.prepare_tool("nope", "{}") {
            Err(err) => assert!(err.contains("unknown tool"), "{err}"),
            Ok(_) => panic!("expected unknown tool error"),
        }
        match a.prepare_tool("bash", "{not json") {
            Err(err) => assert!(err.contains("invalid bash input"), "{err}"),
            Ok(_) => panic!("expected bash decode error"),
        }
        match a.prepare_tool("edit_file", "{not json") {
            Err(err) => assert!(err.contains("invalid edit_file input"), "{err}"),
            Ok(_) => panic!("expected edit decode error"),
        }
    }

    #[test]
    fn session_budget_starts_default_and_survives_continue() {
        // Budget is session-scoped: constructed at DEFAULT_BUDGET, raised by
        // continue, and not wiped between user turns (run_turn must not reset it).
        use super::super::config::BUDGET_INCREMENT;
        use super::super::policy::apply_budget_continue;
        use super::super::events::BudgetDecision;

        let mut a = Agent::new("", tempfile::tempdir().unwrap().path(), "", "");
        assert_eq!(a.budget(), DEFAULT_BUDGET);

        match apply_budget_continue(a.budget(), true) {
            BudgetDecision::Continue { new_budget } => a.budget = new_budget,
            BudgetDecision::Stop => panic!("expected continue"),
        }
        assert_eq!(a.budget(), DEFAULT_BUDGET + BUDGET_INCREMENT);

        // A second "turn boundary" must leave the raised budget intact.
        let raised = a.budget();
        assert_eq!(raised, DEFAULT_BUDGET + BUDGET_INCREMENT);
        assert_eq!(a.budget(), raised);
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
    async fn execute_prepared_bash_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let a = Agent::new("", dir.path(), "", "");
        let (_display, prepared) = a
            .prepare_tool("bash", r#"{"command":"echo prepare-exec-ok"}"#)
            .unwrap();
        let (_tx, mut cancel) = oneshot::channel::<()>();
        let (out, is_err) = a
            .execute_prepared(prepared, &mut cancel)
            .await
            .expect("not cancelled");
        assert!(!is_err, "{out}");
        assert!(out.contains("prepare-exec-ok"), "{out}");
    }

    #[tokio::test]
    async fn execute_prepared_edit_file() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("a.txt"), "alpha\n");
        let a = Agent::new("", dir.path(), "", "");
        let (display, prepared) = a
            .prepare_tool(
                "edit_file",
                r#"{"path":"a.txt","old_string":"alpha","new_string":"beta"}"#,
            )
            .unwrap();
        assert_eq!(display, "a.txt (replace)");
        let (_tx, mut cancel) = oneshot::channel::<()>();
        let (out, is_err) = a
            .execute_prepared(prepared, &mut cancel)
            .await
            .expect("not cancelled");
        assert!(!is_err, "{out}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "beta\n"
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
    fn record_tool_accumulates() {
        let mut a = Agent::new("", tempfile::tempdir().unwrap().path(), "", "");
        a.record_tool("id1", "bash", r#"{"command":"ls"}"#, "out", false);
        a.record_tool("id2", "bash", r#"{"command":"pwd"}"#, "/tmp", false);
        assert_eq!(a.tool_records().len(), 2);
        assert_eq!(a.tool_records()[0].id, "id1");
        assert_eq!(a.tool_records()[1].response, "/tmp");
    }
}
