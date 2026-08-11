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
        description: "Edit or create a file. old_string must be unique unless replace_all is set to true.
            Disambiguate old_string with a few surrounding lines of text to make unique.\
            Leave old_string empty to create a new file. Leave new_string empty to delete text."
            .into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute or relative file path."
                },
                "old_string": {
                    "type": "string",
                    "description": "Text to replace, including whitespace and indentation."
                },
                "new_string": {
                    "type": "string",
                    "description": "Replacement text."
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "Set to true when old_string is not unique."
                }
            },
            "required": ["path", "old_string", "new_string"]
        }),
    }
}
