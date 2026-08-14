# Silicon

Silicon is a minimal, terminal-based coding agent. It gives the model a `bash` tool and an `edit_file` tool to inspect and modify a codebase, and presents the conversation in a [ratatui](https://ratatui.rs) TUI. Model I/O goes through [hydrogen](https://github.com/lab9-sh/hydrogen) (Anthropic, OpenAI, or xAI).

## Features

- **Agent loop with two tools** — `bash` via `/bin/bash -lc` (60s timeout) and `edit_file` for exact string replacement.
- **Reliable file edits** — unique match (or `replace_all`), empty `old_string` creates a new file, no-ops rejected, line-numbered snippets on success.
- **Streaming TUI** — live model text, tool start/result, and usage status.
- **Context budget guardrail** — session soft-cap at ~200k context tokens to limit context fog; continue adds +100k (persists for the session), stop ends the turn so you can start a fresh session with a clean window.
- **Large tool-result guardrail** — output estimated above ~50k tokens pauses for approve (forward raw) or deny (forward your guidance).
- **Session archiving** — `/archive` summarizes the session, appends to `.si/memory.md` (`yyyy-mm-dd hh:mm` prefix), and writes under `.si/logs/{datetime}-{summary}/`: full multi-turn transcript (`transcript.md`, including reasoning summaries when hydrogen exposes them), tool logs (`tool/{id}.md`), the system prompt (`system-prompt.md`), and session settings (`session.md`: model, model intro, thinking effort).
- **`.env` support** — loads provider API keys from a local `.env` when unset.
- **Multi-provider** — Anthropic, OpenAI, or xAI via `SILICON_PROVIDER`.

## Requirements

- Rust 1.85+ (edition 2024)
- An API key for your chosen provider (Anthropic, OpenAI, or xAI)

## Setup

```bash
cp .env.example .env
# edit .env: set SILICON_PROVIDER and the matching API key
# e.g. ANTHROPIC_API_KEY=…  or  OPENAI_API_KEY=…  or  XAI_API_KEY=…
```

## Build & Run

```bash
cargo run --release
# or
cargo build --release && ./target/release/silicon
```

Silicon operates on the current working directory, so run it from the root of the project you want the agent to work on.

## Configuration

| Env var               | Description                                    | Default                       |
|-----------------------|------------------------------------------------|-------------------------------|
| `SILICON_PROVIDER`    | Backend: `anthropic`, `openai`, or `xai`       | `anthropic`                   |
| `ANTHROPIC_API_KEY`   | Anthropic API key (when provider is anthropic) | —                             |
| `OPENAI_API_KEY`      | OpenAI API key (when provider is openai)       | —                             |
| `XAI_API_KEY`         | xAI API key (when provider is xai)             | —                             |
| `SILICON_MODEL`       | Model ID for the selected provider             | provider default\*            |
| `SILICON_MODEL_INTRO` | Model identity line for the system prompt      | `You are Si, a coding agent.` |
| `SILICON_EFFORT`      | Thinking effort: `low`, `medium`, `high`       | `medium`                      |

\* Defaults: `claude-sonnet-5` (anthropic), `gpt-5.6-luna` (openai), `grok-build-0.1` (xai).

Put these in the project `.env` (or export them). **Multi-word values must be quoted** — dotenvy rejects unquoted spaces, e.g. `SILICON_MODEL_INTRO="You are Claude, a large language model created by Anthropic."`.

Host environment notes for the system prompt (tools, language versions, etc.) are optional and loaded at runtime from `.si/config/host.md` when present.

## Usage

- Type a prompt and press `Enter` to send it.
- Scroll with `PgUp`/`PgDn`, mouse wheel, or `Shift+↑`/`Shift+↓`.
- Press `Esc` to clear the input (when idle).
- While running: `Ctrl+C` cancels the turn; again within 2s quits. Idle: quits.
- Budget pause: `c` continues (+100k), `s` stops.
- Large tool result: `Ctrl+Y` approves; type guidance + `Enter` denies.
- `/archive` (idle) summarizes into `.si/memory.md` and dumps the full session transcript, tool logs, system prompt, and session settings.

## Project layout

```
src/main.rs       entry: .env, provider/API key, model/effort, launch TUI
src/lib.rs        library root
src/tools/        bash + edit_file
src/agent/        hydrogen-backed turn loop, budget/large-result, archive
src/tui/          ratatui app
```

## Testing

```bash
cargo test
```
