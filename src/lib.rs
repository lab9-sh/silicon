//! Silicon — terminal coding agent (Rust port of Oxygen).
//!
//! Host tools, agent loop (hydrogen), archive layout, and ratatui UI.

pub mod agent;
pub mod tools;
pub mod tui;

pub use agent::{
    resolve_effort, resolve_model, resolve_model_intro, Agent, AgentEvent, ArchiveResult,
    BudgetDecision, LargeResultDecision, LargeToolResultReply, ToolRecord, DEFAULT_BUDGET,
    DEFAULT_EFFORT, DEFAULT_MODEL, DEFAULT_MODEL_INTRO,
};
