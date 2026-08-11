//! Silicon — terminal coding agent.
//!
//! Host tools, agent loop (hydrogen), archive layout, and ratatui UI.

pub mod agent;
pub mod tools;
pub mod tui;

pub use agent::{
    apply_budget_continue, assemble_user_message_texts, context_tokens, decide_budget_pause,
    decide_large_tool_result, first_turn_context_blocks, is_large_tool_result, load_host_config,
    resolve_api_key, resolve_effort, resolve_model, resolve_model_intro, resolve_provider, Agent,
    AgentEvent, ArchiveResult, BudgetDecision, LargeResultDecision, LargeToolResultReply, Provider,
    ToolRecord, DEFAULT_BUDGET, DEFAULT_EFFORT, DEFAULT_MODEL_ANTHROPIC, DEFAULT_MODEL_INTRO,
    DEFAULT_MODEL_OPENAI, DEFAULT_MODEL_XAI, DEFAULT_PROVIDER, BUDGET_INCREMENT,
};
