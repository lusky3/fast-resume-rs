mod adapters;
mod config;
mod index;
mod search;
mod session;
mod tui;

use std::time::Instant;

use clap::{Parser, Subcommand};

use search::SessionSearch;
use tui::{app::TuiOpts, run_tui};

/// fast-resume: search and resume AI coding agent sessions.
#[derive(Parser)]
#[command(name = "fr", version)]
struct Cli {
    /// Pre-fill the search box with this query.
    #[arg(value_name = "QUERY")]
    query: Option<String>,

    /// Force Unicode half-block rendering instead of Sixel/Kitty inline images.
    ///
    /// Use this flag on terminals that don't support inline graphics, in CI, or
    /// when capturing screenshots where deterministic output is required.
    #[arg(long)]
    no_images: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Scan all agent directories, index sessions, and report statistics.
    Index,
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Index) => cmd_index(),
        None => cmd_tui(cli.query.as_deref().unwrap_or(""), cli.no_images),
    }
}

/// `fr index` — scan + index all sessions and print stats.
fn cmd_index() {
    let started = Instant::now();
    let search = SessionSearch::new();

    println!("Scanning sessions…");
    let sessions = match search.get_all_sessions() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error indexing sessions: {}", e);
            std::process::exit(1);
        }
    };

    let elapsed = started.elapsed();

    let mut per_adapter: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for s in &sessions {
        *per_adapter.entry(s.agent.clone()).or_insert(0) += 1;
    }

    let adapter_count = search.adapter_count();
    println!(
        "Indexed {} sessions across {} adapters ({:.2}s)",
        sessions.len(),
        adapter_count,
        elapsed.as_secs_f64()
    );

    let mut agents: Vec<(&String, &usize)> = per_adapter.iter().collect();
    agents.sort_by_key(|(name, _)| name.as_str());
    for (agent, count) in agents {
        println!("  {:<16} {}", agent, count);
    }
}

/// Default command — launch the TUI.
fn cmd_tui(initial_query: &str, no_images: bool) {
    let opts = TuiOpts {
        initial_query,
        no_images,
    };
    match run_tui(opts) {
        Ok(result) => {
            if let (Some(cmd), Some(_dir)) = (result.resume_command, result.resume_dir) {
                println!("Launching: {}", cmd.join(" "));
                // Phase 7: replace process via execvp. For now, just print.
            }
        }
        Err(e) => {
            eprintln!("TUI error: {e}");
            std::process::exit(1);
        }
    }
}
