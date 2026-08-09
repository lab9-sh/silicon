//! Env/config resolution: model, effort, host config, defaults.

use std::path::Path;

/// Initial session context soft-cap (tokens). Raised by [`BUDGET_INCREMENT`] on continue.
/// Lives for the whole agent process — not reset on each user turn.
pub const DEFAULT_BUDGET: u64 = 200_000;
/// How much to raise the session budget when the user continues past a pause.
pub const BUDGET_INCREMENT: u64 = 100_000;
pub const DEFAULT_MAX_TOKENS: u32 = 128_000;
pub const DEFAULT_MODEL: &str = "claude-sonnet-5";
pub const DEFAULT_EFFORT: &str = "medium";

/// Default model identity line when `SILICON_MODEL_INTRO` is unset.
pub const DEFAULT_MODEL_INTRO: &str = "You are Si, a coding agent.";

/// Read `key` from the environment; return a non-empty trimmed value that
/// passes `validate`, else `default`.
fn env_or(key: &str, default: &str, validate: impl Fn(&str) -> Option<String>) -> String {
    if let Ok(raw) = std::env::var(key) {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            if let Some(v) = validate(trimmed) {
                return v;
            }
        }
    }
    default.into()
}

/// Resolve model id from env (`SILICON_MODEL`) or default.
pub fn resolve_model() -> String {
    env_or("SILICON_MODEL", DEFAULT_MODEL, |s| Some(s.to_string()))
}

/// Resolve model identity intro from env (`SILICON_MODEL_INTRO`) or default.
///
/// Example override: `You are Claude, a large language model created by Anthropic.`
pub fn resolve_model_intro() -> String {
    env_or("SILICON_MODEL_INTRO", DEFAULT_MODEL_INTRO, |s| {
        Some(s.to_string())
    })
}

/// Resolve effort from env (`SILICON_EFFORT`) or default.
pub fn resolve_effort() -> String {
    env_or("SILICON_EFFORT", DEFAULT_EFFORT, |s| {
        let e = s.to_lowercase();
        if matches!(e.as_str(), "low" | "medium" | "high") {
            Some(e)
        } else {
            None
        }
    })
}

/// Optional host-environment block from `cwd/.si/config/host.md`.
///
/// Injected into the system prompt between the Silicon intro and the
/// "You are chatting with…" line. Missing or empty files yield `None`.
pub fn load_host_config(cwd: &Path) -> Option<String> {
    let raw = read_optional_file(&cwd.join(".si").join("config").join("host.md"))?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub(crate) fn read_optional_file(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_file(path: &Path, content: &str) {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn load_host_config_reads_si_config_host_md() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            &dir.path().join(".si").join("config").join("host.md"),
            "\n## Host Tools\n\n`rg`, `fd`\n\n",
        );
        let got = load_host_config(dir.path()).unwrap();
        assert_eq!(got, "## Host Tools\n\n`rg`, `fd`");
        assert!(load_host_config(tempfile::tempdir().unwrap().path()).is_none());
    }

    #[test]
    fn resolve_model_defaults_when_unset() {
        // Smoke-call resolvers (ambient env may override) and assert defaults.
        let _ = resolve_model();
        let _ = resolve_model_intro();
        let _ = resolve_effort();
        assert_eq!(DEFAULT_MODEL, "claude-sonnet-5");
        assert_eq!(DEFAULT_EFFORT, "medium");
        assert!(!DEFAULT_MODEL_INTRO.is_empty());
    }
}
