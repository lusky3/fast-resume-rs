use std::sync::{
    Arc,
    mpsc::{self, Receiver},
};
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::{
    DefaultTerminal, Frame,
    crossterm::{
        event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent,
                KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind},
        execute,
    },
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use tui_input::Input;
use tui_input::backend::crossterm::EventHandler;

use crate::session::Session;
use crate::search::SessionSearch;
use crate::tui::{
    TuiResult,
    filter_bar::{FILTER_AGENTS, draw_filter_bar},
    icons::IconCache,
    modal::{ModalFocus, draw_modal},
    preview::{draw_preview, first_match_line},
    results_list::draw_results,
};

/// Options passed into `run_tui`.
pub struct TuiOpts<'a> {
    /// Pre-fill the search box with this query string.
    pub initial_query: &'a str,
    /// When `true`, create an `IconCache` using Unicode half-blocks instead of
    /// attempting Sixel/Kitty auto-detection.
    pub no_images: bool,
    /// When `true`, skip the yolo modal and always pass yolo flags.
    pub yolo: bool,
}

/// Messages sent from the background indexing thread to the event loop.
enum IndexMsg {
    Done(Vec<Session>),
    Error(String),
}

/// The application state.
pub struct State {
    pub input: Input,
    pub results: Vec<Session>,
    pub table_state: ratatui::widgets::TableState,
    pub preview_scroll: u16,
    pub is_loading: bool,
    pub exit_with: Option<(Vec<String>, String)>,
    /// Active agent filter — `None` means "show all agents".
    pub active_agent_filter: Option<String>,
    /// Icon cache (loaded lazily per agent).
    pub icons: IconCache,
    /// Whether the yolo modal is open.
    pub modal_open: bool,
    /// Current yolo checkbox state in the modal.
    pub modal_yolo: bool,
    /// Which button has focus in the modal.
    pub modal_focus: ModalFocus,
    /// Global yolo flag (set by --yolo CLI flag).
    pub global_yolo: bool,
    /// Autocomplete suggestion for the current input.
    pub suggestion: Option<String>,
    /// Brief notification message shown in the title bar.
    pub notification: Option<(String, Instant)>,
    /// Terminal size on the last draw — used to map mouse coordinates to widgets.
    pub last_term_size: (u16, u16),
    /// Spinner animation frame index.
    spinner_frame: usize,
    /// When the spinner frame was last advanced.
    last_spinner_tick: Instant,
    /// When the last keystroke that modified the query occurred.
    last_search_at: Instant,
    /// The query text that was last submitted to the search engine.
    current_query: String,
    /// Channel receiving indexed sessions from the background thread.
    index_rx: Receiver<IndexMsg>,
    /// Handle to the search engine.
    search: Arc<SessionSearch>,
    /// Whether the initial background load is complete.
    initial_load_done: bool,
}

impl State {
    fn new(
        initial_query: &str,
        search: Arc<SessionSearch>,
        index_rx: Receiver<IndexMsg>,
        no_images: bool,
        yolo: bool,
    ) -> Self {
        let mut table_state = ratatui::widgets::TableState::default();
        table_state.select(Some(0));

        let icons = if no_images {
            IconCache::halfblocks()
        } else {
            IconCache::new().unwrap_or_else(|_| IconCache::halfblocks())
        };

        let suggestion = compute_suggestion(initial_query);

        Self {
            input: Input::from(initial_query),
            results: Vec::new(),
            table_state,
            preview_scroll: 0,
            is_loading: true,
            exit_with: None,
            active_agent_filter: None,
            icons,
            modal_open: false,
            modal_yolo: false,
            modal_focus: ModalFocus::Launch,
            global_yolo: yolo,
            suggestion,
            notification: None,
            last_term_size: (0, 0),
            spinner_frame: 0,
            last_spinner_tick: Instant::now(),
            last_search_at: Instant::now(),
            current_query: initial_query.to_owned(),
            index_rx,
            search,
            initial_load_done: false,
        }
    }

