pub mod app;
pub mod filter_bar;
pub mod icons;
pub mod input_widget;
pub mod modal;
pub mod preview;
pub mod results_list;
pub mod style;

#[allow(unused_imports)]
pub use app::{compute_suggestion, run_tui};

/// The result returned by the TUI after the user makes a selection or quits.
pub struct TuiResult {
    pub resume_command: Option<Vec<String>>,
    pub resume_dir: Option<String>,
}
