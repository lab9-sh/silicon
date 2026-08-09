//! Silicon — terminal coding agent (Rust port of Oxygen).
//!
//! Host tools, agent loop (hydrogen), archive layout, and ratatui UI.

pub mod agent;
pub mod tools;
pub mod tui;

pub use agent::{
    apply_budget_continue, assemble_user_message_texts, context_tokens, decide_budget_pause,
    decide_large_tool_result, first_turn_context_blocks, is_large_tool_result, load_host_config,
    resolve_effort, resolve_model, resolve_model_intro, Agent, AgentEvent, ArchiveResult,
    BudgetDecision, LargeResultDecision, LargeToolResultReply, ToolRecord, DEFAULT_BUDGET,
    DEFAULT_EFFORT, DEFAULT_MODEL, DEFAULT_MODEL_INTRO, BUDGET_INCREMENT,
};
