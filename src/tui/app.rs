//! App state, agent event handling, background poll, and UI entrypoint.

use std::io::{self, stdout};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event, MouseEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::{mpsc, oneshot, Mutex};

use crate::agent::{is_archive_command, Agent, AgentEvent, ArchiveResult, LargeToolResultReply};

use super::format::{base_dir, format_bytes, format_tokens, tool_label, tool_prefix};

const INTERRUPT_WINDOW: Duration = Duration::from_secs(2);
const WELCOME: &str = "Silicon — coding agent (bash + edit_file).\n\
Enter to send · PgUp/PgDn or wheel to scroll · Ctrl+C to interrupt, again to quit.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Idle,
    Running,
    BudgetPause,
    LargeResult,
    Archiving,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LineKind {
    User,
    Agent,
    Tool,
    Meta,
    Error,
}

#[derive(Debug, Clone)]
pub(crate) struct StyledLine {
    pub(crate) kind: LineKind,
    pub(crate) text: String,
}

pub(crate) struct App {
    pub(crate) agent: Arc<Mutex<Agent>>,
    pub(crate) model_name: String,
    pub(crate) effort: String,
    pub(crate) dir_name: String,

    pub(crate) mode: Mode,
    pub(crate) status: String,
    pub(crate) messages: Vec<StyledLine>,
    pub(crate) stream_buf: String,
    pub(crate) ctx_line_shown: bool,

    pub(crate) input: String,
    pub(crate) scroll: u16,
    pub(crate) stick_bottom: bool,

    pub(crate) interrupt_at: Option<Instant>,

    pub(crate) event_rx: Option<mpsc::Receiver<AgentEvent>>,
    pub(crate) cancel_tx: Option<oneshot::Sender<()>>,
    pub(crate) budget_reply: Option<oneshot::Sender<bool>>,
    pub(crate) large_reply: Option<oneshot::Sender<LargeToolResultReply>>,
    pub(crate) archive_rx: Option<oneshot::Receiver<Result<ArchiveResult, String>>>,

    pub(crate) should_quit: bool,
}

impl App {
    pub(crate) fn new(agent: Agent) -> Self {
        let model_name = agent.model().to_string();
        let effort = agent.effort().to_string();
        let dir_name = base_dir(agent.cwd());
        let mut app = Self {
            agent: Arc::new(Mutex::new(agent)),
            model_name,
            effort,
            dir_name,
            mode: Mode::Idle,
            status: "ready".into(),
            messages: Vec::new(),
            stream_buf: String::new(),
            ctx_line_shown: false,
            input: String::new(),
            scroll: 0,
            stick_bottom: true,
            interrupt_at: None,
            event_rx: None,
            cancel_tx: None,
            budget_reply: None,
            large_reply: None,
            archive_rx: None,
            should_quit: false,
        };
        app.append(LineKind::Meta, WELCOME);
        app
    }

    pub(crate) fn append(&mut self, kind: LineKind, text: impl Into<String>) {
        self.messages.push(StyledLine {
            kind,
            text: text.into(),
        });
        if self.stick_bottom {
            self.scroll = u16::MAX;
        }
    }

    fn replace_last(&mut self, kind: LineKind, text: impl Into<String>) {
        if let Some(last) = self.messages.last_mut() {
            last.kind = kind;
            last.text = text.into();
        } else {
            self.append(kind, text);
        }
    }

    fn flush_stream(&mut self) {
        self.stream_buf.clear();
    }

