# Silicon

Silicon is a minimal, terminal-based coding agent — the Rust port of
[Oxygen](../oxygen). It gives Claude a `bash` tool and an `edit_file` tool to
inspect and modify a codebase, and presents the conversation in a
[ratatui](https://ratatui.rs) TUI. Model I/O goes through
[hydrogen](../hydrogen) (Anthropic adapter).

## Features

- **Agent loop with two tools** — `bash` via `/bin/bash -lc` (60s timeout) and
  `edit_file` for exact string replacement.
- **Reliable file edits** — unique match (or `replace_all`), empty `old_string`
  creates a new file, no-ops rejected, line-numbered snippets on success.
- **Streaming TUI** — live model text, tool start/result, and usage status.
- **Context budget guardrail** — pauses every ~200k context tokens; continue
  adds +100k, stop ends the turn.
- **Large tool-result guardrail** — output estimated above ~50k tokens pauses
  for approve (forward raw) or deny (forward your guidance).
- **Session archiving** — `/archive` summarizes the session, appends to
  `.si/memory.md` (`yyyy-mm-dd hh:mm` prefix), and writes tool logs under
  `.si/logs/{datetime}-{summary}/tool/{id}.md`.
- **`.env` support** — loads `ANTHROPIC_API_KEY` from a local `.env` when unset.

## Requirements

- Rust 1.80+ (edition 2021)
- A sibling checkout of [hydrogen](../hydrogen)
- An Anthropic API key

## Setup

```bash
cp .env.example .env
# edit .env and set ANTHROPIC_API_KEY=sk-ant-...
```

## Build & Run

```bash
cargo run --release
# or
cargo build --release && ./target/release/silicon
```

Silicon operates on the current working directory, so run it from the root of
the project you want the agent to work on.

## Configuration

| Env var               | Description                                              | Default            |
|-----------------------|----------------------------------------------------------|--------------------|
| `ANTHROPIC_API_KEY`   | Anthropic API key (required)                             | —                  |
| `SILICON_MODEL`       | Anthropic model ID (`OXYGEN_MODEL` also accepted)        | `claude-sonnet-5`  |
| `SILICON_MODEL_INTRO` | Model identity line for the system prompt (`OXYGEN_MODEL_INTRO` also accepted) | `You are Si, a coding agent.` |
| `SILICON_EFFORT`      | Thinking effort: `low`, `medium`, `high`                 | `medium`           |

## Usage

- Type a prompt and press `Enter` to send it.
- Scroll with `PgUp`/`PgDn`, mouse wheel, or `Shift+↑`/`Shift+↓`.
- Press `Esc` to clear the input (when idle).
- While running: `Ctrl+C` cancels the turn; again within 2s quits. Idle: quits.
- Budget pause: `c` continues (+100k), `s` stops.
- Large tool result: `Ctrl+Y` approves; type guidance + `Enter` denies.
- `/archive` (idle) summarizes into `.si/memory.md` and dumps tool logs.

## Project layout

```
src/main.rs       entry: .env, API key, model/effort, launch TUI
src/lib.rs        library root
src/tools/        bash + edit_file
src/agent/        hydrogen-backed turn loop, budget/large-result, archive
src/tui/          ratatui app
```

## Testing

```bash
cargo test
```
