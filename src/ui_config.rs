//! Read the `ui:` section of `~/.config/tad/config.yaml`.
//!
//! Mirrors the pattern in `theme.rs`: the unified config file is owned
//! by `config.rs` for the `groups:` section, but theme and ui prefs
//! read their keys directly via serde_yml so they can have their own
//! shapes without entangling config::Doc.

use serde::Deserialize;
use std::time::Duration;

/// User-tunable UI prefs. All fields have sensible defaults so a config
/// file with no `ui:` section behaves identically to one with everything
/// at default.
#[derive(Debug, Clone)]
pub struct UiConfig {
    /// When true (default), `tad watch` pops the dashboard with the agent
    /// preselected on an Active→Idle transition. Set false to fully
    /// silence the watcher.
    pub auto_popup: bool,
    /// Idle threshold for the watcher's Active→Idle classification.
    /// Defaults to 30s — matches the `tad status` default so what shows
    /// up as "idle" in the status line is also what triggers the popup.
    pub auto_popup_idle: Duration,
    /// `tmux display-popup -w` value for the auto-popup. Defaults to 80%.
    pub auto_popup_width: String,
    /// `tmux display-popup -h` value for the auto-popup. Defaults to 80%.
    pub auto_popup_height: String,
    /// Per-agent cooldown after a popup fires, so we don't re-pop the
    /// same idle agent every tick. Defaults to 5 minutes — enough that
    /// the user has time to respond, short enough that a truly stuck
    /// agent eventually re-surfaces if they ignored it.
    pub auto_popup_cooldown: Duration,
    /// Snooze durations offered in the dashboard's `s` modal. Default
    /// 5m / 30m / 2h. Snoozes are honored by `tad watch` and persist
    /// across watcher restarts (stored in $XDG_STATE_HOME/tad/snooze.yaml).
    pub snooze_intervals: Vec<Duration>,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            auto_popup: true,
            auto_popup_idle: Duration::from_secs(30),
            auto_popup_width: "80%".into(),
            auto_popup_height: "80%".into(),
            auto_popup_cooldown: Duration::from_secs(5 * 60),
            snooze_intervals: vec![
                Duration::from_secs(5 * 60),
                Duration::from_secs(30 * 60),
                Duration::from_secs(2 * 3600),
            ],
        }
    }
}

/// Wire-format struct; mapped onto [`UiConfig`] with defaults filled in.
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct UiWire {
    auto_popup: Option<bool>,
    auto_popup_idle_secs: Option<u64>,
    auto_popup_width: Option<String>,
    auto_popup_height: Option<String>,
    auto_popup_cooldown_secs: Option<u64>,
    snooze_intervals_secs: Option<Vec<u64>>,
}

/// The top-level fragment we deserialize: just the `ui:` key. Other keys
/// in the same file (theme, groups, _meta) are ignored.
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct Wire {
    ui: UiWire,
}