    /// Cycle the active agent filter forward through FILTER_AGENTS (plus "all").
    pub fn cycle_agent_filter_forward(&mut self) {
        self.active_agent_filter = match self.active_agent_filter.as_deref() {
            None => Some(FILTER_AGENTS[0].to_string()),
            Some(current) => {
                let idx = FILTER_AGENTS.iter().position(|a| *a == current);
                match idx {
                    Some(i) if i + 1 < FILTER_AGENTS.len() => {
                        Some(FILTER_AGENTS[i + 1].to_string())
                    }
                    _ => None,
                }
            }
        };
    }

    /// Cycle the active agent filter backward through FILTER_AGENTS (plus "all").
    pub fn cycle_agent_filter_backward(&mut self) {
        self.active_agent_filter = match self.active_agent_filter.as_deref() {
            None => Some(FILTER_AGENTS[FILTER_AGENTS.len() - 1].to_string()),
            Some(current) => {
                let idx = FILTER_AGENTS.iter().position(|a| *a == current);
                match idx {
                    Some(0) | None => None,
                    Some(i) => Some(FILTER_AGENTS[i - 1].to_string()),
                }
            }
        };
    }

    /// Process any pending messages from the background thread and handle debounced search.
    fn tick(&mut self) {
        // Advance spinner every ~80 ms.
        if self.last_spinner_tick.elapsed() >= Duration::from_millis(80) {
            self.spinner_frame = self.spinner_frame.wrapping_add(1);
            self.last_spinner_tick = Instant::now();
        }

        // Expire notification after 3 seconds.
        if let Some((_, ts)) = &self.notification {
            if ts.elapsed() >= Duration::from_secs(3) {
                self.notification = None;
            }
        }

        // Drain the channel.
        while let Ok(msg) = self.index_rx.try_recv() {
            match msg {
                IndexMsg::Done(sessions) => {
                    self.initial_load_done = true;
                    self.is_loading = false;
                    let query = self.input.value().to_owned();
                    if query.is_empty() {
                        self.results = sessions;
                    } else {
                        match self.search.search(&query, 100) {
                            Ok(hits) => self.results = hits,
                            Err(_) => self.results = sessions,
                        }
                    }
                    self.current_query = self.input.value().to_owned();
                    self.clamp_selection();
                    self.reset_preview_if_selection_changed();
                }
                IndexMsg::Error(e) => {
                    eprintln!("Indexing error: {e}");
                    self.is_loading = false;
                    self.initial_load_done = true;
                }
            }
        }

        // Debounced search.
        if self.initial_load_done
            && !self.is_loading
            && self.last_search_at.elapsed() >= Duration::from_millis(50)
            && self.input.value() != self.current_query
        {
            let query = self.input.value().to_owned();
            let results = if query.is_empty() {
                self.search.get_all_sessions().unwrap_or_default()
            } else {
                self.search.search(&query, 100).unwrap_or_default()
            };
            self.results = results;
            self.current_query = query.clone();
            self.clamp_selection();
            self.reset_preview_scroll_for_query(&query);
        }
    }

    fn clamp_selection(&mut self) {
        if self.results.is_empty() {
            self.table_state.select(None);
        } else {
            let current = self.table_state.selected().unwrap_or(0);
            let clamped = current.min(self.results.len().saturating_sub(1));
            self.table_state.select(Some(clamped));
        }
    }

    fn reset_preview_if_selection_changed(&mut self) {
        self.preview_scroll = 0;
    }

    fn reset_preview_scroll_for_query(&mut self, query: &str) {
        let scroll = self
            .selected_session()
            .map(|s| first_match_line(&s.content, query))
            .unwrap_or(0);
        self.preview_scroll = scroll;
    }

    fn selected_session(&self) -> Option<&Session> {
        let idx = self.table_state.selected()?;
        self.results.get(idx)
    }

    fn select_next(&mut self) {
        if self.results.is_empty() {
            return;
        }
        let next = self
            .table_state
            .selected()
            .map(|i| (i + 1).min(self.results.len().saturating_sub(1)))
            .unwrap_or(0);
        self.table_state.select(Some(next));
        self.preview_scroll = 0;
    }

    fn select_prev(&mut self) {
        if self.results.is_empty() {
            return;
        }
        let prev = self
            .table_state
            .selected()
            .map(|i| i.saturating_sub(1))
            .unwrap_or(0);
        self.table_state.select(Some(prev));
        self.preview_scroll = 0;
    }

