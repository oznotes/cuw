//! Timestamp normalization. The three Claude data sources use three different
//! time encodings, and conflating them is a documented foot-gun:
//!
//! - conversation JSONL `timestamp`   → ISO-8601 (`...Z`)
//! - status-line `rate_limits.*.resets_at` → Unix **seconds**
//! - `history.jsonl`                  → Unix **milliseconds**
//!
//! Everything is normalized to [`std::time::SystemTime`] at the module
//! boundary so the rest of the core stays std-only.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use chrono::DateTime;

/// Build a `SystemTime` from a (possibly negative) Unix timestamp + nanos.
fn from_unix(secs: i64, nanos: u32) -> SystemTime {
    if secs >= 0 {
        UNIX_EPOCH + Duration::new(secs as u64, nanos)
    } else {
        // Pre-epoch: subtract whole seconds, then add the sub-second part.
        UNIX_EPOCH - Duration::new((-secs) as u64, 0) + Duration::new(0, nanos)
    }
}

/// Parse an ISO-8601 / RFC-3339 timestamp (e.g. `2026-06-08T05:15:25.123Z`).
pub fn iso8601_to_systemtime(s: &str) -> Result<SystemTime> {
    let dt = DateTime::parse_from_rfc3339(s)
        .with_context(|| format!("invalid ISO-8601 timestamp: {s}"))?;
    Ok(from_unix(dt.timestamp(), dt.timestamp_subsec_nanos()))
}

/// Convert a Unix **seconds** timestamp to `SystemTime`.
pub fn unix_secs_to_systemtime(secs: i64) -> SystemTime {
    from_unix(secs, 0)
}

/// Convert a Unix **milliseconds** timestamp to `SystemTime`.
pub fn unix_millis_to_systemtime(ms: i64) -> SystemTime {
    from_unix(ms.div_euclid(1000), (ms.rem_euclid(1000) as u32) * 1_000_000)
}

/// Whole days since the Unix epoch in **UTC** — i.e. a UTC-midnight day index.
/// Two timestamps share a calendar day (UTC) iff this returns the same value.
pub fn utc_day(t: SystemTime) -> i64 {
    let secs = match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    };
    secs.div_euclid(86_400)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_epoch_anchors() {
        assert_eq!(iso8601_to_systemtime("1970-01-01T00:00:00Z").unwrap(), UNIX_EPOCH);
        assert_eq!(
            iso8601_to_systemtime("1970-01-02T00:00:00Z").unwrap(),
            UNIX_EPOCH + Duration::from_secs(86_400)
        );
    }

    #[test]
    fn iso_tolerates_fractional_seconds() {
        let t = iso8601_to_systemtime("2026-06-08T05:15:25.500Z").unwrap();
        let t0 = iso8601_to_systemtime("2026-06-08T05:15:25Z").unwrap();
        assert_eq!(t.duration_since(t0).unwrap(), Duration::from_millis(500));
    }

    #[test]
    fn iso_tolerates_explicit_offset() {
        // +00:00 and Z must agree.
        assert_eq!(
            iso8601_to_systemtime("2026-06-08T05:15:25+00:00").unwrap(),
            iso8601_to_systemtime("2026-06-08T05:15:25Z").unwrap()
        );
    }

    #[test]
    fn iso_rejects_garbage() {
        assert!(iso8601_to_systemtime("not-a-time").is_err());
    }

    #[test]
    fn unix_seconds_and_millis() {
        assert_eq!(
            unix_secs_to_systemtime(86_400),
            UNIX_EPOCH + Duration::from_secs(86_400)
        );
        assert_eq!(
            unix_millis_to_systemtime(1_500),
            UNIX_EPOCH + Duration::from_millis(1_500)
        );
    }

    #[test]
    fn utc_day_boundaries() {
        assert_eq!(utc_day(iso8601_to_systemtime("1970-01-01T23:59:59Z").unwrap()), 0);
        assert_eq!(utc_day(iso8601_to_systemtime("1970-01-02T00:00:01Z").unwrap()), 1);
        // same day, different times
        let a = iso8601_to_systemtime("2026-06-08T00:00:01Z").unwrap();
        let b = iso8601_to_systemtime("2026-06-08T23:59:59Z").unwrap();
        assert_eq!(utc_day(a), utc_day(b));
    }
}
