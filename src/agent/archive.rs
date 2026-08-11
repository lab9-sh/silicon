//! Session archive: memory append, log layout, `/archive` detection.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use hydrogen::{ContentBlock, Message, Role, ToolOutput};
use time::OffsetDateTime;

use super::events::ToolRecord;

const TOOL_LOG_FILE_EXT: &str = ".md";
const MAX_SUMMARY_DIR_NAME: usize = 80;

/// Outcome of a successful `/archive`.
#[derive(Debug, Clone)]
pub struct ArchiveResult {
    pub summary: String,
    /// Absolute path to `.si/logs/{datetime}-{sanitized}/`.
    pub log_dir: PathBuf,
    /// Absolute path to `.si/memory.md`.
    pub memory: PathBuf,
}

/// Whether trimmed input is the `/archive` slash command.
pub fn is_archive_command(s: &str) -> bool {
    s.trim() == "/archive"
}

/// Append one sentence to `cwd/.si/memory.md`, prefixed with `now` as
/// `yyyy-mm-dd hh:mm` (UTC), creating directories and the file as needed.
pub fn append_memory(cwd: &Path, now: SystemTime, sentence: &str) -> Result<PathBuf, String> {
    let sentence = sentence.trim();
    if sentence.is_empty() {
        return Err("empty memory sentence".into());
    }
    let dir = cwd.join(".si");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join("memory.md");

    let mut body = String::new();
    if path.exists() {
        let existing = fs::read_to_string(&path).map_err(|e| e.to_string())?;
        body.push_str(&existing);
        if !existing.is_empty() && !existing.ends_with('\n') {
            body.push('\n');
        }
    }
    body.push_str(&format_memory_line(now, sentence));
    body.push('\n');

    fs::write(&path, body.as_bytes()).map_err(|e| e.to_string())?;
    Ok(fs::canonicalize(&path).unwrap_or(path))
}

/// Format a single memory line: `yyyy-mm-dd hh:mm {sentence}` (UTC).
pub fn format_memory_line(now: SystemTime, sentence: &str) -> String {
    let dt = OffsetDateTime::from(now);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02} {}",
        dt.year(),
        u8::from(dt.month()),
        dt.day(),
        dt.hour(),
        dt.minute(),
        sentence
    )
}

/// Session settings captured at `/archive` time.
#[derive(Debug, Clone)]
pub struct SessionSettings {
    pub model: String,
    pub model_intro: String,
    pub thinking_effort: String,
}

/// Create `cwd/.si/logs/{datetime}-{sanitized}/` with tool logs, full transcript,
/// system prompt, and session settings.
///
/// Layout:
/// - `transcript.md` — full multi-turn session log (user/assistant text, tool
///   uses/results, reasoning summaries when hydrogen exposes them)
/// - `tool/{id}.md` — each tool call
/// - `system-prompt.md` — system prompt used for the session (or first user
///   message if no system prompt is available)
/// - `session.md` — model, model intro, and thinking effort
pub fn write_archive_layout(
    cwd: &Path,
    now: SystemTime,
    summary: &str,
    records: &[ToolRecord],
    system_prompt: Option<&str>,
    first_user_message: Option<&str>,
    settings: &SessionSettings,
    messages: &[Message],
) -> Result<PathBuf, String> {
    let mut safe = sanitize_dir_name(summary);
    if safe.is_empty() {
        safe = "session".into();
    }
    let stamp = format_log_stamp(now);
    let dir_name = format!("{stamp}-{safe}");
    let log_dir = cwd.join(".si").join("logs").join(&dir_name);
    let tool_dir = log_dir.join("tool");
    fs::create_dir_all(&tool_dir).map_err(|e| e.to_string())?;

    let transcript_body = format_session_transcript(messages);
    fs::write(log_dir.join("transcript.md"), transcript_body.as_bytes())
        .map_err(|e| e.to_string())?;

    for rec in records {
        let id = if rec.id.is_empty() {
            "unknown".into()
        } else {
            sanitize_file_component(&rec.id)
        };
        let path = tool_dir.join(format!("{id}{TOOL_LOG_FILE_EXT}"));
        let body = format_tool_log(rec);
        fs::write(&path, body.as_bytes()).map_err(|e| e.to_string())?;
    }

    let system_body = format_system_prompt_log(system_prompt, first_user_message);
    fs::write(log_dir.join("system-prompt.md"), system_body.as_bytes())
        .map_err(|e| e.to_string())?;

    let session_body = format_session_settings_log(settings);
    fs::write(log_dir.join("session.md"), session_body.as_bytes()).map_err(|e| e.to_string())?;

    Ok(fs::canonicalize(&log_dir).unwrap_or(log_dir))
}

