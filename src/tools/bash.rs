//! `bash` host tool — `/bin/bash -lc` in the agent cwd with a ~60s timeout.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::oneshot;
use tokio::time::timeout;

pub const BASH_TIMEOUT: Duration = Duration::from_secs(60);

/// Estimated-token threshold above which the agent pauses for user approval
/// before forwarding tool output to the model.
pub const LARGE_TOOL_RESULT_TOKENS: usize = 50_000;

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct BashInput {
    pub command: String,
}

/// Local token estimate (not model-exact). Roughly ~4 chars per token for
/// mixed code/prose; scales with size so large-result gating is meaningful.
pub fn estimate_tokens(s: &str) -> usize {
    // Avoid under-counting very short strings (still positive when non-empty).
    let chars = s.chars().count();
    if chars == 0 {
        return 0;
    }
    (chars + 3) / 4
}

/// Outcome of a bash run that may be cancelled by the agent turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BashOutcome {
    /// Command finished (success or tool-level error). `(output, is_error)`.
    Finished(String, bool),
    /// Turn was cancelled; child process was killed.
    Cancelled,
}

/// Run `command` with `/bin/bash -lc` in `cwd`. Combined stdout/stderr is
/// returned in full. Returns `(output, is_error)`.
///
/// Uncancellable convenience wrapper (tests / simple call sites).
pub async fn run_bash(cwd: &Path, command: &str) -> (String, bool) {
    match run_bash_cancellable(cwd, command, None).await {
        BashOutcome::Finished(out, err) => (out, err),
        BashOutcome::Cancelled => ("error: cancelled".into(), true),
    }
}

/// Run bash, optionally racing against a turn-cancel oneshot. On cancel the
/// child is killed (matches Oxygen's `CommandContext` cancel semantics).
pub async fn run_bash_cancellable(
    cwd: &Path,
    command: &str,
    mut cancel: Option<&mut oneshot::Receiver<()>>,
) -> BashOutcome {
    let command = command.trim();
    if command.is_empty() {
        return BashOutcome::Finished("error: empty command".into(), true);
    }

    let mut child = match Command::new("/bin/bash")
        .arg("-lc")
        .arg(command)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return BashOutcome::Finished(format!("error: {e}"), true),
    };

    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");

    let collect = async {
        let mut out_buf = Vec::new();
        let mut err_buf = Vec::new();
        let (r1, r2) = tokio::join!(
            stdout.read_to_end(&mut out_buf),
            stderr.read_to_end(&mut err_buf)
        );
        let _ = (r1, r2);
        let mut combined = out_buf;
        combined.extend_from_slice(&err_buf);
        String::from_utf8_lossy(&combined).into_owned()
    };

    let work = async {
        let out = collect.await;
        let status = child.wait().await;
        (out, status)
    };

    let timed = timeout(BASH_TIMEOUT, async {
        if let Some(ref mut c) = cancel {
            tokio::select! {
                biased;
                _ = &mut **c => {
                    Err(()) // cancelled
                }
                result = work => Ok(result),
            }
        } else {
            Ok(work.await)
        }
    })
    .await;

    match timed {
        // Outer timeout elapsed.
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            BashOutcome::Finished(
                format!(
                    "error: command timed out after {}s",
                    BASH_TIMEOUT.as_secs()
                ),
                true,
            )
        }
        // Cancelled via oneshot.
        Ok(Err(())) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            BashOutcome::Cancelled
        }
        Ok(Ok((out, status))) => match status {
            Ok(s) if s.success() => {
                if out.is_empty() {
                    BashOutcome::Finished("(no output)".into(), false)
                } else {
                    BashOutcome::Finished(out, false)
                }
            }
            Ok(s) => {
                let exit = s
                    .code()
                    .map(|c| format!("exit status {c}"))
                    .unwrap_or_else(|| "signal".into());
                if out.is_empty() {
                    BashOutcome::Finished(format!("error: {exit}"), true)
                } else {
                    BashOutcome::Finished(format!("{out}\n\n[exit: {exit}]"), true)
                }
            }
            Err(e) => {
                if out.is_empty() {
                    BashOutcome::Finished(format!("error: {e}"), true)
                } else {
                    BashOutcome::Finished(format!("{out}\n\n[exit: {e}]"), true)
                }
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Instant;

    fn run_bash_blocking(cwd: &Path, command: &str) -> (String, bool) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        rt.block_on(run_bash(cwd, command))
    }

    #[test]
    fn run_bash_empty() {
        let (out, is_err) = run_bash_blocking(Path::new("."), "   ");
        assert!(is_err, "expected error, got {out}");
        assert!(out.contains("empty"), "{out}");
    }

    #[test]
    fn run_bash_echo() {
        let (out, is_err) = run_bash_blocking(Path::new("."), "echo hello-silicon");
        assert!(!is_err, "unexpected error: {out}");
        assert!(
            out.contains("hello-silicon"),
            "unexpected output: {out:?}"
        );
    }

    #[test]
    fn run_bash_respects_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("marker.txt");
        std::fs::write(&marker, "ok").unwrap();
        let (out, is_err) = run_bash_blocking(dir.path(), "cat marker.txt");
        assert!(!is_err, "{out}");
        assert!(out.contains("ok"), "{out}");
        let _ = PathBuf::from(".");
    }

    #[test]
    fn run_bash_cancellable_kills_long_command() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let (tx, mut rx) = oneshot::channel();
            let start = Instant::now();
            let handle = tokio::spawn(async move {
                run_bash_cancellable(Path::new("."), "sleep 30", Some(&mut rx)).await
            });
            // Give the child a moment to start, then cancel.
            tokio::time::sleep(Duration::from_millis(100)).await;
            let _ = tx.send(());
            let outcome = handle.await.unwrap();
            assert_eq!(outcome, BashOutcome::Cancelled);
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "cancel should not wait for full sleep: {:?}",
                start.elapsed()
            );
        });
    }

    #[test]
    fn estimate_tokens_scales_with_size() {
        let small = estimate_tokens("hello");
        let mut large_body = String::with_capacity(400_000);
        for i in 0..40_000 {
            large_body.push_str(&format!(
                "line {i}: package main func TestFoo(t *testing.T) {{ t.Log({}) }}\n",
                i * 7
            ));
        }
        let large = estimate_tokens(&large_body);
        assert!(small > 0, "expected positive estimate for small text");
        assert!(large > small, "small={small} large={large}");
        assert!(
            large >= LARGE_TOOL_RESULT_TOKENS,
            "expected large sample above threshold {}: got {large}",
            LARGE_TOOL_RESULT_TOKENS
        );
    }

    #[test]
    fn is_large_threshold_constant() {
        assert_eq!(LARGE_TOOL_RESULT_TOKENS, 50_000);
    }
}
