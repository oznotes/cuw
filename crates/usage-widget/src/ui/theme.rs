// claude-usage - a Claude usage widget for Windows.
// Copyright (c) 2026 Ozgur Oz. MIT License (see LICENSE).
//
//! Level → color. The one piece of UI logic worth isolating: it maps a
//! window's severity to the green/amber/red the gauge uses.

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

/// The dark scrim drawn over the transparent window at the user's chosen
/// opacity. `0.0` = fully see-through (the desktop shows through), `1.0` = a
/// solid dark card. (Config default is `0.4` — a readable scrim over the glass.)
pub fn panel_bg(opacity: f32) -> Hsla {
    hsla(225.0 / 360.0, 0.19, 0.13, opacity.clamp(0.0, 1.0))
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
