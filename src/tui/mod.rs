pub mod app;
pub mod input_widget;
pub mod preview;
pub mod results_list;
pub mod style;

pub use app::run_tui;

/// The result returned by the TUI after the user makes a selection or quits.
pub struct TuiResult {
    pub resume_command: Option<Vec<String>>,
    pub resume_dir: Option<String>,
}
