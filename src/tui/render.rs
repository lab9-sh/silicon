//! Drawing the transcript, status, and input panels.

use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use super::app::{App, LineKind, Mode};
use super::format::{format_window_title, input_placeholder};

/// Max vertical scroll offset so the last *visual* (post-wrap) line can reach
/// the bottom of the viewport. `wrapped_lines` must count rows after wrap.
pub(crate) fn max_scroll_for_view(wrapped_lines: usize, view_height: u16) -> u16 {
    let max = wrapped_lines.saturating_sub(view_height as usize);
    u16::try_from(max).unwrap_or(u16::MAX)
}

/// Count how many terminal rows `lines` occupy when wrapped to `width`.
/// Uses the same wrap algorithm as [`Paragraph`] so scroll matches paint.
pub(crate) fn wrapped_row_count(lines: Vec<Line<'_>>, width: u16) -> usize {
    if width == 0 {
        return 0;
    }
    Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .line_count(width)
}

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

        // Borders consume 2 cols / 2 rows; wrap width and view height are the
        // inner content area. Scroll is applied after wrap (ratatui Paragraph).
        let inner_w = chunks[0].width.saturating_sub(2);
        let view_h = chunks[0].height.saturating_sub(2);
        let total = wrapped_row_count(lines.clone(), inner_w);
        let max_scroll = max_scroll_for_view(total, view_h);
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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::text::Line;

    #[test]
    fn max_scroll_zero_when_content_fits() {
        assert_eq!(max_scroll_for_view(5, 10), 0);
        assert_eq!(max_scroll_for_view(10, 10), 0);
    }

    #[test]
    fn max_scroll_is_excess_rows() {
        assert_eq!(max_scroll_for_view(25, 10), 15);
    }

    #[test]
    fn wrapped_row_count_exceeds_logical_lines() {
        // One logical line, wider than the pane → multiple visual rows.
        let long = "word ".repeat(40); // ~200 chars with spaces (word-wrap friendly)
        let lines = vec![Line::from(long)];
        let logical = 1usize;
        let wrapped = wrapped_row_count(lines, 40);
        assert!(
            wrapped > logical,
            "expected wrap to produce more than {logical} row(s), got {wrapped}"
        );

        // Old bug: max_scroll used logical count → 0, clipping the tail.
        let view_h = 2u16;
        let wrong = max_scroll_for_view(logical, view_h);
        let right = max_scroll_for_view(wrapped, view_h);
        assert_eq!(wrong, 0);
        assert!(right > wrong, "wrap-aware max_scroll ({right}) must exceed unwrapped ({wrong})");
    }

    #[test]
    fn empty_width_yields_zero_rows() {
        assert_eq!(wrapped_row_count(vec![Line::from("hello")], 0), 0);
    }
}