    fn select_next_page(&mut self) {
        if self.results.is_empty() {
            return;
        }
        let next = self
            .table_state
            .selected()
            .map(|i| (i + 10).min(self.results.len().saturating_sub(1)))
            .unwrap_or(0);
        self.table_state.select(Some(next));
        self.preview_scroll = 0;
    }

    fn select_prev_page(&mut self) {
        if self.results.is_empty() {
            return;
        }
        let prev = self
            .table_state
            .selected()
            .map(|i| i.saturating_sub(10))
            .unwrap_or(0);
        self.table_state.select(Some(prev));
        self.preview_scroll = 0;
    }

    /// Copy the resume command for the selected session to the clipboard.
    fn copy_resume_command(&mut self) {
        let Some(session) = self.selected_session().cloned() else {
            return;
        };

        let adapter = self.search.get_adapter_for_agent(&session.agent);
        let cmd = adapter
            .map(|a| a.get_resume_command(&session, self.global_yolo))
            .unwrap_or_else(|| build_resume_command(&session));

        let text = format!("cd {} && {}", session.directory, cmd.join(" "));

        match arboard::Clipboard::new() {
            Ok(mut cb) => {
                if cb.set_text(&text).is_ok() {
                    self.notification = Some(("Copied!".to_owned(), Instant::now()));
                } else {
                    self.notification = Some(("Copy failed".to_owned(), Instant::now()));
                }
            }
            Err(_) => {
                self.notification = Some(("Clipboard unavailable".to_owned(), Instant::now()));
            }
        }
    }
}

/// Compute an autocomplete suggestion for the current input value.
///
/// When the query ends with `agent:` followed by a partial agent name, find the
/// first FILTER_AGENTS entry that starts with the partial text and return the
/// full completion string.
pub fn compute_suggestion(value: &str) -> Option<String> {
    // Find the last `agent:` token in the input.
    let prefix = "agent:";
    let pos = value.rfind(prefix)?;
    let after = &value[pos + prefix.len()..];

    // Only suggest when there is no space after the prefix (still typing).
    if after.contains(' ') {
        return None;
    }
    // Strip any leading `-` or `!` from the partial.
    let partial = after.trim_start_matches(['-', '!']);

    let completion = FILTER_AGENTS
        .iter()
        .find(|&&a| a.starts_with(partial) && a.len() > partial.len())?;

    // Build the full suggestion by replacing the partial with the completion.
    let suggestion = format!("{}{}", &value[..pos + prefix.len()], completion);
    Some(suggestion)
}

/// Entry point for the TUI.
pub fn run_tui(opts: TuiOpts<'_>) -> Result<TuiResult> {
    let search = Arc::new(SessionSearch::new());

    let (tx, rx) = mpsc::channel::<IndexMsg>();
    let search_clone = Arc::clone(&search);
    std::thread::spawn(move || {
        match search_clone.get_all_sessions() {
            Ok(sessions) => {
                let _ = tx.send(IndexMsg::Done(sessions));
            }
            Err(e) => {
                let _ = tx.send(IndexMsg::Error(e.to_string()));
            }
        }
    });

    let mut terminal = ratatui::init();
    // Enable mouse capture so clicks and scroll events reach the event loop.
    let _ = execute!(std::io::stderr(), EnableMouseCapture);
    let result = run_app(
        &mut terminal,
        opts.initial_query,
        opts.no_images,
        opts.yolo,
        search,
        rx,
    );
    // Disable mouse before restoring the terminal.
    let _ = execute!(std::io::stderr(), DisableMouseCapture);
    ratatui::restore();

    result
}

