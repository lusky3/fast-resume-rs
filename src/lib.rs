// Public items used in integration tests are visible to the compiler as dead code
// when building the library alone. Suppress those warnings — these are intentional
// extension points for tests and future callers.
#![allow(dead_code)]

pub mod adapters;
pub mod config;
pub mod index;
pub mod query;
pub mod search;
pub mod session;
pub mod tui;
pub mod util;
