//! Level → color. The one piece of UI logic worth isolating: it maps a
//! window's severity to the green/amber/red the gauge uses.
//!
//! M2: confirm `gpui::hsla`/`Hsla` are the right constructors for the pinned rev.

use gpui::{Hsla, hsla};
use usage_core::model::Level;

/// Green under warn, amber in the warn band, red at/above critical.
pub fn level_color(level: Level) -> Hsla {
    match level {
        Level::Ok => hsla(140.0 / 360.0, 0.55, 0.45, 1.0),
        Level::Warn => hsla(40.0 / 360.0, 0.90, 0.50, 1.0),
        Level::Critical => hsla(0.0, 0.75, 0.52, 1.0),
    }
}

/// A faint dark scrim drawn behind text so the numbers stay readable over a
/// translucent Mica background (design spec §7.3).
pub fn scrim() -> Hsla {
    hsla(225.0 / 360.0, 0.19, 0.13, 0.80)
}

/// Compact activity-cell palette for the details strip.
pub fn heatmap_color(level: u8) -> Hsla {
    match level {
        0 => hsla(225.0 / 360.0, 0.12, 0.25, 0.55),
        1 => hsla(152.0 / 360.0, 0.35, 0.30, 1.0),
        2 => hsla(150.0 / 360.0, 0.45, 0.38, 1.0),
        3 => hsla(148.0 / 360.0, 0.55, 0.46, 1.0),
        _ => hsla(145.0 / 360.0, 0.65, 0.54, 1.0),
    }
}