fn run_app(
    terminal: &mut DefaultTerminal,
    initial_query: &str,
    no_images: bool,
    yolo: bool,
    search: Arc<SessionSearch>,
    rx: Receiver<IndexMsg>,
) -> Result<TuiResult> {
    let mut state = State::new(initial_query, search, rx, no_images, yolo);

    loop {
        terminal.draw(|f| draw(f, &mut state))?;

        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key)
                    if key.kind == KeyEventKind::Press && handle_key(&mut state, key) =>
                {
                    break;
                }
                Event::Mouse(mouse) => handle_mouse(&mut state, mouse),
                Event::Resize(_, _) | Event::Key(_) => {}
                _ => {}
            }
        }

        state.tick();

        if state.exit_with.is_some() {
            break;
        }
    }

    let result = match state.exit_with {
        Some((cmd, dir)) => TuiResult {
            resume_command: Some(cmd),
            resume_dir: Some(dir),
        },
        None => TuiResult {
            resume_command: None,
            resume_dir: None,
        },
    };

    Ok(result)
}

/// Handle a mouse event, mapping coordinates to logical TUI regions.
///
/// Layout (from `draw`):
///   row 0        — title bar
///   rows 1–3     — search box  (height 3, includes borders)
///   row 4        — filter bar  (height 1)
///   rows 5..H-2  — main area   (60% results | 40% preview)
///   row H-1      — footer
fn handle_mouse(state: &mut State, mouse: MouseEvent) {
    let (width, height) = state.last_term_size;
    if width == 0 || height == 0 {
        return;
    }
    let row = mouse.row;
    let filter_row: u16 = 4;
    let main_top: u16 = 5;
    let footer_row = height.saturating_sub(1);
    let main_height = footer_row.saturating_sub(main_top);
    let results_cols = (width as f32 * 0.6).round() as u16;

    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if row == filter_row {
                // Filter bar click: map column → agent button.
                let n = (FILTER_AGENTS.len() + 1) as u16; // +1 for "All"
                let btn_width = (width / n).max(1);
                let idx = (mouse.column / btn_width) as usize;
                if idx == 0 {
                    state.active_agent_filter = None;
                } else if idx <= FILTER_AGENTS.len() {
                    let agent = FILTER_AGENTS[idx - 1].to_string();
                    if state.active_agent_filter.as_deref() == Some(&agent) {
                        state.active_agent_filter = None; // toggle off
                    } else {
                        state.active_agent_filter = Some(agent);
                    }
                }
            } else if row >= main_top
                && row < main_top + main_height
                && mouse.column < results_cols
            {
                // Results table click: select the clicked row.
                // The table has a 1-row header — subtract 1.
                let clicked = (row - main_top).saturating_sub(1) as usize;
                if clicked < state.results.len() {
                    state.table_state.select(Some(clicked));
                    state.preview_scroll = 0;
                }
            }
        }
        MouseEventKind::ScrollDown => {
            if row >= main_top && row < main_top + main_height {
                if mouse.column < results_cols {
                    state.select_next();
                } else {
                    state.preview_scroll = state.preview_scroll.saturating_add(3);
                }
            }
        }
        MouseEventKind::ScrollUp => {
            if row >= main_top && row < main_top + main_height {
                if mouse.column < results_cols {
                    state.select_prev();
                } else {
                    state.preview_scroll = state.preview_scroll.saturating_sub(3);
                }
            }
        }
        _ => {}
    }
}

