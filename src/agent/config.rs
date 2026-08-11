//! Env/config resolution: provider, model, effort, host config, defaults.

use std::path::Path;

/// Initial session context soft-cap (tokens). Raised by [`BUDGET_INCREMENT`] on continue.
/// Lives for the whole agent process — not reset on each user turn.
pub const DEFAULT_BUDGET: u64 = 200_000;
/// How much to raise the session budget when the user continues past a pause.
pub const BUDGET_INCREMENT: u64 = 100_000;
pub const DEFAULT_MODEL_ANTHROPIC: &str = "claude-sonnet-5";
pub const DEFAULT_MODEL_OPENAI: &str = "gpt-5.6-luna";
pub const DEFAULT_MODEL_XAI: &str = "grok-build-0.1";
pub const DEFAULT_EFFORT: &str = "medium";
pub const DEFAULT_PROVIDER: Provider = Provider::Anthropic;

/// Default model identity line when `SILICON_MODEL_INTRO` is unset.
pub const DEFAULT_MODEL_INTRO: &str = "You are Si, a coding agent.";

/// LLM backend. Maps 1:1 onto hydrogen's `Client::{anthropic,openai,xai}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Anthropic,
    OpenAi,
    Xai,
}

impl Provider {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenAi => "openai",
            Self::Xai => "xai",
        }
    }

    /// Env var that holds this provider's API key.
    pub fn api_key_env(self) -> &'static str {
        match self {
            Self::Anthropic => "ANTHROPIC_API_KEY",
            Self::OpenAi => "OPENAI_API_KEY",
            Self::Xai => "XAI_API_KEY",
        }
    }

    /// Default `SILICON_MODEL` when unset for this provider.
    pub fn default_model(self) -> &'static str {
        match self {
            Self::Anthropic => DEFAULT_MODEL_ANTHROPIC,
            Self::OpenAi => DEFAULT_MODEL_OPENAI,
            Self::Xai => DEFAULT_MODEL_XAI,
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "anthropic" | "claude" => Some(Self::Anthropic),
            "openai" | "oai" => Some(Self::OpenAi),
            "xai" | "grok" => Some(Self::Xai),
            _ => None,
        }
    }
}

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

/// Resolve provider from env (`SILICON_PROVIDER`) or default Anthropic.
///
/// Accepted values: `anthropic`/`claude`, `openai`/`oai`, `xai`/`grok`.
pub fn resolve_provider() -> Result<Provider, String> {
    match std::env::var("SILICON_PROVIDER") {
        Ok(v) => {
            let trimmed = v.trim();
            if trimmed.is_empty() {
                return Ok(DEFAULT_PROVIDER);
            }
            Provider::parse(trimmed).ok_or_else(|| {
                format!(
                    "invalid SILICON_PROVIDER={trimmed:?} (expected anthropic, openai, or xai)"
                )
            })
        }
        Err(_) => Ok(DEFAULT_PROVIDER),
    }
}

/// Read the API key for `provider` from the matching env var.
pub fn resolve_api_key(provider: Provider) -> Result<String, String> {
    let key_name = provider.api_key_env();
    let api_key = std::env::var(key_name).unwrap_or_default();
    let api_key = api_key.trim().to_string();
    if api_key.is_empty() {
        return Err(format!(
            "{key_name} is not set (export it or put it in .env for SILICON_PROVIDER={})",
            provider.as_str()
        ));
    }
    Ok(api_key)
}

/// Resolve model id from env (`SILICON_MODEL`) or the provider default.
pub fn resolve_model(provider: Provider) -> String {
    env_or("SILICON_MODEL", provider.default_model(), |s| {
        Some(s.to_string())
    })
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
/// Injected into the system prompt between the Silicon intro and the cwd line.
/// Missing or empty files yield `None`.
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
        let _ = resolve_model(Provider::Anthropic);
        let _ = resolve_model_intro();
        let _ = resolve_effort();
        assert_eq!(DEFAULT_MODEL_ANTHROPIC, "claude-sonnet-5");
        assert_eq!(DEFAULT_MODEL_OPENAI, "gpt-5.6-luna");
        assert_eq!(DEFAULT_MODEL_XAI, "grok-build-0.1");
        assert_eq!(DEFAULT_EFFORT, "medium");
        assert!(!DEFAULT_MODEL_INTRO.is_empty());
        assert_eq!(Provider::Anthropic.default_model(), DEFAULT_MODEL_ANTHROPIC);
        assert_eq!(Provider::OpenAi.default_model(), DEFAULT_MODEL_OPENAI);
        assert_eq!(Provider::Xai.default_model(), DEFAULT_MODEL_XAI);
    }

    #[test]
    fn provider_parse_accepts_aliases() {
        assert_eq!(Provider::parse("anthropic"), Some(Provider::Anthropic));
        assert_eq!(Provider::parse("Claude"), Some(Provider::Anthropic));
        assert_eq!(Provider::parse("openai"), Some(Provider::OpenAi));
        assert_eq!(Provider::parse("OAI"), Some(Provider::OpenAi));
        assert_eq!(Provider::parse("xai"), Some(Provider::Xai));
        assert_eq!(Provider::parse("grok"), Some(Provider::Xai));
        assert_eq!(Provider::parse("nope"), None);
    }

    #[test]
    fn provider_api_key_env_names() {
        assert_eq!(Provider::Anthropic.api_key_env(), "ANTHROPIC_API_KEY");
        assert_eq!(Provider::OpenAi.api_key_env(), "OPENAI_API_KEY");
        assert_eq!(Provider::Xai.api_key_env(), "XAI_API_KEY");
    }
}