/// Human-readable multi-turn transcript of the hydrogen conversation.
///
/// Includes every message the model saw or produced: user text (including
/// first-turn README/memory injection), assistant text, tool uses, tool
/// results, and reasoning summaries when the provider exposes them via
/// [`hydrogen::ReasoningBlock::summary`]. Opaque/encrypted reasoning payloads
/// and redacted thinking blocks (no summary) are noted but not dumped.
pub fn format_session_transcript(messages: &[Message]) -> String {
    let mut out = String::from(
        "# Session transcript\n\n\
         Full multi-turn log of what the agent saw and did. Use this to review \
         tool-use patterns, host context that was available, and reasoning \
         summaries (when the upstream API emitted them).\n\n",
    );
    if messages.is_empty() {
        out.push_str("_(empty session — no messages.)_\n");
        return out;
    }

    for (i, msg) in messages.iter().enumerate() {
        let role = match msg.role {
            Role::User => "user",
            Role::Assistant => "assistant",
        };
        let _ = writeln!(out, "## Message {i} ({role})\n");
        if msg.content.is_empty() {
            out.push_str("_(empty message)_\n\n");
            continue;
        }
        for block in &msg.content {
            match block {
                ContentBlock::Text(t) => {
                    out.push_str("### Text\n\n");
                    out.push_str(t.text.trim_end());
                    out.push_str("\n\n");
                }
                ContentBlock::Reasoning(r) => match r.summary() {
                    Some(s) if !s.trim().is_empty() => {
                        out.push_str("### Reasoning summary\n\n");
                        out.push_str(s.trim_end());
                        out.push_str("\n\n");
                    }
                    _ => {
                        out.push_str(
                            "### Reasoning\n\n\
                             _(no summary available — redacted or provider-private payload only)_\n\n",
                        );
                    }
                },
                ContentBlock::ToolUse(t) => {
                    let _ = writeln!(out, "### Tool use: `{}` (`{}`)\n", t.name, t.id);
                    out.push_str("```json\n");
                    out.push_str(&pretty_json(&t.input));
                    out.push_str("\n```\n\n");
                }
                ContentBlock::ToolResult(t) => {
                    let _ = writeln!(out, "### Tool result (`{}`)\n", t.id);
                    match &t.output {
                        ToolOutput::Text(s) => {
                            out.push_str("```\n");
                            out.push_str(s);
                            if !s.ends_with('\n') {
                                out.push('\n');
                            }
                            out.push_str("```\n\n");
                        }
                        ToolOutput::Json(v) => {
                            out.push_str("```json\n");
                            out.push_str(&pretty_json(v));
                            out.push_str("\n```\n\n");
                        }
                        ToolOutput::Error(s) => {
                            out.push_str("**error**\n\n```\n");
                            out.push_str(s);
                            if !s.ends_with('\n') {
                                out.push('\n');
                            }
                            out.push_str("```\n\n");
                        }
                    }
                }
            }
        }
    }
    out
}

fn pretty_json(v: &serde_json::Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
}

/// Prefer the system prompt; fall back to the first user message when the
/// system prompt was not stored separately (or is empty).
pub fn format_system_prompt_log(
    system_prompt: Option<&str>,
    first_user_message: Option<&str>,
) -> String {
    let system = system_prompt.map(str::trim).filter(|s| !s.is_empty());
    if let Some(sp) = system {
        return format!("# System prompt\n\n{sp}\n");
    }
    let first = first_user_message.map(str::trim).filter(|s| !s.is_empty());
    if let Some(msg) = first {
        return format!(
            "# First user message\n\n\
             _(System prompt was not stored separately for this session.)_\n\n\
             {msg}\n"
        );
    }
    "# System prompt\n\n_(empty session — no system prompt or user message.)_\n".into()
}

/// Markdown body for `session.md`.
pub fn format_session_settings_log(settings: &SessionSettings) -> String {
    format!(
        "# Session settings\n\n\
         - model: {model}\n\
         - model_intro: {intro}\n\
         - thinking_effort: {effort}\n",
        model = settings.model,
        intro = settings.model_intro,
        effort = settings.thinking_effort,
    )
}

