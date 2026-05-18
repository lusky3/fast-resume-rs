use std::sync::{
    Arc,
    mpsc::{self, Receiver},
};
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::{
    DefaultTerminal, Frame,
    crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    text::Span,
    widgets::Paragraph,
};
use tui_input::{Input, InputRequest};

use crate::session::Session;
use crate::search::SessionSearch;
use crate::tui::{
    TuiResult,
    input_widget::draw_search_input,
    preview::{draw_preview, first_match_line},
    results_list::draw_results,
};

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
    fn new(initial_query: &str, search: Arc<SessionSearch>, index_rx: Receiver<IndexMsg>) -> Self {
        let mut table_state = ratatui::widgets::TableState::default();
        table_state.select(Some(0));

        Self {
            input: Input::from(initial_query),
            results: Vec::new(),
            table_state,
            preview_scroll: 0,
            is_loading: true,
            exit_with: None,
            spinner_frame: 0,
            last_spinner_tick: Instant::now(),
            last_search_at: Instant::now(),
            current_query: initial_query.to_owned(),
            index_rx,
            search,
            initial_load_done: false,
        }
    }

    /// Process any pending messages from the background thread and handle debounced search.
    fn tick(&mut self) {
        // Advance spinner every ~80 ms.
        if self.last_spinner_tick.elapsed() >= Duration::from_millis(80) {
            self.spinner_frame = self.spinner_frame.wrapping_add(1);
            self.last_spinner_tick = Instant::now();
        }

        // Drain the channel — process all pending messages.
        while let Ok(msg) = self.index_rx.try_recv() {
            match msg {
                IndexMsg::Done(sessions) => {
                    self.initial_load_done = true;
                    self.is_loading = false;
                    // If query is empty show all sessions, else search.
                    let query = self.input.value().to_owned();
                    if query.is_empty() {
                        self.results = sessions;
                    } else {
                        // Use the already-indexed data — run a search now.
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

        // Debounced search: run if 50 ms have elapsed since the last keystroke and the
        // query has changed.
        if self.initial_load_done
            && !self.is_loading
            && self.last_search_at.elapsed() >= Duration::from_millis(50)
            && self.input.value() != self.current_query
        {
            let query = self.input.value().to_owned();
            let results = if query.is_empty() {
                self.search
                    .get_all_sessions()
                    .unwrap_or_default()
            } else {
                self.search.search(&query, 100).unwrap_or_default()
            };
            self.results = results;
            self.current_query = query.clone();
            self.clamp_selection();
            self.reset_preview_scroll_for_query(&query);
        }
    }

    /// Clamp the selection so it's always within bounds.
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
        // Jump to the first match in the selected session's content.
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
}

/// Entry point for the TUI.
pub fn run_tui(initial_query: &str) -> Result<TuiResult> {
    let search = Arc::new(SessionSearch::new());

    // Spawn background indexing thread.
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
    let result = run_app(&mut terminal, initial_query, search, rx);
    ratatui::restore();

    result
}

fn run_app(
    terminal: &mut DefaultTerminal,
    initial_query: &str,
    search: Arc<SessionSearch>,
    rx: Receiver<IndexMsg>,
) -> Result<TuiResult> {
    let mut state = State::new(initial_query, search, rx);

    loop {
        terminal.draw(|f| draw(f, &mut state))?;

        // Poll with 50 ms timeout — doubles as debounce tick.
        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key)
                    if key.kind == KeyEventKind::Press && handle_key(&mut state, key) =>
                {
                    break;
                }
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

/// Handle a key press. Returns `true` when the event loop should exit.
fn handle_key(state: &mut State, key: KeyEvent) -> bool {
    // Ctrl+C — always quit.
    if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('c') {
        return true;
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

        // Resume
        KeyCode::Enter => {
            if let Some(session) = state.selected_session() {
                // Build a minimal resume command.
                let cmd = build_resume_command(session);
                let dir = session.directory.clone();
                state.exit_with = Some((cmd, dir));
                return true;
            }
        }

        // Tab — reserved for autocomplete (no-op in Phase 3)
        KeyCode::Tab => {}

        // All other printable keys + Backspace go to the search input.
        _ => {
            let prev_value = state.input.value().to_owned();
            if let Some(req) = key_to_input_request(&key) {
                state.input.handle(req);
            }
            if state.input.value() != prev_value {
                state.last_search_at = Instant::now();
                state.is_loading = state.initial_load_done; // show spinner during search
            }
        }
    }

    false
}

/// Build a resume command argv list for a session.
fn build_resume_command(session: &Session) -> Vec<String> {
    match session.agent.as_str() {
        "claude" => vec!["claude".to_owned(), "--resume".to_owned(), session.id.clone()],
        "codex" => vec!["codex".to_owned(), "--session".to_owned(), session.id.clone()],
        "copilot-cli" => vec!["gh".to_owned(), "copilot".to_owned(), "resume".to_owned(), session.id.clone()],
        "vibe" => vec!["vibe".to_owned(), "--session".to_owned(), session.id.clone()],
        "kiro" => vec!["kiro".to_owned(), "--session".to_owned(), session.id.clone()],
        _ => vec![session.agent.clone(), "--session".to_owned(), session.id.clone()],
    }
}

/// Convert a crossterm `KeyEvent` to a `tui_input::InputRequest`, mirroring the
/// `tui_input::backend::crossterm::to_input_request` logic without the crossterm version conflict.
fn key_to_input_request(key: &KeyEvent) -> Option<InputRequest> {
    use InputRequest::*;
    match (key.code, key.modifiers) {
        (KeyCode::Backspace, KeyModifiers::NONE) | (KeyCode::Char('h'), KeyModifiers::CONTROL) => {
            Some(DeletePrevChar)
        }
        (KeyCode::Delete, KeyModifiers::NONE) => Some(DeleteNextChar),
        (KeyCode::Left, KeyModifiers::NONE) | (KeyCode::Char('b'), KeyModifiers::CONTROL) => {
            Some(GoToPrevChar)
        }
        (KeyCode::Left, KeyModifiers::CONTROL) | (KeyCode::Char('b'), KeyModifiers::META) => {
            Some(GoToPrevWord)
        }
        (KeyCode::Right, KeyModifiers::NONE) | (KeyCode::Char('f'), KeyModifiers::CONTROL) => {
            Some(GoToNextChar)
        }
        (KeyCode::Right, KeyModifiers::CONTROL) | (KeyCode::Char('f'), KeyModifiers::META) => {
            Some(GoToNextWord)
        }
        (KeyCode::Char('u'), KeyModifiers::CONTROL) => Some(DeleteLine),
        (KeyCode::Char('w'), KeyModifiers::CONTROL)
        | (KeyCode::Backspace, KeyModifiers::ALT) => Some(DeletePrevWord),
        (KeyCode::Delete, KeyModifiers::CONTROL) => Some(DeleteNextWord),
        (KeyCode::Char('k'), KeyModifiers::CONTROL) => Some(DeleteTillEnd),
        (KeyCode::Char('a'), KeyModifiers::CONTROL) | (KeyCode::Home, KeyModifiers::NONE) => {
            Some(GoToStart)
        }
        (KeyCode::Char('e'), KeyModifiers::CONTROL) | (KeyCode::End, KeyModifiers::NONE) => {
            Some(GoToEnd)
        }
        (KeyCode::Char(c), KeyModifiers::NONE) => Some(InsertChar(c)),
        (KeyCode::Char(c), KeyModifiers::SHIFT) => Some(InsertChar(c)),
        _ => None,
    }
}

/// Top-level draw function.
pub fn draw(f: &mut Frame, state: &mut State) {
    let area = f.area();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title bar
            Constraint::Length(3), // search box (with border)
            Constraint::Min(0),    // main area
            Constraint::Length(1), // footer hints
        ])
        .split(area);

    // Title bar
    draw_title(f, chunks[0], state);

    // Search input
    draw_search_input(
        f,
        chunks[1],
        &state.input,
        state.is_loading,
        true, // always active in Phase 3
        state.spinner_frame,
    );

    // Main area: 60% results, 40% preview
    let main_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(chunks[2]);

    // Avoid borrow conflict: extract what we need before mutable borrow of table_state.
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

    // Footer
    draw_footer(f, chunks[3]);
}

fn draw_title(f: &mut Frame, area: ratatui::layout::Rect, state: &State) {
    let version = env!("CARGO_PKG_VERSION");
    let count = state.results.len();
    let text = format!(" fast-resume v{version}   {count} sessions");
    let para = Paragraph::new(Span::styled(
        text,
        Style::default().fg(Color::White).add_modifier(ratatui::style::Modifier::BOLD),
    ));
    f.render_widget(para, area);
}

fn draw_footer(f: &mut Frame, area: ratatui::layout::Rect) {
    let hints = " ↑/k prev  ↓/j next  PgUp/PgDn  Enter resume  q quit ";
    let para = Paragraph::new(Span::styled(hints, Style::default().fg(Color::DarkGray)));
    f.render_widget(para, area);
}
