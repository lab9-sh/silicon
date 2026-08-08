//! Silicon entry point: load `.env`, require API key, launch ratatui UI.

use std::process::ExitCode;

use silicon::agent::{resolve_effort, resolve_model, Agent};
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
    let _ = dotenvy::from_path(cwd.join(".env"));

    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .unwrap_or_default()
        .trim()
        .to_string();
    if api_key.is_empty() {
        return Err(
            "ANTHROPIC_API_KEY is not set (export it or put it in .env)".into(),
        );
    }

    let model = resolve_model();
    let effort = resolve_effort();
    let agent = Agent::new(&api_key, cwd, &model, &effort);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;

    rt.block_on(async {
        // Enter the runtime so tui::run can use Handle::current().
        tui::run(agent).map_err(|e| e.to_string())
    })
}
