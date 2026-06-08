//! Canonical data types — the contract between the collector and the UI.
//!
//! These are deliberately std-only (no chrono, no gpui) so both the logic
//! core and any UI can depend on them without pulling extra weight.

use std::time::SystemTime;

/// One rate-limit window's state: how much is used and when it resets.
///
/// `used_pct` is **Anthropic's own utilization number** (0.0..=100.0), so the
/// gauge never needs a guessed denominator.
#[derive(Clone, Debug, PartialEq)]
pub struct Window {
    pub used_pct: f32,
    pub resets_at: Option<SystemTime>,
}

/// Severity bucket derived from a window's `used_pct`. Drives the UI color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Ok,
    Warn,
    Critical,
}

impl Level {
    /// Map a percentage to a level. `warn`/`critical` are the *lower bounds*
    /// of each band (e.g. `70.0`, `90.0`), so `pct == warn` is already `Warn`
    /// and `pct == critical` is already `Critical`.
    pub fn from_pct(pct: f32, warn: f32, critical: f32) -> Level {
        if pct >= critical {
            Level::Critical
        } else if pct >= warn {
            Level::Warn
        } else {
            Level::Ok
        }
    }
}

/// Where the quota figures in a snapshot came from.
#[derive(Clone, Debug, PartialEq)]
pub enum Provenance {
    /// Fresh value from the local status-line cache (no network).
    StatusLine,
    /// Fetched from the OAuth usage endpoint.
    OAuth,
    /// Both live sources failed; showing the last-known-good reading taken at this time.
    Stale { last_good_at: SystemTime },
}

/// Live quota source that produced the last usable reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveSource {
    StatusLine,
    OAuth,
}

/// Best-effort health detail for the current snapshot. The widget can stay
/// quiet by default while still exposing enough context for troubleshooting.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Diagnostics {
    /// Age of the readable status-line cache, if one exists.
    pub statusline_age_secs: Option<u64>,
    /// Source of the displayed quota reading, including stale fallbacks.
    pub last_quota_success: Option<LiveSource>,
    /// Last JSONL ingestion problem observed during this tick.
    pub jsonl_error: Option<String>,
    /// Last OAuth polling problem observed during this tick.
    pub oauth_error: Option<String>,
}

/// Local token detail derived from the JSONL transcripts. Informational —
/// the headline is the quota windows, not these counts.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TokenStats {
    /// Today's (UTC) total output tokens — the headline figure (output-weighted,
    /// matching the ccusage/bozdemir convention).
    pub today_total_output: u64,
    /// Per-model output tokens today, sorted descending.
    pub by_model: Vec<(String, u64)>,
    /// Output tokens/minute over the last ~90s, if there is recent activity.
    pub live_tok_per_min: Option<f64>,
    /// Top projects by output tokens today, sorted descending (popup-only).
    pub top_projects: Vec<(String, u64)>,
    /// Compact activity grid, oldest week to newest week, Sunday..Saturday.
    pub activity_heatmap: Vec<[u8; 7]>,
}

/// One immutable refresh of everything the widget shows.
#[derive(Clone, Debug)]
pub struct UsageSnapshot {
    /// 5-hour rolling window — the primary gauge.
    pub five_hour: Window,
    /// 7-day window — the secondary gauge.
    pub seven_day: Window,
    /// Opus-specific 7-day window, when the API reports it separately.
    pub seven_day_opus: Option<Window>,
    pub tokens: TokenStats,
    pub source: Provenance,
    pub fetched_at: SystemTime,
    pub diagnostics: Diagnostics,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_bands_are_inclusive_lower_bounds() {
        let (warn, crit) = (70.0, 90.0);
        assert_eq!(Level::from_pct(0.0, warn, crit), Level::Ok);
        assert_eq!(Level::from_pct(69.9, warn, crit), Level::Ok);
        assert_eq!(Level::from_pct(70.0, warn, crit), Level::Warn); // boundary => Warn
        assert_eq!(Level::from_pct(89.9, warn, crit), Level::Warn);
        assert_eq!(Level::from_pct(90.0, warn, crit), Level::Critical); // boundary => Critical
        assert_eq!(Level::from_pct(100.0, warn, crit), Level::Critical);
    }

    #[test]
    fn level_respects_custom_thresholds() {
        assert_eq!(Level::from_pct(50.0, 50.0, 80.0), Level::Warn);
        assert_eq!(Level::from_pct(79.0, 50.0, 80.0), Level::Warn);
        assert_eq!(Level::from_pct(80.0, 50.0, 80.0), Level::Critical);
    }
}