/// Filesystem-safe directory name segment from a summary sentence.
pub fn sanitize_dir_name(s: &str) -> String {
    let s = s.trim();
    if s.is_empty() {
        return String::new();
    }
    let mut out = String::with_capacity(s.len());
    let mut prev_hyphen = false;
    for r in s.chars() {
        match r {
            '/' | '\\' | '\0' => continue,
            c if c.is_control() => continue,
            c if c.is_ascii_alphanumeric() => {
                out.push(c.to_ascii_lowercase());
                prev_hyphen = false;
            }
            c if c.is_alphabetic() => {
                for lc in c.to_lowercase() {
                    out.push(lc);
                }
                prev_hyphen = false;
            }
            c if c.is_numeric() => {
                out.push(c);
                prev_hyphen = false;
            }
            '-' | '_' | '.' | ',' => {
                if !prev_hyphen && !out.is_empty() {
                    out.push('-');
                    prev_hyphen = true;
                }
            }
            c if c.is_whitespace() => {
                if !prev_hyphen && !out.is_empty() {
                    out.push('-');
                    prev_hyphen = true;
                }
            }
            _ => {}
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.chars().count() > MAX_SUMMARY_DIR_NAME {
        let truncated: String = out.chars().take(MAX_SUMMARY_DIR_NAME).collect();
        truncated.trim_matches('-').to_string()
    } else {
        out
    }
}

fn sanitize_file_component(s: &str) -> String {
    let s = s.trim();
    if s.is_empty() {
        return "unknown".into();
    }
    let mut out = String::new();
    for r in s.chars() {
        if r == '/' || r == '\\' || r == '\0' || r.is_control() {
            out.push('_');
        } else {
            out.push(r);
        }
    }
    if out.is_empty() || out == "." || out == ".." {
        "unknown".into()
    } else {
        out
    }
}

pub fn format_tool_log(rec: &ToolRecord) -> String {
    format!(
        "# Tool call {id}\n\n\
         ## Call\n\n\
         - name: {name}\n\
         - id: {id}\n\
         - is_error: {err}\n\n\
         ### Input\n\n\
         ```json\n{input}\n```\n\n\
         ## Response\n\n\
         ```\n{response}\n```\n",
        id = rec.id,
        name = rec.name,
        err = rec.is_error,
        input = rec.input,
        response = rec.response,
    )
}

/// Trim model output to a single line/sentence for memory + dir name.
pub fn one_sentence(s: &str) -> String {
    let mut s = s.trim().to_string();
    if s.is_empty() {
        return s;
    }
    if s.len() >= 2 {
        let bytes = s.as_bytes();
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            s = s[1..s.len() - 1].trim().to_string();
        }
    }
    if let Some(i) = s.find('\n') {
        s = s[..i].trim().to_string();
    }
    s
}

fn format_log_stamp(now: SystemTime) -> String {
    let dt = OffsetDateTime::from(now);
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        dt.year(),
        u8::from(dt.month()),
        dt.day(),
        dt.hour(),
        dt.minute(),
        dt.second()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::{Date, Month, PrimitiveDateTime, Time};

    /// Build a SystemTime for a UTC civil datetime via the `time` crate.
    fn system_time_utc(
        year: i32,
        month: u32,
        day: u32,
        hour: u32,
        minute: u32,
        second: u32,
    ) -> SystemTime {
        let date = Date::from_calendar_date(
            year,
            Month::try_from(month as u8).expect("month"),
            day as u8,
        )
        .expect("date");
        let t = Time::from_hms(hour as u8, minute as u8, second as u8).expect("time");
        PrimitiveDateTime::new(date, t).assume_utc().into()
    }

    fn write_file(path: &Path, content: &str) {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    fn has_datetime_prefix(s: &str) -> bool {
        for line in s.lines() {
            let b = line.as_bytes();
            if b.len() >= 16
                && b[4] == b'-'
                && b[7] == b'-'
                && b[10] == b' '
                && b[13] == b':'
                && b[0..4].iter().all(u8::is_ascii_digit)
                && b[5..7].iter().all(u8::is_ascii_digit)
                && b[8..10].iter().all(u8::is_ascii_digit)
                && b[11..13].iter().all(u8::is_ascii_digit)
                && b[14..16].iter().all(u8::is_ascii_digit)
            {
                return true;
            }
        }
        false
    }

    #[test]
    fn is_archive_command_matches() {
        assert!(is_archive_command("/archive"));
        assert!(is_archive_command("  /archive  "));
        assert!(!is_archive_command("/archive now"));
        assert!(!is_archive_command("archive"));
        assert!(!is_archive_command(""));
    }

    #[test]
    fn sanitize_dir_name_cases() {
        assert_eq!(
            sanitize_dir_name("User explored the repo structure."),
            "user-explored-the-repo-structure"
        );
        assert_eq!(
            sanitize_dir_name("path/with\\separators"),
            "pathwithseparators"
        );
        assert_eq!(sanitize_dir_name("  Hello, World!  "), "hello-world");
        assert_eq!(sanitize_dir_name(""), "");
        assert_eq!(sanitize_dir_name("!!!"), "");
        assert_eq!(
            sanitize_dir_name(&"a".repeat(100)),
            "a".repeat(MAX_SUMMARY_DIR_NAME)
        );
    }

    #[test]
    fn append_memory_appends_not_wipes() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join(".si").join("memory.md"), "prior line\n");

        let now = system_time_utc(2026, 8, 7, 15, 4, 5);
        let path = append_memory(dir.path(), now, "new sentence about the session.").unwrap();
        let got = fs::read_to_string(&path).unwrap();
        assert!(got.contains("prior line"), "{got}");
        assert!(got.contains("new sentence about the session."), "{got}");
        assert!(
            got.contains("2026-08-07 15:04 new sentence about the session."),
            "missing datetime prefix: {got}"
        );
        assert!(got.find("prior line").unwrap() < got.find("new sentence").unwrap());
    }

    #[test]
    fn append_memory_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let now = system_time_utc(2026, 8, 7, 9, 5, 0);
        let path = append_memory(dir.path(), now, "first memory.").unwrap();
        let got = fs::read_to_string(&path).unwrap();
        assert_eq!(got.trim(), "2026-08-07 09:05 first memory.");
    }

    #[test]
    fn write_archive_layout_tool_files() {
        let dir = tempfile::tempdir().unwrap();
        let now = system_time_utc(2026, 8, 7, 15, 4, 5);
        let summary = "User listed files and ran tests.";
        let records = vec![
            ToolRecord {
                id: "toolu_abc".into(),
                name: "bash".into(),
                input: r#"{"command":"ls"}"#.into(),
                response: "a.go\nb.go".into(),
                is_error: false,
            },
            ToolRecord {
                id: "toolu_def".into(),
                name: "bash".into(),
                input: r#"{"command":"false"}"#.into(),
                response: "error: exit 1".into(),
                is_error: true,
            },
        ];
        let settings = SessionSettings {
            model: "claude-sonnet-5".into(),
            model_intro: "You are Si, a coding agent.".into(),
            thinking_effort: "medium".into(),
        };

        use hydrogen::types::{TextBlock, ToolResultBlock, ToolUseBlock};

        let messages = vec![
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text(TextBlock::new("list the files"))],
            },
            Message {
                role: Role::Assistant,
                content: vec![
                    reasoning_block(Some("I should list the repo root.")),
                    ContentBlock::ToolUse(ToolUseBlock::new(
                        "toolu_abc",
                        "bash",
                        serde_json::json!({"command": "ls"}),
                    )),
                ],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult(ToolResultBlock {
                    id: "toolu_abc".into(),
                    output: ToolOutput::Text("a.go\nb.go".into()),
                })],
            },
        ];

        let log_dir = write_archive_layout(
            dir.path(),
            now,
            summary,
            &records,
            Some("You are running in Silicon."),
            Some("list the files"),
            &settings,
            &messages,
        )
        .unwrap();
        let base = log_dir.file_name().unwrap().to_string_lossy();
        assert!(
            base.starts_with("20260807-150405-"),
            "dir name missing datetime prefix: {base}"
        );
        assert!(
            base.contains("user-listed-files"),
            "missing sanitized summary: {base}"
        );

        let entries: Vec<_> = fs::read_dir(dir.path().join(".si").join("logs"))
            .unwrap()
            .collect();
        assert_eq!(entries.len(), 1);

        for rec in &records {
            let path = log_dir.join("tool").join(format!("{}.md", rec.id));
            let body = fs::read_to_string(&path).expect("tool file");
            assert!(body.contains(&rec.input), "{body}");
            assert!(body.contains(&rec.response), "{body}");
            assert!(body.contains(&rec.name), "{body}");
        }

        let system_body = fs::read_to_string(log_dir.join("system-prompt.md")).unwrap();
        assert!(system_body.contains("# System prompt"), "{system_body}");
        assert!(
            system_body.contains("You are running in Silicon."),
            "{system_body}"
        );

        let session_body = fs::read_to_string(log_dir.join("session.md")).unwrap();
        assert!(session_body.contains("model: claude-sonnet-5"), "{session_body}");
        assert!(
            session_body.contains("model_intro: You are Si, a coding agent."),
            "{session_body}"
        );
        assert!(
            session_body.contains("thinking_effort: medium"),
            "{session_body}"
        );

        let transcript = fs::read_to_string(log_dir.join("transcript.md")).unwrap();
        assert!(transcript.contains("# Session transcript"), "{transcript}");
        assert!(transcript.contains("list the files"), "{transcript}");
        assert!(
            transcript.contains("### Reasoning summary"),
            "{transcript}"
        );
        assert!(
            transcript.contains("I should list the repo root."),
            "{transcript}"
        );
        assert!(transcript.contains("### Tool use: `bash`"), "{transcript}");
        assert!(transcript.contains("toolu_abc"), "{transcript}");
        assert!(transcript.contains("a.go"), "{transcript}");
    }

    /// Build a Reasoning content block via serde (fields are crate-private).
    fn reasoning_block(summary: Option<&str>) -> ContentBlock {
        serde_json::from_value(serde_json::json!({
            "kind": "reasoning",
            "provider": "anthropic",
            "payload": {"type": "thinking", "thinking": summary.unwrap_or("")},
            "summary": summary,
        }))
        .expect("reasoning block")
    }

    #[test]
    fn format_session_transcript_covers_blocks_and_missing_summary() {
        use hydrogen::types::{TextBlock, ToolResultBlock, ToolUseBlock};

        let messages = vec![
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text(TextBlock::new("fix a bug in main.rs"))],
            },
            Message {
                role: Role::Assistant,
                content: vec![
                    reasoning_block(Some("Check main.rs then run tests.")),
                    ContentBlock::Text(TextBlock::new("Looking at main.rs.")),
                    ContentBlock::ToolUse(ToolUseBlock::new(
                        "t1",
                        "bash",
                        serde_json::json!({"command": "cat main.rs"}),
                    )),
                ],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult(ToolResultBlock {
                    id: "t1".into(),
                    output: ToolOutput::Error("exit 1".into()),
                })],
            },
            Message {
                role: Role::Assistant,
                content: vec![reasoning_block(None)],
            },
        ];
        let body = format_session_transcript(&messages);
        assert!(body.contains("fix a bug in main.rs"), "{body}");
        assert!(body.contains("### Reasoning summary"), "{body}");
        assert!(body.contains("Check main.rs then run tests."), "{body}");
        assert!(body.contains("Looking at main.rs."), "{body}");
        assert!(body.contains("### Tool use: `bash` (`t1`)"), "{body}");
        assert!(body.contains("cat main.rs"), "{body}");
        assert!(body.contains("**error**"), "{body}");
        assert!(body.contains("exit 1"), "{body}");
        assert!(
            body.contains("no summary available"),
            "redacted/missing summary should be noted: {body}"
        );
    }

    #[test]
    fn format_session_transcript_empty() {
        let body = format_session_transcript(&[]);
        assert!(body.contains("empty session"), "{body}");
    }

    #[test]
    fn format_system_prompt_log_falls_back_to_first_user() {
        let body = format_system_prompt_log(None, Some("hello world"));
        assert!(body.contains("# First user message"), "{body}");
        assert!(body.contains("hello world"), "{body}");
        assert!(
            body.contains("System prompt was not stored separately"),
            "{body}"
        );

        let empty = format_system_prompt_log(Some("  "), None);
        assert!(empty.contains("empty session"), "{empty}");
    }

    #[test]
    fn format_session_settings_log_shape() {
        let body = format_session_settings_log(&SessionSettings {
            model: "m".into(),
            model_intro: "intro line".into(),
            thinking_effort: "high".into(),
        });
        assert_eq!(
            body,
            "# Session settings\n\n\
             - model: m\n\
             - model_intro: intro line\n\
             - thinking_effort: high\n"
        );
    }

    #[test]
    fn one_sentence_trims() {
        assert_eq!(one_sentence("  hello world.  "), "hello world.");
        assert_eq!(one_sentence("\"quoted\""), "quoted");
        assert_eq!(one_sentence("first\nsecond"), "first");
    }

    #[test]
    fn format_memory_line_shape() {
        let now = system_time_utc(2020, 1, 2, 3, 4, 5);
        let line = format_memory_line(now, "note");
        assert_eq!(line, "2020-01-02 03:04 note");
        assert!(has_datetime_prefix(&line));
    }
}
