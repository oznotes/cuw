//! The OAuth usage endpoint: `GET https://api.anthropic.com/api/oauth/usage`.
//!
//! Returns the real 5h / 7d utilization. The parsing is pure and tested; the
//! actual HTTP call is behind the `net` feature so the core builds/tests with
//! no TLS dependency.

use std::time::SystemTime;

use anyhow::{Context, Result};
use serde::Deserialize;

use super::QuotaReading;
use crate::model::{Provenance, Window};
use crate::timeutil::iso8601_to_systemtime;

#[cfg(feature = "net")]
const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
#[cfg(feature = "net")]
const OAUTH_BETA: &str = "oauth-2025-04-20";

#[derive(Deserialize)]
struct WindowJson {
    utilization: f32,
    #[serde(default)]
    resets_at: Option<String>,
}

#[derive(Deserialize)]
struct UsageJson {
    five_hour: WindowJson,
    seven_day: WindowJson,
    #[serde(default)]
    seven_day_opus: Option<WindowJson>,
}

fn to_window(w: &WindowJson) -> Result<Window> {
    let resets_at = match &w.resets_at {
        Some(s) => Some(iso8601_to_systemtime(s)?),
        None => None,
    };
    Ok(Window {
        used_pct: w.utilization,
        resets_at,
    })
}

/// Parse a usage-endpoint response body into a [`QuotaReading`]. Pure.
/// Unknown fields (`seven_day_sonnet`, `extra_usage`, …) are ignored.
pub fn parse_usage_json(body: &str, observed_at: SystemTime) -> Result<QuotaReading> {
    let u: UsageJson = serde_json::from_str(body).context("parsing oauth usage JSON")?;
    Ok(QuotaReading {
        five_hour: to_window(&u.five_hour)?,
        seven_day: to_window(&u.seven_day)?,
        seven_day_opus: match &u.seven_day_opus {
            Some(w) => Some(to_window(w)?),
            None => None,
        },
        source: Provenance::OAuth,
        observed_at,
    })
}

/// Fetch the live usage. Sends the three mandatory headers — omitting the
/// `User-Agent` yields persistent HTTP 429.
#[cfg(feature = "net")]
pub fn fetch(token: &str, cc_version: &str) -> Result<QuotaReading> {
    let ua = format!("claude-code/{cc_version}");
    let resp = ureq::get(USAGE_URL)
        .set("Authorization", &format!("Bearer {token}"))
        .set("anthropic-beta", OAUTH_BETA)
        .set("User-Agent", &ua)
        .call()
        .context("GET /api/oauth/usage")?;
    let body = resp.into_string().context("reading usage response body")?;
    parse_usage_json(&body, SystemTime::now())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "five_hour": {"utilization": 72.5, "resets_at": "2026-06-08T10:00:00Z"},
      "seven_day": {"utilization": 41.0, "resets_at": "2026-06-12T00:00:00Z"},
      "seven_day_opus": null,
      "seven_day_sonnet": {"utilization": 5.0, "resets_at": "2026-06-12T00:00:00Z"},
      "extra_usage": {"is_enabled": false, "monthly_limit": 0, "used_credits": 0}
    }"#;

    #[test]
    fn parses_windows_and_ignores_unknown_fields() {
        let r = parse_usage_json(SAMPLE, SystemTime::UNIX_EPOCH).unwrap();
        assert_eq!(r.five_hour.used_pct, 72.5);
        assert!(r.five_hour.resets_at.is_some());
        assert_eq!(r.seven_day.used_pct, 41.0);
        assert!(r.seven_day_opus.is_none());
        assert_eq!(r.source, Provenance::OAuth);
        assert_eq!(r.observed_at, SystemTime::UNIX_EPOCH);
    }

    #[test]
    fn parses_opus_window_when_present_and_tolerates_missing_resets() {
        let body = r#"{"five_hour":{"utilization":1.0},
                       "seven_day":{"utilization":2.0},
                       "seven_day_opus":{"utilization":3.0,"resets_at":"2026-06-12T00:00:00Z"}}"#;
        let r = parse_usage_json(body, SystemTime::UNIX_EPOCH).unwrap();
        assert_eq!(r.seven_day_opus.unwrap().used_pct, 3.0);
        assert!(r.five_hour.resets_at.is_none()); // missing resets_at => None, not an error
    }

    #[test]
    fn rejects_malformed_or_incomplete() {
        assert!(parse_usage_json("{not json", SystemTime::UNIX_EPOCH).is_err());
        // five_hour is required
        assert!(
            parse_usage_json(
                r#"{"seven_day":{"utilization":2.0}}"#,
                SystemTime::UNIX_EPOCH
            )
            .is_err()
        );
    }
}
