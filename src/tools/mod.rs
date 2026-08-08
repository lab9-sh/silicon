//! Host tools: `bash` and `edit_file`.

mod bash;
mod edit;

pub use bash::{
    estimate_tokens, run_bash, run_bash_cancellable, BashInput, BashOutcome, BASH_TIMEOUT,
    LARGE_TOOL_RESULT_TOKENS,
};
pub use edit::{run_edit, EditInput, EDIT_CONTEXT_LINES};

use hydrogen::ToolDef;
use serde_json::json;

/// Tool definitions registered with every agent request (cached prompt prefix).
pub fn session_tools() -> Vec<ToolDef> {
    vec![bash_tool_def(), edit_tool_def()]
}

pub fn bash_tool_def() -> ToolDef {
    ToolDef {
        name: "bash".into(),
        description: "Run a bash command.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "Bash command to execute."
                }
            },
            "required": ["command"]
        }),
    }
}

pub fn edit_tool_def() -> ToolDef {
    ToolDef {
        name: "edit_file".into(),
        description: "Edit a file by exact string replacement, or create a new file. \
            Prefer this over bash heredocs/sed for file edits. \
            old_string must match the file exactly (including whitespace and indentation) \
            and must be unique unless replace_all is true; include a few surrounding lines \
            to disambiguate. Leave old_string empty to create a new file with new_string as \
            its contents. To delete text, pass an empty new_string."
            .into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File path, absolute or relative to the working directory."
                },
                "old_string": {
                    "type": "string",
                    "description": "Exact text to replace. Empty means create a new file."
                },
                "new_string": {
                    "type": "string",
                    "description": "Replacement text (or the full contents of a new file)."
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "Replace every occurrence of old_string instead of requiring a unique match."
                }
            },
            "required": ["path", "old_string", "new_string"]
        }),
    }
}
