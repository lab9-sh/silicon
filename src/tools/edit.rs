//! `edit_file` host tool — exact string replace / create with line snippets.

use std::fs;
use std::path::{Path, PathBuf};

/// Lines of surrounding context in the post-edit snippet.
pub const EDIT_CONTEXT_LINES: usize = 4;

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct EditInput {
    pub path: String,
    #[serde(default)]
    pub old_string: String,
    #[serde(default)]
    pub new_string: String,
    #[serde(default)]
    pub replace_all: bool,
}

impl EditInput {
    /// Short human-readable summary of the edit for the TUI.
    pub fn display(&self) -> String {
        let path = if self.path.is_empty() {
            "(no path)"
        } else {
            &self.path
        };
        if self.old_string.is_empty() {
            format!("{path} (create)")
        } else if self.new_string.is_empty() {
            format!("{path} (delete text)")
        } else if self.replace_all {
            format!("{path} (replace all)")
        } else {
            format!("{path} (replace)")
        }
    }
}

/// Apply an exact-string edit. Returns `(output, is_error)`.
///
/// Semantics (match Oxygen):
/// - `old_string == ""` creates a new file (parent dirs created); must not exist.
/// - otherwise `old_string` must occur exactly once, unless `replace_all`.
/// - `old_string == new_string` is rejected as a no-op.
pub fn run_edit(cwd: &Path, input: &EditInput) -> (String, bool) {
    let path_raw = input.path.trim();
    if path_raw.is_empty() {
        return ("error: path is required".into(), true);
    }

    let path: PathBuf = if Path::new(path_raw).is_absolute() {
        PathBuf::from(path_raw)
    } else {
        cwd.join(path_raw)
    };
    let rel = rel_to(cwd, &path);

    if input.old_string.is_empty() {
        if path.exists() {
            return (
                format!("error: {rel} already exists; provide old_string to edit it"),
                true,
            );
        }
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                if let Err(e) = fs::create_dir_all(parent) {
                    return (format!("error: {e}"), true);
                }
            }
        }
        if let Err(e) = fs::write(&path, input.new_string.as_bytes()) {
            return (format!("error: {e}"), true);
        }
        let n = line_count(&input.new_string);
        let snip = snippet(&input.new_string, 1, n);
        return (format!("created {rel} ({n} lines)\n{snip}"), false);
    }

    if input.old_string == input.new_string {
        return (
            "error: old_string and new_string are identical; no edit to apply".into(),
            true,
        );
    }

    let content = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (
                format!("error: {rel} does not exist; pass an empty old_string to create it"),
                true,
            );
        }
        Err(e) => return (format!("error: {e}"), true),
    };

    let count = content.matches(&input.old_string).count();
    match count {
        0 => {
            return (
                format!(
                    "error: old_string not found in {rel} (check exact whitespace and indentation)"
                ),
                true,
            );
        }
        n if n > 1 && !input.replace_all => {
            return (
                format!(
                    "error: old_string occurs {n} times in {rel}; include more surrounding context to make it unique, or set replace_all"
                ),
                true,
            );
        }
        _ => {}
    }

    let updated = if input.replace_all {
        content.replace(&input.old_string, &input.new_string)
    } else {
        content.replacen(&input.old_string, &input.new_string, 1)
    };

    let mode = fs::metadata(&path)
        .map(|m| m.permissions())
        .ok();
    if let Err(e) = fs::write(&path, updated.as_bytes()) {
        return (format!("error: {e}"), true);
    }
    if let Some(perms) = mode {
        let _ = fs::set_permissions(&path, perms);
    }

    // Locate the first replacement so the snippet shows the changed region.
    let idx = content.find(&input.old_string).unwrap_or(0);
    let start_line = line_count(&content[..idx]);
    let end_line = start_line + line_count(&input.new_string).saturating_sub(1).max(0);
    // When new_string is empty, line_count is 0 → end_line = start_line - 1.
    // Clamp: treat as a point edit at start_line.
    let end_line = if input.new_string.is_empty() {
        start_line.max(1)
    } else {
        end_line.max(start_line)
    };

    let n = if input.replace_all { count } else { 1 };
    let plural = if n > 1 && input.replace_all {
        "replacements"
    } else {
        "replacement"
    };
    let snip = snippet(&updated, start_line.max(1), end_line.max(1));
    (
        format!("edited {rel} ({n} {plural})\n{snip}"),
        false,
    )
}

