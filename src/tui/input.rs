//! Mode-specific keyboard handling.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use tokio::runtime::Handle;

use crate::agent::LargeToolResultReply;

use super::app::{App, LineKind, Mode};

impl App {
    pub(crate) fn handle_key(&mut self, key: KeyEvent, rt: &Handle) {
        if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
            return;
        }

        // Scroll keys work in every mode.
        if self.handle_scroll_key(key) {
            return;
        }

        match self.mode {
            Mode::BudgetPause => self.handle_key_budget(key),
            Mode::LargeResult => self.handle_key_large_result(key),
            Mode::Running | Mode::Archiving => self.handle_key_running(key),
            Mode::Idle => self.handle_key_idle(key, rt),
        }
    }

    fn handle_scroll_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::PageUp => {
                self.stick_bottom = false;
                self.scroll = self.scroll.saturating_sub(10);
                true
            }
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_add(10);
                true
            }
            KeyCode::Up if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.stick_bottom = false;
                self.scroll = self.scroll.saturating_sub(1);
                true
            }
            KeyCode::Down if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.scroll = self.scroll.saturating_add(1);
                true
            }
            _ => false,
        }
    }

    fn handle_key_budget(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.interrupt_or_quit();
            return;
        }
        match key.code {
            KeyCode::Char('c') | KeyCode::Char('C') => {
                if let Some(tx) = self.budget_reply.take() {
                    let _ = tx.send(true);
                }
                self.mode = Mode::Running;
                self.status = "continuing (+100k)…".into();
                self.append(LineKind::Meta, "Continuing with +100k session budget.");
            }
            KeyCode::Char('s') | KeyCode::Char('S') => {
                if let Some(tx) = self.budget_reply.take() {
                    let _ = tx.send(false);
                }
                self.mode = Mode::Running;
                self.status = "stopping…".into();
                self.append(
                    LineKind::Meta,
                    "Stopped at session context budget. Start a new session for a clean window.",
                );
            }
            _ => {}
        }
    }

    fn handle_key_large_result(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('c') => {
                    self.interrupt_or_quit();
                    return;
                }
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    if let Some(tx) = self.large_reply.take() {
                        let _ = tx.send(LargeToolResultReply {
                            approve: true,
                            message: String::new(),
                        });
                    }
                    self.input.clear();
                    self.mode = Mode::Running;
                    self.status = "approved large tool result…".into();
                    self.append(LineKind::Meta, "Approved large tool result.");
                    return;
                }
                _ => {}
            }
        }
        match key.code {
            KeyCode::Enter => {
                let guidance = self.input.clone(); // verbatim
                if let Some(tx) = self.large_reply.take() {
                    let _ = tx.send(LargeToolResultReply {
                        approve: false,
                        message: guidance,
                    });
                }
                self.input.clear();
                self.mode = Mode::Running;
                self.status = "denied large tool result…".into();
                self.append(
                    LineKind::Meta,
                    "Denied large tool result; sent your guidance.",
                );
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.input.push(c);
            }
            KeyCode::Backspace => {
                self.input.pop();
            }
            _ => {}
        }
    }

    fn handle_key_running(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.interrupt_or_quit();
        }
    }

    fn handle_key_idle(&mut self, key: KeyEvent, rt: &Handle) {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.quit();
            return;
        }
        match key.code {
            KeyCode::Esc => {
                self.input.clear();
            }
            KeyCode::Enter => {
                self.submit_prompt(rt);
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.input.push(c);
            }
            KeyCode::Backspace => {
                self.input.pop();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEventState;

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: mods,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    #[test]
    fn budget_continue_and_stop_transitions() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let agent = crate::agent::Agent::new("", std::env::temp_dir(), "m", "medium");
        let mut app = App::new(agent);
        let (tx, _rx) = tokio::sync::oneshot::channel();
        app.mode = Mode::BudgetPause;
        app.budget_reply = Some(tx);

        app.handle_key(key(KeyCode::Char('c'), KeyModifiers::NONE), rt.handle());
        assert_eq!(app.mode, Mode::Running);
        assert!(app.status.contains("continuing") || app.status.contains("+100k"));

        let (tx, _rx) = tokio::sync::oneshot::channel();
        app.mode = Mode::BudgetPause;
        app.budget_reply = Some(tx);
        app.handle_key(key(KeyCode::Char('s'), KeyModifiers::NONE), rt.handle());
        assert_eq!(app.mode, Mode::Running);
        assert!(app.status.contains("stopping"));
    }

    #[test]
    fn large_result_approve_clears_input_and_runs() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let agent = crate::agent::Agent::new("", std::env::temp_dir(), "m", "medium");
        let mut app = App::new(agent);
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        app.mode = Mode::LargeResult;
        app.large_reply = Some(tx);
        app.input = "should clear".into();

        app.handle_key(
            key(KeyCode::Char('y'), KeyModifiers::CONTROL),
            rt.handle(),
        );
        assert_eq!(app.mode, Mode::Running);
        assert!(app.input.is_empty());
        let reply = rx.try_recv().expect("reply sent");
        assert!(reply.approve);
    }

    #[test]
    fn idle_char_types_into_input() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let agent = crate::agent::Agent::new("", std::env::temp_dir(), "m", "medium");
        let mut app = App::new(agent);
        assert_eq!(app.mode, Mode::Idle);
        app.handle_key(key(KeyCode::Char('h'), KeyModifiers::NONE), rt.handle());
        app.handle_key(key(KeyCode::Char('i'), KeyModifiers::NONE), rt.handle());
        assert_eq!(app.input, "hi");
    }

    #[test]
    fn page_up_unsticks_scroll() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let agent = crate::agent::Agent::new("", std::env::temp_dir(), "m", "medium");
        let mut app = App::new(agent);
        app.scroll = 20;
        app.stick_bottom = true;
        app.handle_key(key(KeyCode::PageUp, KeyModifiers::NONE), rt.handle());
        assert!(!app.stick_bottom);
        assert_eq!(app.scroll, 10);
    }
}
