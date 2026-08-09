//! Archive / session-summarization orchestration on `Agent`.

use std::time::{Duration, Instant, SystemTime};

use hydrogen::{ContentBlock, Conversation, RequestOptions, Response, Role};

use super::archive::{
    append_memory, one_sentence, write_archive_layout, ArchiveResult, SessionSettings,
};
use super::config::resolve_model_intro;
use super::turn::{Agent, CompleteFn};

const ARCHIVE_SUMMARY_SYSTEM: &str = "You summarize coding-agent sessions in exactly one sentence.\n\
Reply with a single plain sentence only — no quotes, no bullet points, no preamble.";

const ARCHIVE_SUMMARY_INSTRUCTION: &str = "Summarize this coding-agent session in exactly one sentence, written for your future self as a memory note: what was asked, what you changed, and the outcome.\n\
Reply with a single plain sentence only — no quotes, no bullet points, no preamble. Do not call any tools.";

const ARCHIVE_SUMMARY_MAX_TOKENS: u32 = 4096;
const CACHE_FRESH_WINDOW: Duration = Duration::from_secs(4 * 60);

impl Agent {
    /// Override archive completion (tests). Pass `None` to restore live API.
    pub fn set_complete_fn(&mut self, fn_: Option<CompleteFn>) {
        self.complete_fn = fn_;
    }

    /// Archive: summarize, append memory, write tool logs + session metadata.
    pub async fn archive(&mut self) -> Result<ArchiveResult, String> {
        let summary = self.summarize_session().await?;
        let summary = one_sentence(&summary);
        if summary.is_empty() {
            return Err("empty session summary from model".into());
        }
        let now = SystemTime::now();
        let mem_path = append_memory(&self.cwd, now, &summary)?;
        let system = self.system_prompt();
        let first_user = self.first_user_message_text();
        let settings = SessionSettings {
            model: self.model.clone(),
            model_intro: resolve_model_intro(),
            thinking_effort: self.effort.clone(),
        };
        let log_dir = write_archive_layout(
            &self.cwd,
            now,
            &summary,
            &self.tool_records,
            Some(system.as_str()),
            first_user.as_deref(),
            &settings,
        )?;
        Ok(ArchiveResult {
            summary,
            log_dir,
            memory: mem_path,
        })
    }

    /// Plain text of the first user message in the conversation, if any.
    fn first_user_message_text(&self) -> Option<String> {
        for msg in self.conv.messages() {
            if msg.role != Role::User {
                continue;
            }
            let mut parts = Vec::new();
            for block in &msg.content {
                if let ContentBlock::Text(t) = block {
                    let text = t.text.trim();
                    if !text.is_empty() {
                        parts.push(text.to_string());
                    }
                }
            }
            if !parts.is_empty() {
                return Some(parts.join("\n\n"));
            }
        }
        None
    }

    async fn summarize_session(&mut self) -> Result<String, String> {
        if self.can_summarize_from_history() {
            match self.summarize_from_history().await {
                Ok(s) if !s.trim().is_empty() => return Ok(s),
                Ok(_) => {}
                Err(_) => {}
            }
        }
        self.summarize_compact().await
    }

    fn can_summarize_from_history(&self) -> bool {
        if self.complete_fn.is_some() || self.conv.messages().is_empty() {
            return false;
        }
        match self.last_request_at {
            Some(t) => t.elapsed() < CACHE_FRESH_WINDOW,
            None => false,
        }
    }

    async fn summarize_from_history(&mut self) -> Result<String, String> {
        // Clone conversation, append instruction, send non-streaming.
        let mut conv = self.conv.clone();
        conv.push_user(ARCHIVE_SUMMARY_INSTRUCTION);
        let opts = self.request_options(ARCHIVE_SUMMARY_MAX_TOKENS);
        self.last_request_at = Some(Instant::now());
        let resp = self
            .client
            .send(&conv, &opts)
            .await
            .map_err(|e| e.to_string())?;
        Ok(response_text(&resp))
    }

    async fn summarize_compact(&self) -> Result<String, String> {
        let user = self.session_transcript_for_summary();
        let system = ARCHIVE_SUMMARY_SYSTEM.to_string();
        if let Some(ref fn_) = self.complete_fn {
            return fn_(system, user);
        }
        // Live compact completion via a throwaway conversation.
        let mut conv = Conversation::new();
        conv.push_user(user);
        let opts = RequestOptions {
            model: self.model.clone(),
            system: Some(system),
            max_tokens: Some(256),
            ..Default::default()
        };
        let resp = self
            .client
            .send(&conv, &opts)
            .await
            .map_err(|e| e.to_string())?;
        Ok(response_text(&resp))
    }

