//! First-turn context assembly: README, memory, user prompt.

use std::path::Path;

use super::config::read_optional_file;

/// Optional first-turn context blocks: README.md then `.si/memory.md`.
pub fn first_turn_context_blocks(cwd: &Path) -> Vec<String> {
    let mut blocks = Vec::new();
    if let Some(readme) = read_optional_file(&cwd.join("README.md")) {
        blocks.push(format!(
            "Here is the project's README.md for context:\n\n{readme}"
        ));
    }
    if let Some(mem) = read_optional_file(&cwd.join(".si").join("memory.md")) {
        blocks.push(format!("Here is memory from prior sessions:\n\n{mem}"));
    }
    blocks
}

/// Shipped context-assembly path used by `RunTurn`.
///
/// When `history_len == 0` (first turn): README then memory (each if present)
/// followed by `prompt`. When `history_len > 0`: only the prompt.
pub fn assemble_user_message_texts(cwd: &Path, history_len: usize, prompt: &str) -> Vec<String> {
    if history_len == 0 {
        let mut parts = first_turn_context_blocks(cwd);
        parts.push(prompt.to_string());
        parts
    } else {
        vec![prompt.to_string()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn write_file(path: &Path, content: &str) {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn first_turn_readme_then_memory_then_prompt() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("README.md"), "README BODY");
        write_file(&dir.path().join(".si").join("memory.md"), "MEMORY BODY");

        let got = assemble_user_message_texts(dir.path(), 0, "user prompt here");
        assert_eq!(got.len(), 3, "{got:?}");
        assert!(got[0].contains("README.md") && got[0].contains("README BODY"), "{}", got[0]);
        assert!(got[1].contains("memory") && got[1].contains("MEMORY BODY"), "{}", got[1]);
        assert_eq!(got[2], "user prompt here");
    }

    #[test]
    fn first_turn_memory_only() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join(".si").join("memory.md"), "only memory");
        let got = assemble_user_message_texts(dir.path(), 0, "hi");
        assert_eq!(got.len(), 2);
        assert!(got[0].contains("only memory"));
        assert_eq!(got[1], "hi");
    }

    #[test]
    fn first_turn_neither() {
        let dir = tempfile::tempdir().unwrap();
        let got = assemble_user_message_texts(dir.path(), 0, "just the prompt");
        assert_eq!(got, vec!["just the prompt".to_string()]);
    }

    #[test]
    fn later_turn_does_not_reinject_context() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("README.md"), "README");
        write_file(&dir.path().join(".si").join("memory.md"), "MEM");

        let first = assemble_user_message_texts(dir.path(), 0, "first");
        assert_eq!(first.len(), 3);

        let later = assemble_user_message_texts(dir.path(), 2, "second turn");
        assert_eq!(later, vec!["second turn".to_string()]);
    }
}
