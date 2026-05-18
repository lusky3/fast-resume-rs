//! Agent icon cache backed by `ratatui-image`.
//!
//! Icons live in the `assets/` directory adjacent to the binary (at runtime) or
//! at `$CARGO_MANIFEST_DIR/assets/` during development builds.
//!
//! **Rendering restrictions (from migration plan §6 and risk register §1):**
//! Never render `StatefulImage` inside a scrolling preview pane or inside a
//! `Table` cell — only in fixed-position areas (filter bar buttons).  The
//! results table uses a styled text badge instead to avoid protocol artifacts.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use ratatui_image::{StatefulImage, picker::Picker, protocol::StatefulProtocol};

pub struct IconCache {
    picker: Picker,
    /// `None` sentinel means "load was attempted and failed" — skip future attempts.
    protocols: HashMap<String, Option<StatefulProtocol>>,
}

impl IconCache {
    /// Attempt to auto-detect the terminal's graphics capabilities via stdio
    /// escape-sequence probing.  Falls back to Unicode half-blocks if detection
    /// fails or the terminal doesn't respond.
    pub fn new() -> Result<Self> {
        let picker = Picker::from_query_stdio()
            .unwrap_or_else(|_| Picker::halfblocks());
        Ok(Self {
            picker,
            protocols: HashMap::new(),
        })
    }

    /// Construct an `IconCache` that is guaranteed to use Unicode half-block
    /// rendering.  This is the deterministic fallback used with `--no-images`
    /// and in test snapshots so that no Sixel/Kitty byte sequences appear in
    /// the output.
    pub fn halfblocks() -> Self {
        Self {
            picker: Picker::halfblocks(),
            protocols: HashMap::new(),
        }
    }

    /// Return a mutable reference to the `StatefulProtocol` for `agent`, or
    /// `None` when:
    /// - the icon PNG doesn't exist on disk, or
    /// - the image failed to decode.
    ///
    /// The result is memoised: failed loads are stored as `None` to avoid
    /// repeated file-system access on every frame.
    pub fn get(&mut self, agent: &str) -> Option<&mut StatefulProtocol> {
        if !self.protocols.contains_key(agent) {
            let entry = self.load_icon(agent);
            self.protocols.insert(agent.to_string(), entry);
        }
        self.protocols.get_mut(agent)?.as_mut()
    }

    /// Internal: try to load `<agent>.png` from the assets directory and
    /// create a `StatefulProtocol` from it.  Returns `None` on any error so
    /// the caller can cache the failure.
    fn load_icon(&self, agent: &str) -> Option<StatefulProtocol> {
        let path = assets_dir()?.join(format!("{agent}.png"));
        if !path.exists() {
            return None;
        }
        let img = image::ImageReader::open(&path).ok()?.decode().ok()?;
        Some(self.picker.new_resize_protocol(img))
    }

    /// Render the icon for `agent` into `area` on the current frame, if an
    /// icon is available.
    ///
    /// `area` should be a small, **fixed-position** rect (e.g. inside a filter
    /// bar button cell).  Never pass a rect inside a scrolling pane — see the
    /// module-level doc for the rationale.
    pub fn render_icon(
        &mut self,
        f: &mut ratatui::Frame,
        agent: &str,
        area: ratatui::layout::Rect,
    ) {
        if let Some(proto) = self.get(agent) {
            f.render_stateful_widget(StatefulImage::new(), area, proto);
        }
    }
}

impl Default for IconCache {
    fn default() -> Self {
        Self::halfblocks()
    }
}

/// Locate the `assets/` directory.
///
/// Strategy (in order):
/// 1. Sibling of `std::env::current_exe()` — works for installed binaries.
/// 2. `$CARGO_MANIFEST_DIR/assets` — works during `cargo run` / `cargo test`.
///
/// Returns `None` when neither path can be determined.
fn assets_dir() -> Option<PathBuf> {
    // Development: prefer CARGO_MANIFEST_DIR so tests don't need a real binary.
    if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
        let p = Path::new(&manifest).join("assets");
        if p.is_dir() {
            return Some(p);
        }
    }

    // Runtime: look next to the exe.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let p = dir.join("assets");
            if p.is_dir() {
                return Some(p);
            }
        }
    }

    None
}