    pub(crate) fn submit_prompt(&mut self, rt: &tokio::runtime::Handle) {
        let prompt = self.input.trim().to_string();
        if prompt.is_empty() {
            return;
        }
        self.input.clear();

        if is_archive_command(&prompt) {
            self.append(LineKind::User, format!("You: {prompt}"));
            self.append(LineKind::Meta, "Archiving session…");
            self.stream_buf.clear();
            self.mode = Mode::Archiving;
            self.status = "archiving…".into();
            self.interrupt_at = None;

            let agent = self.agent.clone();
            let (tx, rx) = oneshot::channel();
            self.archive_rx = Some(rx);
            rt.spawn(async move {
                let res = {
                    let mut a = agent.lock().await;
                    a.archive().await
                };
                let _ = tx.send(res);
            });
            return;
        }

        self.append(LineKind::User, format!("You: {prompt}"));
        self.stream_buf.clear();
        self.ctx_line_shown = false;
        self.mode = Mode::Running;
        self.status = "thinking…".into();
        self.interrupt_at = None;
        self.stick_bottom = true;

        let (ev_tx, ev_rx) = mpsc::channel(64);
        let (cancel_tx, cancel_rx) = oneshot::channel();
        self.event_rx = Some(ev_rx);
        self.cancel_tx = Some(cancel_tx);

        let agent = self.agent.clone();
        rt.spawn(async move {
            let mut a = agent.lock().await;
            a.run_turn(&prompt, ev_tx, cancel_rx).await;
        });
    }

    fn handle_agent_event(&mut self, ev: AgentEvent) {
        match ev {
            AgentEvent::TextDelta(e) => {
                if self.stream_buf.is_empty() {
                    self.append(LineKind::Agent, "Silicon: ");
                    self.ctx_line_shown = false;
                }
                self.stream_buf.push_str(&e.text);
                self.replace_last(LineKind::Agent, format!("Silicon: {}", self.stream_buf));
            }
            AgentEvent::ToolStart(e) => {
                self.flush_stream();
                let prefix = tool_prefix(&e.name);
                self.append(LineKind::Tool, format!("{prefix}{}", e.command));
                self.status = format!("running {}…", tool_label(&e.name));
            }
            AgentEvent::ToolResult(e) => {
                let kind = if e.is_error {
                    LineKind::Error
                } else {
                    LineKind::Meta
                };
                self.append(kind, e.output);
            }
            AgentEvent::Usage(e) => {
                self.status = format!(
                    "ctx {} / budget {}  (in {} · cache+ {} · cache↻ {} · out {})",
                    format_tokens(e.context_tokens),
                    format_tokens(e.budget),
                    e.input_tokens,
                    e.cache_create,
                    e.cache_read,
                    e.output_tokens
                );
                if !self.ctx_line_shown {
                    self.append(
                        LineKind::Meta,
                        format!("context: {} tokens", format_tokens(e.context_tokens)),
                    );
                    self.ctx_line_shown = true;
                }
            }
            AgentEvent::BudgetPause { event, reply } => {
                self.flush_stream();
                self.mode = Mode::BudgetPause;
                self.budget_reply = Some(reply);
                self.status = format!(
                    "budget pause · ctx {} / {} — [c]ontinue +100k / [s]top",
                    format_tokens(event.context_tokens),
                    format_tokens(event.budget)
                );
                self.append(
                    LineKind::Meta,
                    format!(
                        "Session context budget reached ({} / {}). Press c to continue (+100k for this session) or s to stop (start a fresh session for a clean window).",
                        format_tokens(event.context_tokens),
                        format_tokens(event.budget)
                    ),
                );
                self.stick_bottom = true;
            }
            AgentEvent::LargeToolResult { event, reply } => {
                self.flush_stream();
                self.mode = Mode::LargeResult;
                self.large_reply = Some(reply);
                self.input.clear();
                self.status = format!(
                    "large tool result · ~{} tokens ({}) — Ctrl+Y approve · Enter deny",
                    format_tokens(event.tokens as u64),
                    format_bytes(event.bytes)
                );
                self.append(
                    LineKind::Meta,
                    format!(
                        "Tool result is large (~{} tokens, {}) for: {}\nCtrl+Y to approve and send it, or type guidance and press Enter to deny.",
                        format_tokens(event.tokens as u64),
                        format_bytes(event.bytes),
                        event.command
                    ),
                );
                self.stick_bottom = true;
            }
            AgentEvent::TurnDone { err, cancelled } => {
                self.flush_stream();
                self.event_rx = None;
                self.cancel_tx = None;
                self.budget_reply = None;
                self.large_reply = None;
                self.mode = Mode::Idle;
                self.interrupt_at = None;
                if cancelled {
                    self.status = "interrupted".into();
                    self.append(LineKind::Meta, "Turn cancelled.");
                } else if let Some(e) = err {
                    self.status = "error".into();
                    self.append(LineKind::Error, format!("error: {e}"));
                } else {
                    self.status = "ready".into();
                }
            }
        }
    }

