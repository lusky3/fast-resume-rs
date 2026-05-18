use ratatui::{
    Frame,
    layout::{Constraint, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Row, Table, TableState},
};

use crate::session::Session;
use crate::tui::style::{SELECTED_ROW, agent_color};

/// Highlight occurrences of `query` (case-insensitive) in `source` with bold yellow.
///
/// Returns a `Line` composed of alternating plain and highlighted `Span`s.
pub fn highlight_text(source: &str, query: &str) -> Line<'static> {
    if query.is_empty() {
        return Line::from(source.to_owned());
    }

    let lower_source = source.to_lowercase();
    let lower_query = query.to_lowercase();

    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut last = 0usize;

    // Walk through all case-insensitive occurrences of query in source.
    while let Some(pos) = lower_source[last..].find(&lower_query) {
        let abs_pos = last + pos;
        let end = abs_pos + lower_query.len();

        // Plain text before the match.
        if abs_pos > last {
            spans.push(Span::raw(source[last..abs_pos].to_owned()));
        }
        // The matched text, styled.
        spans.push(Span::styled(
            source[abs_pos..end].to_owned(),
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(ratatui::style::Color::Yellow),
        ));

        last = end;
    }

    // Any remaining plain text.
    if last < source.len() {
        spans.push(Span::raw(source[last..].to_owned()));
    }

    Line::from(spans)
}

/// Format the session age as a human-readable string.
fn format_age(session: &Session) -> String {
    let now = jiff::Timestamp::now();
    let diff_secs = now.as_second() - session.timestamp.as_second();
    if diff_secs < 0 {
        return "now".to_owned();
    }
    let diff_secs = diff_secs as u64;
    if diff_secs < 60 {
        format!("{diff_secs}s")
    } else if diff_secs < 3600 {
        format!("{}m", diff_secs / 60)
    } else if diff_secs < 86400 {
        format!("{}h", diff_secs / 3600)
    } else {
        format!("{}d", diff_secs / 86400)
    }
}

/// Shorten a directory path by replacing the home directory with `~`.
fn shorten_dir(dir: &str) -> String {
    if let Some(home) = std::env::var_os("HOME") {
        let home_str = home.to_string_lossy();
        if let Some(rest) = dir.strip_prefix(home_str.as_ref()) {
            return format!("~{rest}");
        }
    }
    dir.to_owned()
}

/// Draw the results table.
pub fn draw_results(
    f: &mut Frame,
    area: Rect,
    sessions: &[Session],
    table_state: &mut TableState,
    query: &str,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} session(s) ", sessions.len()));

    let rows: Vec<Row> = sessions
        .iter()
        .map(|s| {
            let agent_span = Span::styled(
                format!(" {} ", s.agent),
                Style::default().fg(agent_color(&s.agent)),
            );
            let title_line = highlight_text(&s.title, query);
            let dir_span = Span::styled(
                shorten_dir(&s.directory),
                Style::default().fg(ratatui::style::Color::DarkGray),
            );
            let turns_span = Span::raw(s.message_count.to_string());
            let age_span = Span::styled(
                format_age(s),
                Style::default().fg(ratatui::style::Color::DarkGray),
            );

            Row::new(vec![
                ratatui::widgets::Cell::from(Line::from(agent_span)),
                ratatui::widgets::Cell::from(title_line),
                ratatui::widgets::Cell::from(Line::from(dir_span)),
                ratatui::widgets::Cell::from(Line::from(turns_span)),
                ratatui::widgets::Cell::from(Line::from(age_span)),
            ])
        })
        .collect();

    // Responsive column widths: fixed columns for agent, turns, age; title + dir fill the rest.
    let w = area.width.saturating_sub(2); // subtract borders
    let agent_w = 18u16;
    let turns_w = 6u16;
    let age_w = 6u16;
    let remaining = w.saturating_sub(agent_w + turns_w + age_w);
    let title_w = (remaining * 60 / 100).max(10);
    let dir_w = remaining.saturating_sub(title_w);

    let widths = [
        Constraint::Length(agent_w),
        Constraint::Length(title_w),
        Constraint::Length(dir_w),
        Constraint::Length(turns_w),
        Constraint::Length(age_w),
    ];

    let header = Row::new(["Agent", "Title", "Directory", "Turns", "Age"])
        .style(Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED));

    let table = Table::new(rows, widths)
        .header(header)
        .block(block)
        .row_highlight_style(SELECTED_ROW)
        .highlight_symbol("▶ ");

    f.render_stateful_widget(table, area, table_state);
}
