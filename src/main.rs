//! Silicon entry point: load `.env`, require API key, launch ratatui UI.

use std::process::ExitCode;

use silicon::agent::{
    resolve_api_key, resolve_effort, resolve_model, resolve_provider, Agent,
};
use silicon::tui;

fn main() -> ExitCode {
    if let Err(e) = run() {
        eprintln!("silicon: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn run() -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;

    // Load .env for keys not already set (dotenvy does not override).
    // Multi-word values must be quoted (dotenvy rejects `KEY=foo bar`).
    let env_path = cwd.join(".env");
    if env_path.is_file() {
        dotenvy::from_path(&env_path).map_err(|e| {
            format!(
                "failed to load {}: {e}\n\
                 Tip: quote multi-word values, e.g. SILICON_MODEL_INTRO=\"You are …\"",
                env_path.display()
            )
        })?;
    }

    let provider = resolve_provider()?;
    let api_key = resolve_api_key(provider)?;
    let model = resolve_model(provider);
    let effort = resolve_effort();
    let agent = Agent::new(provider, &api_key, cwd, &model, &effort);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;

    rt.block_on(async {
        // Enter the runtime so tui::run can use Handle::current().
        tui::run(agent).map_err(|e| e.to_string())
    })
}
