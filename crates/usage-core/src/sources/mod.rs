// claude-usage - a Claude usage widget for Windows.
// Copyright (c) 2026 Ozgur Oz. MIT License (see LICENSE).
//
//! Quota sources and their reconciliation.
//!
//! Two live sources provide the headline utilization numbers:
//! - [`statusline`] — a local cache file fed by Claude Code's status line (no network).
//! - [`oauth`] — the OAuth usage endpoint (network; behind the `net` feature).
//!
//! [`jsonl`] provides the secondary local token detail.
//!
//! [`reconcile`] decides which reading to actually show each tick.

pub mod jsonl;
pub mod oauth;
pub mod statusline;

use std::time::{Duration, SystemTime};

use crate::model::{Provenance, Window};

/// A single quota observation from one live source (before it becomes part of a
/// [`crate::model::UsageSnapshot`]). `source` is always `StatusLine` or `OAuth`
/// here — never `Stale`.
#[derive(Clone, Debug, PartialEq)]
pub struct QuotaReading {
    pub five_hour: Window,
    pub seven_day: Window,
    pub seven_day_opus: Option<Window>,
    pub source: Provenance,
    pub observed_at: SystemTime,
}

/// Decide which quota reading to display this tick, and under what provenance.
///
/// Preference order:
/// 1. A **fresh** status-line reading (age <= `statusline_max_age`) — free + live.
/// 2. The OAuth reading.
/// 3. A **stale** status-line reading (shown as `Stale`).
/// 4. The previous good reading `last_good` (shown as `Stale`).
/// 5. Nothing.
///
/// Returns the reading to render plus the *effective* provenance (which may be
/// `Stale` even though `QuotaReading::source` records the original source).
pub fn reconcile(
    statusline: Option<QuotaReading>,
    oauth: Option<QuotaReading>,
    last_good: Option<QuotaReading>,
    now: SystemTime,
    statusline_max_age: Duration,
) -> Option<(QuotaReading, Provenance)> {
    let is_fresh = |r: &QuotaReading| {
        // Clock skew (observed_at in the future) counts as fresh.
        now.duration_since(r.observed_at).unwrap_or(Duration::ZERO) <= statusline_max_age
    };

    if let Some(sl) = &statusline {
        if is_fresh(sl) {
            return Some((sl.clone(), Provenance::StatusLine));
        }
    }
    if let Some(o) = oauth {
        return Some((o, Provenance::OAuth));
    }
    // Degrade: show the freshest stale thing we have.
    let stale = match (statusline, last_good) {
        (Some(sl), Some(lg)) => Some(if sl.observed_at >= lg.observed_at {
            sl
        } else {
            lg
        }),
        (Some(sl), None) => Some(sl),
        (None, Some(lg)) => Some(lg),
        (None, None) => None,
    };
    stale.map(|r| {
        let at = r.observed_at;
        (r, Provenance::Stale { last_good_at: at })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(pct: f32, age_secs: u64, now: SystemTime, source: Provenance) -> QuotaReading {
        QuotaReading {
            five_hour: Window {
                used_pct: pct,
                resets_at: None,
            },
            seven_day: Window {
                used_pct: pct / 2.0,
                resets_at: None,
            },
            seven_day_opus: None,
            source,
            observed_at: now - Duration::from_secs(age_secs),
        }
    }

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(2_000_000_000)
    }

    const MAX_AGE: Duration = Duration::from_secs(120);

    #[test]
    fn fresh_statusline_wins_over_oauth() {
        let n = now();
        let sl = reading(50.0, 10, n, Provenance::StatusLine);
        let oa = reading(60.0, 0, n, Provenance::OAuth);
        let (r, prov) = reconcile(Some(sl), Some(oa), None, n, MAX_AGE).unwrap();
        assert_eq!(prov, Provenance::StatusLine);
        assert_eq!(r.five_hour.used_pct, 50.0);
    }

    #[test]
    fn stale_statusline_yields_to_oauth() {
        let n = now();
        let sl = reading(50.0, 300, n, Provenance::StatusLine); // 5 min old > 120s
        let oa = reading(60.0, 0, n, Provenance::OAuth);
        let (r, prov) = reconcile(Some(sl), Some(oa), None, n, MAX_AGE).unwrap();
        assert_eq!(prov, Provenance::OAuth);
        assert_eq!(r.five_hour.used_pct, 60.0);
    }

    #[test]
    fn oauth_used_when_no_statusline() {
        let n = now();
        let oa = reading(60.0, 0, n, Provenance::OAuth);
        let (_, prov) = reconcile(None, Some(oa), None, n, MAX_AGE).unwrap();
        assert_eq!(prov, Provenance::OAuth);
    }

    #[test]
    fn stale_statusline_shown_when_nothing_else() {
        let n = now();
        let sl = reading(50.0, 300, n, Provenance::StatusLine);
        let observed = sl.observed_at;
        let (r, prov) = reconcile(Some(sl), None, None, n, MAX_AGE).unwrap();
        assert_eq!(r.five_hour.used_pct, 50.0);
        assert_eq!(
            prov,
            Provenance::Stale {
                last_good_at: observed
            }
        );
    }

    #[test]
    fn falls_back_to_last_good_as_stale() {
        let n = now();
        let lg = reading(42.0, 600, n, Provenance::OAuth);
        let observed = lg.observed_at;
        let (r, prov) = reconcile(None, None, Some(lg), n, MAX_AGE).unwrap();
        assert_eq!(r.five_hour.used_pct, 42.0);
        assert_eq!(
            prov,
            Provenance::Stale {
                last_good_at: observed
            }
        );
    }

    #[test]
    fn picks_more_recent_of_stale_statusline_and_last_good() {
        let n = now();
        let sl = reading(50.0, 300, n, Provenance::StatusLine); // newer
        let lg = reading(42.0, 600, n, Provenance::OAuth); // older
        let (r, _) = reconcile(Some(sl), None, Some(lg), n, MAX_AGE).unwrap();
        assert_eq!(r.five_hour.used_pct, 50.0);
    }

    #[test]
    fn nothing_available_is_none() {
        assert!(reconcile(None, None, None, now(), MAX_AGE).is_none());
    }

    #[test]
    fn future_timestamp_counts_as_fresh() {
        let n = now();
        let mut sl = reading(50.0, 0, n, Provenance::StatusLine);
        sl.observed_at = n + Duration::from_secs(30); // clock skew into the future
        let (_, prov) = reconcile(Some(sl), None, None, n, MAX_AGE).unwrap();
        assert_eq!(prov, Provenance::StatusLine);
    }
}
