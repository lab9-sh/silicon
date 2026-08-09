//! Drawing the transcript, status, and input panels.

use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use super::app::{App, LineKind, Mode};
use super::format::{format_window_title, input_placeholder};

impl App {
    pub(crate) fn draw(&mut self, f: &mut Frame) {
        let area = f.area();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(3),
                Constraint::Length(1),
                Constraint::Length(3),
            ])
            .split(area);

        // Transcript
        let lines: Vec<Line> = self
            .messages
            .iter()
            .flat_map(|m| {
                let style = match m.kind {
                    LineKind::User => Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                    LineKind::Agent => Style::default().fg(Color::White),
                    LineKind::Tool => Style::default().fg(Color::Yellow),
                    LineKind::Meta => Style::default().fg(Color::DarkGray),
                    LineKind::Error => Style::default().fg(Color::Red),
                };
                m.text
                    .lines()
                    .map(move |l| Line::from(Span::styled(l.to_string(), style)))
            })
            .collect();

        let total = lines.len() as u16;
        let view_h = chunks[0].height.saturating_sub(2);
        let max_scroll = total.saturating_sub(view_h);
        if self.stick_bottom || self.scroll > max_scroll {
            self.scroll = max_scroll;
            self.stick_bottom = true;
        }

        let title = format_window_title(
            &self.dir_name,
            &self.model_name,
            &self.effort,
            self.mode,
        );
        let para = Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(title))
            .wrap(Wrap { trim: false })
            .scroll((self.scroll, 0));
        f.render_widget(para, chunks[0]);

        // Status
        let status = Paragraph::new(Span::styled(
            self.status.as_str(),
            Style::default().fg(Color::DarkGray),
        ));
        f.render_widget(status, chunks[1]);

        // Input
        let input_display = if self.input.is_empty()
            && matches!(self.mode, Mode::Idle | Mode::LargeResult)
        {
            Span::styled(
                input_placeholder(self.mode),
                Style::default().fg(Color::DarkGray),
            )
        } else {
            Span::raw(format!("┃ {}", self.input))
        };
        let input = Paragraph::new(Line::from(input_display)).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(match self.mode {
                    Mode::Idle | Mode::LargeResult => Style::default().fg(Color::Cyan),
                    _ => Style::default().fg(Color::DarkGray),
                }),
        );
        f.render_widget(input, chunks[2]);
    }
}
