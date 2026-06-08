//! Pure data + logic core for the Claude usage widget.
//!
//! No GUI, no platform code. Everything here is unit-testable and produces
//! [`model::UsageSnapshot`] values that the GUI renders as a pure function.
//!
//! Module map (mirrors the design spec §5):
//! - [`model`]      — canonical data types + `Level::from_pct`.
//! - [`timeutil`]   — timestamp normalization (ISO-8601 / Unix-sec / Unix-ms → `SystemTime`, UTC-day).
//! - [`config`]     — widget configuration load/save/merge.
//! - [`sources`]    — quota readings (oauth, statusline) + JSONL token detail + `reconcile`.
//! - [`collector`]  — orchestrates sources into one `UsageSnapshot` per tick.
//! - [`statusline_cmd`] — the `--statusline` helper logic.

pub mod collector;
pub mod config;
pub mod model;
pub mod sources;
pub mod statusline_cmd;
pub mod timeutil;