pub fn load() -> UiConfig {
    let path = match dirs::config_dir() {
        Some(p) => p.join("tad").join("config.yaml"),
        None => return UiConfig::default(),
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return UiConfig::default(),
    };
    let wire: Wire = serde_yml::from_str(&text).unwrap_or_default();
    let defaults = UiConfig::default();
    UiConfig {
        auto_popup: wire.ui.auto_popup.unwrap_or(defaults.auto_popup),
        auto_popup_idle: wire
            .ui
            .auto_popup_idle_secs
            .map(Duration::from_secs)
            .unwrap_or(defaults.auto_popup_idle),
        auto_popup_width: wire
            .ui
            .auto_popup_width
            .unwrap_or(defaults.auto_popup_width),
        auto_popup_height: wire
            .ui
            .auto_popup_height
            .unwrap_or(defaults.auto_popup_height),
        auto_popup_cooldown: wire
            .ui
            .auto_popup_cooldown_secs
            .map(Duration::from_secs)
            .unwrap_or(defaults.auto_popup_cooldown),
        snooze_intervals: wire
            .ui
            .snooze_intervals_secs
            .map(|v| v.into_iter().map(Duration::from_secs).collect())
            .filter(|v: &Vec<Duration>| !v.is_empty())
            .unwrap_or(defaults.snooze_intervals),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Map a Wire onto a UiConfig the same way load() does, sharing one
    /// place to keep the field-by-field merge in sync between prod code
    /// and tests.
    fn merge(wire: Wire) -> UiConfig {
        let defaults = UiConfig::default();
        UiConfig {
            auto_popup: wire.ui.auto_popup.unwrap_or(defaults.auto_popup),
            auto_popup_idle: wire
                .ui
                .auto_popup_idle_secs
                .map(Duration::from_secs)
                .unwrap_or(defaults.auto_popup_idle),
            auto_popup_width: wire
                .ui
                .auto_popup_width
                .unwrap_or(defaults.auto_popup_width),
            auto_popup_height: wire
                .ui
                .auto_popup_height
                .unwrap_or(defaults.auto_popup_height),
            auto_popup_cooldown: wire
                .ui
                .auto_popup_cooldown_secs
                .map(Duration::from_secs)
                .unwrap_or(defaults.auto_popup_cooldown),
            snooze_intervals: wire
                .ui
                .snooze_intervals_secs
                .map(|v| v.into_iter().map(Duration::from_secs).collect())
                .filter(|v: &Vec<Duration>| !v.is_empty())
                .unwrap_or(defaults.snooze_intervals),
        }
    }

    #[test]
    fn defaults_are_sensible() {
        let d = UiConfig::default();
        assert!(d.auto_popup);
        assert_eq!(d.auto_popup_idle, Duration::from_secs(30));
        assert_eq!(d.auto_popup_width, "80%");
        assert_eq!(d.auto_popup_height, "80%");
        assert_eq!(d.auto_popup_cooldown, Duration::from_secs(300));
        assert_eq!(
            d.snooze_intervals,
            vec![
                Duration::from_secs(300),
                Duration::from_secs(1800),
                Duration::from_secs(7200),
            ]
        );
    }

    #[test]
    fn parses_full_ui_section() {
        let yaml = "\
theme: dracula
ui:
  auto_popup: false
  auto_popup_idle_secs: 10
  auto_popup_width: 60%
  auto_popup_height: 70%
  auto_popup_cooldown_secs: 120
  snooze_intervals_secs: [60, 600, 3600]
groups:
  foo:
    hosts: [a]
";
        let wire: Wire = serde_yml::from_str(yaml).unwrap();
        let cfg = merge(wire);
        assert!(!cfg.auto_popup);
        assert_eq!(cfg.auto_popup_idle, Duration::from_secs(10));
        assert_eq!(cfg.auto_popup_width, "60%");
        assert_eq!(cfg.auto_popup_height, "70%");
        assert_eq!(cfg.auto_popup_cooldown, Duration::from_secs(120));
        assert_eq!(
            cfg.snooze_intervals,
            vec![
                Duration::from_secs(60),
                Duration::from_secs(600),
                Duration::from_secs(3600),
            ]
        );
    }

    #[test]
    fn empty_snooze_list_falls_back_to_defaults() {
        // A user who writes `snooze_intervals_secs: []` shouldn't end up
        // with an empty list (no snooze options in the picker == broken
        // UX); fall back to the default set.
        let yaml = "ui:\n  snooze_intervals_secs: []\n";
        let wire: Wire = serde_yml::from_str(yaml).unwrap();
        let cfg = merge(wire);
        assert_eq!(cfg.snooze_intervals.len(), 3);
    }

    #[test]
    fn missing_ui_section_yields_defaults() {
        let yaml = "theme: dracula\ngroups: {}\n";
        let wire: Wire = serde_yml::from_str(yaml).unwrap();
        assert!(wire.ui.auto_popup.is_none());
        assert!(wire.ui.auto_popup_idle_secs.is_none());
    }

    #[test]
    fn unknown_ui_keys_are_ignored() {
        // Future-proofing: a config that includes a key tad doesn't know
        // about yet must not fail to parse.
        let yaml = "ui:\n  auto_popup: false\n  some_future_key: 42\n";
        let wire: Wire = serde_yml::from_str(yaml).unwrap();
        assert_eq!(wire.ui.auto_popup, Some(false));
    }
}