/// Handle a key press. Returns `true` when the event loop should exit.
fn handle_key(state: &mut State, key: KeyEvent) -> bool {
    // Ctrl+C — always quit.
    if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('c') {
        return true;
    }

    // ── Modal mode ───────────────────────────────────────────────────────────
    if state.modal_open {
        return handle_modal_key(state, key);
    }

    match key.code {
        // Navigation
        KeyCode::Down | KeyCode::Char('j') => {
            state.select_next();
        }
        KeyCode::Up | KeyCode::Char('k') => {
            state.select_prev();
        }
        KeyCode::PageDown => {
            state.select_next_page();
        }
        KeyCode::PageUp => {
            state.select_prev_page();
        }

        // Preview scroll
        KeyCode::Char('d') if key.modifiers == KeyModifiers::CONTROL => {
            state.preview_scroll = state.preview_scroll.saturating_add(5);
        }
        KeyCode::Char('u') if key.modifiers == KeyModifiers::CONTROL => {
            state.preview_scroll = state.preview_scroll.saturating_sub(5);
        }

        // Quit
        KeyCode::Esc | KeyCode::Char('q') => {
            return true;
        }

        // Copy resume command to clipboard.
        KeyCode::Char('c') if key.modifiers != KeyModifiers::CONTROL => {
            state.copy_resume_command();
        }

        // Resume
        KeyCode::Enter => {
            if let Some(session) = state.selected_session().cloned() {
                let adapter = state.search.get_adapter_for_agent(&session.agent);
                let needs_modal = adapter
                    .map(|a| a.supports_yolo())
                    .unwrap_or(false)
                    && !state.global_yolo;

                if needs_modal {
                    state.modal_open = true;
                    state.modal_focus = ModalFocus::Launch;
                    state.modal_yolo = false;
                } else {
                    let yolo = state.global_yolo;
                    let cmd = adapter
                        .map(|a| a.get_resume_command(&session, yolo))
                        .unwrap_or_else(|| build_resume_command(&session));
                    let dir = session.directory.clone();
                    state.exit_with = Some((cmd, dir));
                    return true;
                }
            }
        }

        // Tab — accept autocomplete suggestion OR cycle filter.
        KeyCode::Tab => {
            if let Some(suggestion) = state.suggestion.clone() {
                // Accept the suggestion.
                state.input = Input::from(suggestion.as_str());
                state.suggestion = None;
                state.last_search_at = Instant::now();
            } else {
                state.cycle_agent_filter_forward();
            }
        }
        // Shift+Tab — cycle backward through agent filters.
        KeyCode::BackTab => {
            state.cycle_agent_filter_backward();
        }

        // All other printable keys + Backspace → search input.
        _ => {
            let prev_value = state.input.value().to_owned();
            state.input.handle_event(&Event::Key(key));
            let new_value = state.input.value().to_owned();
            if new_value != prev_value {
                state.last_search_at = Instant::now();
                state.is_loading = state.initial_load_done;
                state.suggestion = compute_suggestion(&new_value);
            }
        }
    }

    false
}

/// Handle a key press when the yolo modal is open.  Returns `true` to quit.
fn handle_modal_key(state: &mut State, key: KeyEvent) -> bool {
    match key.code {
        // Dismiss.
        KeyCode::Esc | KeyCode::Char('q') => {
            state.modal_open = false;
        }

        // Toggle yolo.
        KeyCode::Char(' ') | KeyCode::Char('y') => {
            state.modal_yolo = true;
        }
        KeyCode::Char('n') => {
            state.modal_yolo = false;
        }

        // Cycle button focus.
        KeyCode::Tab | KeyCode::Left | KeyCode::Right => {
            state.modal_focus = state.modal_focus.toggle();
        }

        // Confirm.
        KeyCode::Enter => {
            match state.modal_focus {
                ModalFocus::Cancel => {
                    state.modal_open = false;
                }
                ModalFocus::Launch => {
                    if let Some(session) = state.selected_session().cloned() {
                        let yolo = state.modal_yolo || state.global_yolo;
                        let adapter = state.search.get_adapter_for_agent(&session.agent);
                        let cmd = adapter
                            .map(|a| a.get_resume_command(&session, yolo))
                            .unwrap_or_else(|| build_resume_command(&session));
                        let dir = session.directory.clone();
                        state.modal_open = false;
                        state.exit_with = Some((cmd, dir));
                        return true;
                    }
                    state.modal_open = false;
                }
            }
        }

        _ => {}
    }
    false
}

/// Fallback resume command when no adapter is found.
fn build_resume_command(session: &Session) -> Vec<String> {
    match session.agent.as_str() {
        "claude" => vec!["claude".to_owned(), "--resume".to_owned(), session.id.clone()],
        "codex" => vec!["codex".to_owned(), "--session".to_owned(), session.id.clone()],
        "copilot-cli" => vec![
            "gh".to_owned(),
            "copilot".to_owned(),
            "resume".to_owned(),
            session.id.clone(),
        ],
        "vibe" => vec!["vibe".to_owned(), "--session".to_owned(), session.id.clone()],
        "kiro" => vec!["kiro".to_owned(), "--session".to_owned(), session.id.clone()],
        _ => vec![
            session.agent.clone(),
            "--session".to_owned(),
            session.id.clone(),
        ],
    }
}

