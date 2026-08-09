//! Pure format helpers for the ratatui UI.

use std::path::Path;

use super::app::Mode;

const INPUT_PLACEHOLDER: &str = "Ask Silicon to inspect or edit the repo…";

/// Build the window title from directory, model, effort, and mode.
pub fn format_window_title(dir: &str, model: &str, effort: &str, mode: Mode) -> String {
    let mut parts = Vec::new();
    if !dir.is_empty() {
        parts.push(dir);
    }
    if !model.is_empty() {
        parts.push(model);
    }
    if !effort.is_empty() {
        parts.push(effort);
    }
    let mut title = if parts.is_empty() {
        "silicon".into()
    } else {
        parts.join(" · ")
    };
    match mode {
        Mode::Running | Mode::Archiving => title.push_str(" — working…"),
        Mode::BudgetPause | Mode::LargeResult => title.push_str(" — needs input"),
        Mode::Idle => {}
    }
    title
}

pub fn input_placeholder(mode: Mode) -> &'static str {
    match mode {
        Mode::LargeResult => "Type guidance for the agent, then Enter to deny…",
        _ => INPUT_PLACEHOLDER,
    }
}

pub fn tool_prefix(name: &str) -> &'static str {
    match name {
        "edit_file" => "edit> ",
        "" | "bash" => "bash$ ",
        _ => "tool> ",
    }
}

pub fn tool_label(name: &str) -> &str {
    if name.is_empty() {
        "bash"
    } else {
        name
    }
}

pub fn base_dir(cwd: &Path) -> String {
    cwd.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| s != "." && s != "/")
        .unwrap_or_default()
}

pub fn format_tokens(n: u64) -> String {
    if n >= 1000 {
        format!("{:.0}k", n as f64 / 1000.0)
    } else {
        format!("{n}")
    }
}

pub fn format_bytes(n: usize) -> String {
    if n >= 1 << 20 {
        format!("{:.1} MiB", n as f64 / (1 << 20) as f64)
    } else if n >= 1 << 10 {
        format!("{:.1} KiB", n as f64 / (1 << 10) as f64)
    } else {
        format!("{n} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_title_modes() {
        let idle = format_window_title("repo", "claude-sonnet-5", "medium", Mode::Idle);
        assert_eq!(idle, "repo · claude-sonnet-5 · medium");

        let working = format_window_title("repo", "claude-sonnet-5", "medium", Mode::Running);
        assert!(working.ends_with(" — working…"), "{working}");

        let needs = format_window_title("repo", "m", "high", Mode::BudgetPause);
        assert!(needs.ends_with(" — needs input"), "{needs}");

        let empty = format_window_title("", "", "", Mode::Idle);
        assert_eq!(empty, "silicon");
    }

    #[test]
    fn tool_prefix_labels() {
        assert_eq!(tool_prefix("bash"), "bash$ ");
        assert_eq!(tool_prefix("edit_file"), "edit> ");
        assert_eq!(tool_label(""), "bash");
        assert_eq!(tool_label("edit_file"), "edit_file");
    }

    #[test]
    fn format_helpers() {
        assert_eq!(format_tokens(500), "500");
        assert_eq!(format_tokens(200_000), "200k");
        assert!(format_bytes(2048).contains("KiB"));
    }

    #[test]
    fn input_placeholder_large_result() {
        assert!(input_placeholder(Mode::LargeResult).contains("deny"));
        assert_eq!(input_placeholder(Mode::Idle), INPUT_PLACEHOLDER);
    }
}
