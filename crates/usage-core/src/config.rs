//! Widget configuration: window placement, view mode, frosted-glass backdrop,
//! color thresholds, and refresh cadence.
//!
//! Persisted as JSON at `%APPDATA%/claude-usage/widget-config.json`. Loading is
//! **infallible** — a missing or corrupt file yields defaults — and unknown
//! fields are ignored, so old binaries tolerate new config keys.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ViewMode {
    Bars,
    Gauge,
}

/// The window background material. `Mica`/`MicaAlt` are Win11-only; the widget
/// falls back down this list (`win::backdrop`) when unavailable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Backdrop {
    Mica,
    MicaAlt,
    Acrylic,
    Transparent,
    Opaque,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Saved top-left window position in logical pixels, if the user moved it.
    pub position: Option<(f32, f32)>,
    /// UI scale, clamped to 0.6..=2.0 by the UI.
    pub scale: f32,
    pub view_mode: ViewMode,
    pub backdrop: Backdrop,
    /// Background opacity 0.15..=1.0 (background only; text stays opaque).
    pub opacity: f32,
    /// Lower bound of the amber band.
    pub warn_threshold: f32,
    /// Lower bound of the red band.
    pub critical_threshold: f32,
    /// How often to rebuild the snapshot (seconds).
    pub refresh_secs: u64,
    /// Minimum seconds between OAuth usage polls (endpoint asks for >= 180).
    pub quota_poll_secs: u64,
    /// A status-line cache reading newer than this (seconds) is preferred over OAuth.
    pub statusline_max_age_secs: u64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            position: None,
            scale: 1.0,
            view_mode: ViewMode::Bars,
            backdrop: Backdrop::Mica,
            opacity: 1.0,
            warn_threshold: 70.0,
            critical_threshold: 90.0,
            refresh_secs: 30,
            quota_poll_secs: 180,
            statusline_max_age_secs: 120,
        }
    }
}

impl Config {
    /// `%APPDATA%/claude-usage/widget-config.json` (falls back to the CWD if no
    /// config dir is discoverable, which should never happen on Windows).
    pub fn path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("claude-usage")
            .join("widget-config.json")
    }

    /// Load from the default path. Never fails.
    pub fn load() -> Config {
        Self::load_from(&Self::path())
    }

    /// Load from an explicit path; defaults on missing/unreadable/corrupt file.
    pub fn load_from(path: &Path) -> Config {
        match std::fs::read_to_string(path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
            Err(_) => Config::default(),
        }
    }

    /// Save to the default path.
    pub fn save(&self) -> Result<()> {
        self.save_to(&Self::path())
    }

    /// Save to an explicit path, creating parent dirs, writing atomically
    /// (temp file + rename) so a reader never sees a half-written file.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("creating config dir {}", dir.display()))?;
        }
        let json = serde_json::to_string_pretty(self)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json.as_bytes())
            .with_context(|| format!("writing {}", tmp.display()))?;
        // std::fs::rename replaces an existing destination on Windows
        // (MoveFileExW + MOVEFILE_REPLACE_EXISTING).
        std::fs::rename(&tmp, path).with_context(|| format!("renaming into {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let c = Config::default();
        assert_eq!(c.view_mode, ViewMode::Bars);
        assert_eq!(c.backdrop, Backdrop::Mica);
        assert_eq!(c.warn_threshold, 70.0);
        assert_eq!(c.critical_threshold, 90.0);
        assert_eq!(c.refresh_secs, 30);
        assert_eq!(c.quota_poll_secs, 180);
        assert_eq!(c.statusline_max_age_secs, 120);
        assert_eq!(c.position, None);
    }

    #[test]
    fn json_round_trips() {
        let c = Config {
            position: Some((100.0, 200.0)),
            view_mode: ViewMode::Gauge,
            backdrop: Backdrop::Acrylic,
            ..Config::default()
        };
        let s = serde_json::to_string(&c).unwrap();
        let back: Config = serde_json::from_str(&s).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn partial_json_fills_missing_with_defaults() {
        let c: Config = serde_json::from_str(r#"{"scale": 1.5, "view_mode": "Gauge"}"#).unwrap();
        assert_eq!(c.scale, 1.5);
        assert_eq!(c.view_mode, ViewMode::Gauge);
        // untouched fields keep their defaults
        assert_eq!(c.backdrop, Backdrop::Mica);
        assert_eq!(c.refresh_secs, 30);
    }

    #[test]
    fn empty_object_is_all_defaults() {
        let c: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(c, Config::default());
    }

    #[test]
    fn unknown_fields_are_ignored() {
        // forward compatibility: a newer config key must not break an older binary
        let c: Config =
            serde_json::from_str(r#"{"scale": 1.2, "future_feature": {"x": 1}}"#).unwrap();
        assert_eq!(c.scale, 1.2);
    }

    #[test]
    fn corrupt_file_loads_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("widget-config.json");
        std::fs::write(&p, b"{ this is not json").unwrap();
        assert_eq!(Config::load_from(&p), Config::default());
    }

    #[test]
    fn missing_file_loads_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("does-not-exist.json");
        assert_eq!(Config::load_from(&p), Config::default());
    }

    #[test]
    fn save_then_load_is_identity() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nested").join("widget-config.json");
        let c = Config {
            scale: 1.75,
            view_mode: ViewMode::Gauge,
            backdrop: Backdrop::MicaAlt,
            ..Config::default()
        };
        c.save_to(&p).unwrap();
        assert!(p.exists());
        assert_eq!(Config::load_from(&p), c);
    }
}
