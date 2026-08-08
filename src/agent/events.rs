//! Events emitted during a turn for the TUI to render.

use tokio::sync::oneshot;

#[derive(Debug, Clone)]
pub struct TextDeltaEvent {
    pub text: String,
}

/// Announces a tool invocation. `name` is the tool name; `command` is a short
/// display summary of its input.
#[derive(Debug, Clone)]
pub struct ToolStartEvent {
    pub name: String,
    pub command: String,
}

#[derive(Debug, Clone)]
pub struct ToolResultEvent {
    pub output: String,
    pub is_error: bool,
}

#[derive(Debug, Clone)]
pub struct UsageEvent {
    pub context_tokens: u64,
    pub budget: u64,
    pub input_tokens: u64,
    pub cache_create: u64,
    pub cache_read: u64,
    pub output_tokens: u64,
}

#[derive(Debug, Clone)]
pub struct BudgetPauseEvent {
    pub context_tokens: u64,
    pub budget: u64,
}

/// User decision for an oversized tool result.
#[derive(Debug, Clone)]
pub struct LargeToolResultReply {
    pub approve: bool,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct LargeToolResultEvent {
    pub command: String,
    pub tokens: usize,
    pub bytes: usize,
}

#[derive(Debug)]
pub enum AgentEvent {
    TextDelta(TextDeltaEvent),
    ToolStart(ToolStartEvent),
    ToolResult(ToolResultEvent),
    Usage(UsageEvent),
    /// Context budget reached. Reply via the oneshot: `true` = continue +100k.
    BudgetPause {
        event: BudgetPauseEvent,
        reply: oneshot::Sender<bool>,
    },
    /// Tool result is large. Reply with approve/deny (+ optional guidance).
    LargeToolResult {
        event: LargeToolResultEvent,
        reply: oneshot::Sender<LargeToolResultReply>,
    },
    TurnDone {
        err: Option<String>,
        cancelled: bool,
    },
}

/// Pure outcome of a budget-pause user choice (for unit tests).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetDecision {
    /// User chose continue: new budget after +increment.
    Continue { new_budget: u64 },
    /// User chose stop: end the turn cleanly.
    Stop,
}

/// Pure outcome of a large-result user choice (for unit tests).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LargeResultDecision {
    /// Forward the raw tool output.
    Approve,
    /// Forward `message` as the tool result (is_error = true).
    Deny { message: String },
}

/// One tool invocation recorded for archive logs.
#[derive(Debug, Clone)]
pub struct ToolRecord {
    pub id: String,
    pub name: String,
    /// Raw JSON input.
    pub input: String,
    pub response: String,
    pub is_error: bool,
}