    fn session_transcript_for_summary(&self) -> String {
        let mut b = String::from("Summarize this coding-agent session in exactly one sentence.\n\n");
        if self.conv.messages().is_empty() && self.tool_records.is_empty() {
            b.push_str("(empty session — no user turns yet)\n");
            return b;
        }
        for (i, msg) in self.conv.messages().iter().enumerate() {
            let role = match msg.role {
                Role::User => "user",
                Role::Assistant => "assistant",
            };
            b.push_str(&format!("--- message {i} ({role}) ---\n"));
            for block in &msg.content {
                match block {
                    ContentBlock::Text(t) => {
                        let text = &t.text;
                        if text.starts_with("Here is the project's README.md for context:")
                            || text.starts_with("Here is memory from prior sessions:")
                        {
                            b.push_str("[injected context omitted]\n");
                            continue;
                        }
                        let text = if text.len() > 2000 {
                            format!("{}…", &text[..2000])
                        } else {
                            text.clone()
                        };
                        b.push_str(&text);
                        b.push('\n');
                    }
                    ContentBlock::ToolUse(t) => {
                        b.push_str(&format!("[tool_use name={} id={}]\n", t.name, t.id));
                    }
                    ContentBlock::ToolResult(t) => {
                        b.push_str(&format!("[tool_result id={}]\n", t.id));
                    }
                    ContentBlock::Reasoning(_) => {}
                }
            }
        }
        if !self.tool_records.is_empty() {
            b.push_str(&format!(
                "\nTool calls this session: {}\n",
                self.tool_records.len()
            ));
            for rec in &self.tool_records {
                b.push_str(&format!("- {} ({})\n", rec.name, rec.id));
            }
        }
        b
    }
}

fn response_text(resp: &Response) -> String {
    let mut s = String::new();
    for block in &resp.message.content {
        if let ContentBlock::Text(t) = block {
            s.push_str(&t.text);
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    fn write_file(path: &Path, content: &str) {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    #[tokio::test]
    async fn archive_orchestration_with_stubbed_model() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join(".si").join("memory.md"), "old memory line\n");

        let mut a = Agent::new("", dir.path(), "test-model", "");
        a.record_tool(
            "call_1",
            "bash",
            r#"{"command":"echo hi"}"#,
            "hi",
            false,
        );
        // Compact summary builds transcript from conv + tool_records only.
        a.conv.push_user("list the files");

        let saw = Arc::new(Mutex::new((String::new(), String::new())));
        let saw2 = saw.clone();
        const SUMMARY: &str = "User listed repository files with bash.";
        a.set_complete_fn(Some(Arc::new(move |system, user| {
            *saw2.lock().unwrap() = (system, user);
            Ok(SUMMARY.into())
        })));

        let res = a.archive().await.unwrap();
        assert_eq!(res.summary, SUMMARY);

        let (sys, user) = saw.lock().unwrap().clone();
        assert!(!sys.is_empty() && !user.is_empty());
        assert!(user.contains("list the files"), "{user}");

        let mem = std::fs::read_to_string(&res.memory).unwrap();
        assert!(mem.contains("old memory line") && mem.contains(SUMMARY), "{mem}");
        // datetime prefix
        assert!(mem.lines().any(|l| {
            l.len() >= 16 && l.contains(SUMMARY) && l.as_bytes()[4] == b'-'
        }), "{mem}");

        assert!(res.log_dir.exists());
        let tool_path = res.log_dir.join("tool").join("call_1.md");
        let body = std::fs::read_to_string(&tool_path).unwrap();
        assert!(body.contains(r#"{"command":"echo hi"}"#) && body.contains("hi"), "{body}");

        let system_body = std::fs::read_to_string(res.log_dir.join("system-prompt.md")).unwrap();
        assert!(system_body.contains("# System prompt"), "{system_body}");
        assert!(
            system_body.contains("You are running in Silicon"),
            "{system_body}"
        );

        let session_body = std::fs::read_to_string(res.log_dir.join("session.md")).unwrap();
        assert!(session_body.contains("model: test-model"), "{session_body}");
        assert!(
            session_body.contains("thinking_effort: medium"),
            "{session_body}"
        );
        assert!(session_body.contains("model_intro:"), "{session_body}");

        let base = res.log_dir.file_name().unwrap().to_string_lossy();
        let parts: Vec<_> = base.splitn(3, '-').collect();
        assert!(parts.len() >= 3, "{base}");
        assert_eq!(parts[0].len(), 8);
        assert_eq!(parts[1].len(), 6);
        assert!(base.contains(&super::super::archive::sanitize_dir_name(SUMMARY)));
    }
}
