use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    widgets::{Block, Borders, Paragraph},
};
use tui_input::Input;

use crate::tui::style::{SEARCH_BORDER_ACTIVE, SEARCH_BORDER_INACTIVE};

/// Spinner frames cycled at ~80 ms intervals.
const SPINNER_FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Draw the search input box.
///
/// - `active` controls the border colour (cyan when focused).
/// - `spinner_frame` is cycled externally; the input icon becomes a spinner when `is_loading`.
pub fn draw_search_input(
    f: &mut Frame,
    area: Rect,
    input: &Input,
    is_loading: bool,
    active: bool,
    spinner_frame: usize,
) {
    let border_style: Style = if active {
        SEARCH_BORDER_ACTIVE
    } else {
        SEARCH_BORDER_INACTIVE
    };

    let icon = if is_loading {
        SPINNER_FRAMES[spinner_frame % SPINNER_FRAMES.len()]
    } else {
        '🔍'
    };

    let title = format!(" {icon} search ");

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(title.as_str());

    let inner = block.inner(area);
    let scroll = input.visual_scroll(inner.width as usize);

    let paragraph = Paragraph::new(input.value()).scroll((0, scroll as u16)).block(block);

    f.render_widget(paragraph, area);

    // Place the terminal cursor inside the input box.
    let cursor_x = inner.x + (input.visual_cursor().saturating_sub(scroll)) as u16;
    f.set_cursor_position((cursor_x, inner.y));
}