fn rel_to(cwd: &Path, path: &Path) -> String {
    path.strip_prefix(cwd)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| path.display().to_string())
}

/// 1-based number of lines `s` spans (0 for empty).
pub fn line_count(s: &str) -> usize {
    if s.is_empty() {
        return 0;
    }
    s.matches('\n').count() + 1
}

/// Render lines `[start_line, end_line]` with `EDIT_CONTEXT_LINES` of context,
/// prefixed by 1-based line numbers.
pub fn snippet(content: &str, start_line: usize, end_line: usize) -> String {
    let lines: Vec<&str> = content.split('\n').collect();
    let start_line = start_line.max(1);
    let from = start_line.saturating_sub(EDIT_CONTEXT_LINES).max(1);
    let mut to = end_line + EDIT_CONTEXT_LINES;
    if to > lines.len() {
        to = lines.len();
    }
    if from > lines.len() {
        return String::new();
    }

    let width = to.to_string().len();
    let mut out = String::new();
    for i in from..=to {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!(
            "{num:>width$}  {line}",
            num = i,
            width = width,
            line = lines[i - 1]
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn write_file(dir: &Path, name: &str, content: &str) -> PathBuf {
        let path = dir.join(name);
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).unwrap();
        }
        fs::write(&path, content).unwrap();
        path
    }

    fn read_file(path: &Path) -> String {
        fs::read_to_string(path).unwrap()
    }

    #[test]
    fn replaces_unique_match() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_file(
            dir.path(),
            "main.go",
            "package main\n\nfunc main() {\n\tprintln(\"hi\")\n}\n",
        );

        let (out, is_err) = run_edit(
            dir.path(),
            &EditInput {
                path: "main.go".into(),
                old_string: "println(\"hi\")".into(),
                new_string: "println(\"hello\")".into(),
                replace_all: false,
            },
        );
        assert!(!is_err, "{out}");
        let got = read_file(&path);
        assert!(got.contains("println(\"hello\")"), "{got}");
        assert!(!got.contains("\"hi\""), "{got}");
        assert!(out.contains("edited main.go (1 replacement)"), "{out}");
        assert!(out.contains("hello"), "snippet should show change: {out}");
    }

    #[test]
    fn ambiguous_match_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_file(dir.path(), "a.txt", "x\nx\n");

        let (out, is_err) = run_edit(
            dir.path(),
            &EditInput {
                path: "a.txt".into(),
                old_string: "x".into(),
                new_string: "y".into(),
                replace_all: false,
            },
        );
        assert!(is_err);
        assert!(out.contains("occurs 2 times"), "{out}");
        assert_eq!(read_file(&path), "x\nx\n");
    }

    #[test]
    fn replace_all() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_file(dir.path(), "a.txt", "x\nx\nx\n");

        let (out, is_err) = run_edit(
            dir.path(),
            &EditInput {
                path: "a.txt".into(),
                old_string: "x".into(),
                new_string: "y".into(),
                replace_all: true,
            },
        );
        assert!(!is_err, "{out}");
        assert!(out.contains("3 replacements"), "{out}");
        assert_eq!(read_file(&path), "y\ny\ny\n");
    }

    #[test]
    fn creates_file_with_parents() {
        let dir = tempfile::tempdir().unwrap();

        let (out, is_err) = run_edit(
            dir.path(),
            &EditInput {
                path: "pkg/new/file.txt".into(),
                old_string: String::new(),
                new_string: "one\ntwo\n".into(),
                replace_all: false,
            },
        );
        assert!(!is_err, "{out}");
        assert!(out.contains("created pkg/new/file.txt"), "{out}");
        assert_eq!(
            read_file(&dir.path().join("pkg/new/file.txt")),
            "one\ntwo\n"
        );
    }

    #[test]
    fn create_refuses_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "a.txt", "keep\n");

        let (out, is_err) = run_edit(
            dir.path(),
            &EditInput {
                path: "a.txt".into(),
                old_string: String::new(),
                new_string: "clobber\n".into(),
                replace_all: false,
            },
        );
        assert!(is_err);
        assert!(out.contains("already exists"), "{out}");
        assert_eq!(read_file(&dir.path().join("a.txt")), "keep\n");
    }

    #[test]
    fn rejects_empty_path_and_noop() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "a.txt", "a\n");

        let (out, is_err) = run_edit(
            dir.path(),
            &EditInput {
                path: String::new(),
                new_string: "x".into(),
                ..Default::default()
            },
        );
        assert!(is_err && out.contains("path is required"), "{out}");

        let (out, is_err) = run_edit(
            dir.path(),
            &EditInput {
                path: "a.txt".into(),
                old_string: "a".into(),
                new_string: "a".into(),
                replace_all: false,
            },
        );
        assert!(is_err && out.contains("identical"), "{out}");
    }

    #[test]
    fn no_match() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "a.txt", "hello\n");
        let (out, is_err) = run_edit(
            dir.path(),
            &EditInput {
                path: "a.txt".into(),
                old_string: "nope".into(),
                new_string: "y".into(),
                replace_all: false,
            },
        );
        assert!(is_err && out.contains("not found"), "{out}");
    }

    #[test]
    fn missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let (out, is_err) = run_edit(
            dir.path(),
            &EditInput {
                path: "nope.txt".into(),
                old_string: "a".into(),
                new_string: "b".into(),
                replace_all: false,
            },
        );
        assert!(is_err && out.contains("does not exist"), "{out}");
    }

    #[test]
    fn preserves_file_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_file(dir.path(), "run.sh", "#!/bin/sh\necho old\n");
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).unwrap();

        let (out, is_err) = run_edit(
            dir.path(),
            &EditInput {
                path: "run.sh".into(),
                old_string: "old".into(),
                new_string: "new".into(),
                replace_all: false,
            },
        );
        assert!(!is_err, "{out}");
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755);
    }

    #[test]
    fn absolute_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_file(dir.path(), "a.txt", "alpha\n");
        let (out, is_err) = run_edit(
            dir.path(),
            &EditInput {
                path: path.to_string_lossy().into(),
                old_string: "alpha".into(),
                new_string: "beta".into(),
                replace_all: false,
            },
        );
        assert!(!is_err, "{out}");
        assert_eq!(read_file(&path), "beta\n");
    }

    #[test]
    fn edit_display() {
        assert_eq!(
            EditInput {
                path: "a.go".into(),
                new_string: "x".into(),
                ..Default::default()
            }
            .display(),
            "a.go (create)"
        );
        assert_eq!(
            EditInput {
                path: "a.go".into(),
                old_string: "x".into(),
                ..Default::default()
            }
            .display(),
            "a.go (delete text)"
        );
        assert_eq!(
            EditInput {
                path: "a.go".into(),
                old_string: "x".into(),
                new_string: "y".into(),
                ..Default::default()
            }
            .display(),
            "a.go (replace)"
        );
        assert_eq!(
            EditInput {
                path: "a.go".into(),
                old_string: "x".into(),
                new_string: "y".into(),
                replace_all: true,
            }
            .display(),
            "a.go (replace all)"
        );
    }

    #[test]
    fn snippet_line_numbers_and_context() {
        let content = "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nl10\nl11\nl12\n";
        let got = snippet(content, 6, 6);
        assert!(got.contains(" 6  l6"), "{got}");
        assert!(got.contains(" 2  l2"), "{got}");
        assert!(got.contains("10  l10"), "{got}");
    }
}
