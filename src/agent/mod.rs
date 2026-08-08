//! Agent loop, context assembly, budget/large-result guardrails, archive.

mod archive;
mod events;
mod turn;

pub use archive::{
    append_memory, format_session_settings_log, format_system_prompt_log, is_archive_command,
    one_sentence, sanitize_dir_name, write_archive_layout, ArchiveResult, SessionSettings,
};
pub use events::{
    AgentEvent, BudgetDecision, LargeResultDecision, LargeToolResultReply, ToolRecord,
};
pub use turn::{
    apply_budget_continue, assemble_user_message_texts, context_tokens, decide_budget_pause,
    decide_large_tool_result, first_turn_context_blocks, first_turn_user_texts, load_host_config,
    resolve_effort, resolve_model, resolve_model_intro, Agent, CompleteFn, DEFAULT_BUDGET,
    DEFAULT_EFFORT, DEFAULT_MAX_TOKENS, DEFAULT_MODEL, DEFAULT_MODEL_INTRO, BUDGET_INCREMENT,
};