/// Top-level draw function.
pub fn draw(f: &mut Frame, state: &mut State) {
    let area = f.area();
    state.last_term_size = (area.width, area.height);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title bar
            Constraint::Length(3), // search box (with border)
            Constraint::Length(1), // filter bar
            Constraint::Min(0),    // main area
            Constraint::Length(1), // footer hints
        ])
        .split(area);

    draw_title(f, chunks[0], state);

    draw_search_input_with_suggestion(
        f,
        chunks[1],
        &state.input,
        state.is_loading,
        state.suggestion.as_deref(),
        state.spinner_frame,
    );

    let active_filter = state.active_agent_filter.clone();
    draw_filter_bar(f, chunks[2], active_filter.as_deref(), &mut state.icons);

    let main_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(chunks[3]);

    let selected_idx = state.table_state.selected();
    let selected_session_clone = selected_idx.and_then(|i| state.results.get(i)).cloned();
    let query = state.current_query.clone();

    draw_results(
        f,
        main_chunks[0],
        &state.results,
        &mut state.table_state,
        &query,
    );

    draw_preview(
        f,
        main_chunks[1],
        selected_session_clone.as_ref(),
        &query,
        state.preview_scroll,
    );

    draw_footer(f, chunks[4], state.modal_open);

    // Modal overlay (drawn last so it's on top).
    if state.modal_open {
        draw_modal(f, area, state.modal_yolo, state.modal_focus);
    }
}

fn draw_title(f: &mut Frame, area: ratatui::layout::Rect, state: &State) {
    let version = env!("CARGO_PKG_VERSION");
    let count = state.results.len();

    let text = if let Some((msg, _)) = &state.notification {
        format!(" fast-resume v{version}   {count} sessions   {msg}")
    } else {
        format!(" fast-resume v{version}   {count} sessions")
    };

    let para = Paragraph::new(Span::styled(
        text,
        Style::default()
            .fg(Color::White)
            .add_modifier(ratatui::style::Modifier::BOLD),
    ));
    f.render_widget(para, area);
}

fn draw_footer(f: &mut Frame, area: ratatui::layout::Rect, modal_open: bool) {
    let hints = if modal_open {
        " Space/y/n: toggle yolo · Tab: switch button · Enter: confirm · Esc: cancel "
    } else {
        " ↑/k prev  ↓/j next  PgUp/PgDn  Enter resume  c copy  Tab autocomplete  q quit "
    };
    let para = Paragraph::new(Span::styled(hints, Style::default().fg(Color::DarkGray)));
    f.render_widget(para, area);
}

/// Draw the search input with an optional autocomplete suggestion shown as dim text.
fn draw_search_input_with_suggestion(
    f: &mut Frame,
    area: ratatui::layout::Rect,
    input: &Input,
    is_loading: bool,
    suggestion: Option<&str>,
    spinner_frame: usize,
) {
    use crate::tui::input_widget::draw_search_input;
    use ratatui::layout::Margin;
    use ratatui::widgets::{Block, Borders};

    // Delegate normal rendering.
    draw_search_input(f, area, input, is_loading, true, spinner_frame);

    // Overlay the suggestion tail as dim text if present.
    if let Some(sug) = suggestion {
        let typed = input.value();
        if sug.len() > typed.len() && sug.starts_with(typed) {
            let tail = &sug[typed.len()..];
            // Compute inner area (same as draw_search_input does).
            let block = Block::default().borders(Borders::ALL);
            let inner = area.inner(Margin {
                horizontal: 1,
                vertical: 1,
            });
            let scroll = input.visual_scroll(inner.width as usize);
            let cursor_x =
                inner.x + (input.visual_cursor().saturating_sub(scroll)) as u16;

            if cursor_x < inner.x + inner.width {
                let sug_area = ratatui::layout::Rect {
                    x: cursor_x,
                    y: inner.y,
                    width: inner.width.saturating_sub(cursor_x - inner.x),
                    height: 1,
                };
                let para = Paragraph::new(Line::from(Span::styled(
                    tail.to_owned(),
                    Style::default().fg(Color::DarkGray),
                )));
                f.render_widget(para, sug_area);
            }

            // Suppress the unused variable warning.
            let _ = block;
        }
    }
}