    pub(crate) fn poll_background(&mut self) {
        // Drain agent events without holding a borrow on `self.event_rx`.
        let mut batch = Vec::new();
        let mut disconnected = false;
        if let Some(rx) = self.event_rx.as_mut() {
            loop {
                match rx.try_recv() {
                    Ok(ev) => batch.push(ev),
                    Err(mpsc::error::TryRecvError::Empty) => break,
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }
        for ev in batch {
            self.handle_agent_event(ev);
        }
        if disconnected {
            self.event_rx = None;
            if matches!(
                self.mode,
                Mode::Running | Mode::BudgetPause | Mode::LargeResult
            ) {
                self.mode = Mode::Idle;
                self.status = "ready".into();
            }
        }

        let archive = self
            .archive_rx
            .as_mut()
            .and_then(|rx| match rx.try_recv() {
                Ok(v) => Some(Some(v)),
                Err(oneshot::error::TryRecvError::Empty) => None,
                Err(oneshot::error::TryRecvError::Closed) => Some(None),
            });
        if let Some(outcome) = archive {
            self.archive_rx = None;
            match outcome {
                Some(Ok(res)) => {
                    self.mode = Mode::Idle;
                    self.status = "ready".into();
                    self.append(
                        LineKind::Meta,
                        format!(
                            "Archived session.\nSummary: {}\nMemory: {}\nLogs: {}",
                            res.summary,
                            res.memory.display(),
                            res.log_dir.display()
                        ),
                    );
                }
                Some(Err(e)) => {
                    self.mode = Mode::Idle;
                    self.status = "archive failed".into();
                    self.append(LineKind::Error, format!("archive error: {e}"));
                }
                None => {
                    self.mode = Mode::Idle;
                    self.status = "archive failed".into();
                }
            }
        }
    }

    pub(crate) fn interrupt_or_quit(&mut self) {
        if let Some(at) = self.interrupt_at {
            if at.elapsed() < INTERRUPT_WINDOW {
                self.quit();
                return;
            }
        }
        self.interrupt_at = Some(Instant::now());
        if let Some(tx) = self.cancel_tx.take() {
            let _ = tx.send(());
        }
        self.status = "interrupting… (ctrl+c again to quit)".into();
        self.append(LineKind::Meta, "Interrupting…");
    }

    pub(crate) fn quit(&mut self) {
        if let Some(tx) = self.cancel_tx.take() {
            let _ = tx.send(());
        }
        if let Some(tx) = self.budget_reply.take() {
            let _ = tx.send(false);
        }
        self.large_reply = None;
        self.should_quit = true;
    }
}

/// Run the terminal UI until quit. Requires a multi-thread tokio runtime.
pub fn run(agent: Agent) -> io::Result<()> {
    let rt = tokio::runtime::Handle::current();
    enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(out);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(agent);
    let tick = Duration::from_millis(50);

    let result = (|| -> io::Result<()> {
        loop {
            app.poll_background();
            terminal.draw(|f| app.draw(f))?;

            if app.should_quit {
                break;
            }

            if event::poll(tick)? {
                match event::read()? {
                    Event::Key(key) => app.handle_key(key, &rt),
                    Event::Mouse(m) => match m.kind {
                        MouseEventKind::ScrollUp => {
                            app.stick_bottom = false;
                            app.scroll = app.scroll.saturating_sub(3);
                        }
                        MouseEventKind::ScrollDown => {
                            app.scroll = app.scroll.saturating_add(3);
                        }
                        _ => {}
                    },
                    Event::Resize(_, _) => {}
                    _ => {}
                }
            }
        }
        Ok(())
    })();

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    result
}
