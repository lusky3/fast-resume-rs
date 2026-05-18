use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::session::Session;
use crate::tui::results_list::highlight_text;
use crate::tui::style::{PREVIEW_BORDER, USER_MSG_STYLE, agent_style};

/// Draw the preview pane for the selected session.
///
/// - `session` is `None` when nothing is selected.
/// - `query` is used to highlight matching terms.
/// - `scroll` is the vertical scroll offset (in lines).
pub fn draw_preview(
    f: &mut Frame,
    area: Rect,
    session: Option<&Session>,
    query: &str,
    scroll: u16,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(PREVIEW_BORDER)
        .title(" preview ");

    let Some(session) = session else {
        let para = Paragraph::new(Span::styled(
            "No session selected",
            Style::default().fg(Color::DarkGray),
        ))
        .block(block);
        f.render_widget(para, area);
        return;
    };

    let lines = build_preview_lines(session, query);

    let para = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));

    f.render_widget(para, area);
}

/// Split `session.content` into per-message `Line`s with user/assistant styling.
fn build_preview_lines<'a>(session: &Session, query: &str) -> Vec<Line<'a>> {
    let mut lines: Vec<Line<'a>> = Vec::new();

    // Messages are separated by double newlines.
    for message in session.content.split("\n\n") {
        let message = message.trim();
        if message.is_empty() {
            lines.push(Line::default());
            continue;
        }

        if let Some(body) = message.strip_prefix("» ") {
            // User message
            let body_line = highlight_text(body, query);
            let mut spans = vec![Span::styled("» ", USER_MSG_STYLE)];
            spans.extend(body_line.spans);
            lines.push(Line::from(spans));
        } else {
            // Assistant message — show a badge on the first line.
            let badge = format!("● {} ", session.agent);
            let badge_span = Span::styled(badge, agent_style(&session.agent));

            // Split the message into its lines so we can render the badge only once.
            let mut msg_lines = message.lines();

            let first = msg_lines.next().unwrap_or("");
            let first_hl = highlight_text(first, query);
            let mut first_spans = vec![badge_span];
            first_spans.extend(first_hl.spans);
            lines.push(Line::from(first_spans));

            for rest in msg_lines {
                let hl = highlight_text(rest, query);
                let mut rest_spans = vec![Span::styled(
                    "  ",
                    Style::default().add_modifier(Modifier::DIM),
                )];
                rest_spans.extend(hl.spans);
                lines.push(Line::from(rest_spans));
            }
        }

        // Blank line between messages.
        lines.push(Line::default());
    }

    lines
}

/// Compute the line index of the first query match in `content`.
///
/// Returns `0` if the query is empty or no match is found.
pub fn first_match_line(content: &str, query: &str) -> u16 {
    if query.is_empty() {
        return 0;
    }
    let lower_query = query.to_lowercase();
    // Work line-by-line (same split as build_preview_lines, approximately).
    for (line_idx, line) in content.lines().enumerate() {
        if line.to_lowercase().contains(&lower_query) {
            return line_idx as u16;
        }
    }
    0
}
