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
    /// Idle threshold for the watcher's attention classification when
    /// the precise transcript signal isn't available — agents whose
    /// transcript mtime is older than this fall into the mtime
    /// fallback "Idle" bucket and light up the `@tad-attn` marker.
    /// Defaults to 30s.
    pub attention_idle: Duration,
    /// Snooze durations offered in the dashboard's `s` modal. Default
    /// 5m / 30m / 2h. Snoozes are honored by `tad watch` (they
    /// suppress the `@tad-attn` marker) and persist across watcher
    /// restarts (stored in $XDG_STATE_HOME/tad/snooze.yaml).
    pub snooze_intervals: Vec<Duration>,
    /// How recent the transcript mtime must be for an AwaitingInput
    /// agent to count toward the "N waiting" tail in `tad status`.
    /// Default 10 minutes — long enough to span a brief
    /// coffee-break, short enough that day-old sessions the user has
    /// clearly walked away from don't keep the status bar alarming.
    /// The dashboard's Agents view still surfaces stale AwaitingInput
    /// rows (with their age) so abandoned work isn't invisible.
    pub awaiting_freshness: Duration,
    /// Fire a desktop notification (`notify::send_blocked`) when an
    /// agent transitions into `Blocked` while the dashboard is open.
    /// Default true; set `notify_on_blocked: false` under `ui:` to
    /// quiet it.
    pub notify_on_blocked: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            attention_idle: Duration::from_secs(30),
            snooze_intervals: vec![
                Duration::from_secs(5 * 60),
                Duration::from_secs(30 * 60),
                Duration::from_secs(2 * 3600),
            ],
            awaiting_freshness: Duration::from_secs(10 * 60),
            notify_on_blocked: true,
        }
    }
}

/// Wire-format struct; mapped onto [`UiConfig`] with defaults filled
/// in. Pre-v0.11 keys (`auto_popup`, `auto_popup_idle_secs`, …) are
/// still accepted so an existing config doesn't fail to parse; their
/// values are mapped where they have an analogue (idle_secs →
/// attention_idle) and otherwise ignored. See
/// [`deprecation_warning_for`] for the user-facing notice.
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct UiWire {
    attention_idle_secs: Option<u64>,
    snooze_intervals_secs: Option<Vec<u64>>,
    awaiting_freshness_secs: Option<u64>,
    notify_on_blocked: Option<bool>,

    // Legacy keys (pre-v0.11). Kept so existing configs deserialize
    // cleanly. `auto_popup_idle_secs` is mapped onto the new
    // `attention_idle` field; the rest are silently ignored.
    auto_popup: Option<bool>,
    auto_popup_idle_secs: Option<u64>,
    auto_popup_width: Option<String>,
    auto_popup_height: Option<String>,
    auto_popup_cooldown_secs: Option<u64>,
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
    merge(wire)
}

/// If any legacy `auto_popup*` keys are present in the user's config,
/// return a one-line warning to emit at startup. Returns None when the
/// config is clean. Pure on the parsed wire so tests can drive it
/// without filesystem.
pub fn deprecation_warning() -> Option<String> {
    let path = dirs::config_dir()?.join("tad").join("config.yaml");
    let text = std::fs::read_to_string(&path).ok()?;
    let wire: Wire = serde_yml::from_str(&text).ok()?;
    deprecation_warning_for(&wire.ui)
}

fn deprecation_warning_for(ui: &UiWire) -> Option<String> {
    let mut found = Vec::new();
    if ui.auto_popup.is_some() {
        found.push("auto_popup");
    }
    if ui.auto_popup_idle_secs.is_some() {
        found.push("auto_popup_idle_secs");
    }
    if ui.auto_popup_width.is_some() {
        found.push("auto_popup_width");
    }
    if ui.auto_popup_height.is_some() {
        found.push("auto_popup_height");
    }
    if ui.auto_popup_cooldown_secs.is_some() {
        found.push("auto_popup_cooldown_secs");
    }
    if found.is_empty() {
        return None;
    }
    Some(format!(
        "ui.{} {} deprecated in v0.11 (popup removed; passive @tad-attn marker replaces it). \
         Use `attention_idle_secs` instead of `auto_popup_idle_secs`; remove the rest.",
        found.join(", ui."),
        if found.len() == 1 { "is" } else { "are" }
    ))
}

fn merge(wire: Wire) -> UiConfig {
    let defaults = UiConfig::default();
    // attention_idle: prefer the new key, fall back to the legacy
    // `auto_popup_idle_secs` if the new key is unset (so a user who
    // hasn't migrated their config yet still gets their tuned value).
    let attention_idle = wire
        .ui
        .attention_idle_secs
        .or(wire.ui.auto_popup_idle_secs)
        .map(Duration::from_secs)
        .unwrap_or(defaults.attention_idle);
    UiConfig {
        attention_idle,
        snooze_intervals: wire
            .ui
            .snooze_intervals_secs
            .map(|v| v.into_iter().map(Duration::from_secs).collect())
            .filter(|v: &Vec<Duration>| !v.is_empty())
            .unwrap_or(defaults.snooze_intervals),
        awaiting_freshness: wire
            .ui
            .awaiting_freshness_secs
            .map(Duration::from_secs)
            .unwrap_or(defaults.awaiting_freshness),
        notify_on_blocked: wire
            .ui
            .notify_on_blocked
            .unwrap_or(defaults.notify_on_blocked),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sensible() {
        let d = UiConfig::default();
        assert_eq!(d.attention_idle, Duration::from_secs(30));
        assert_eq!(
            d.snooze_intervals,
            vec![
                Duration::from_secs(300),
                Duration::from_secs(1800),
                Duration::from_secs(7200),
            ]
        );
        assert_eq!(d.awaiting_freshness, Duration::from_secs(600));
        assert!(d.notify_on_blocked);
    }

    #[test]
    fn parses_new_ui_section() {
        let yaml = "\
theme: dracula
ui:
  attention_idle_secs: 10
  snooze_intervals_secs: [60, 600, 3600]
  awaiting_freshness_secs: 120
groups:
  foo:
    hosts: [a]
";
        let wire: Wire = serde_yml::from_str(yaml).unwrap();
        let cfg = merge(wire);
        assert_eq!(cfg.attention_idle, Duration::from_secs(10));
        assert_eq!(cfg.awaiting_freshness, Duration::from_secs(120));
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
    fn legacy_auto_popup_idle_secs_is_honored_for_attention_idle() {
        // A pre-v0.11 config that only tuned auto_popup_idle_secs
        // should still take effect under the new field name.
        let yaml = "ui:\n  auto_popup_idle_secs: 90\n";
        let wire: Wire = serde_yml::from_str(yaml).unwrap();
        let cfg = merge(wire);
        assert_eq!(cfg.attention_idle, Duration::from_secs(90));
    }

    #[test]
    fn new_key_wins_when_both_legacy_and_new_present() {
        let yaml = "ui:\n  attention_idle_secs: 5\n  auto_popup_idle_secs: 90\n";
        let wire: Wire = serde_yml::from_str(yaml).unwrap();
        let cfg = merge(wire);
        assert_eq!(cfg.attention_idle, Duration::from_secs(5));
    }

    #[test]
    fn empty_snooze_list_falls_back_to_defaults() {
        let yaml = "ui:\n  snooze_intervals_secs: []\n";
        let wire: Wire = serde_yml::from_str(yaml).unwrap();
        let cfg = merge(wire);
        assert_eq!(cfg.snooze_intervals.len(), 3);
    }

    #[test]
    fn notify_on_blocked_false_is_honored() {
        let yaml = "ui:\n  notify_on_blocked: false\n";
        let wire: Wire = serde_yml::from_str(yaml).unwrap();
        let cfg = merge(wire);
        assert!(!cfg.notify_on_blocked);
    }

    #[test]
    fn notify_on_blocked_defaults_true_when_absent() {
        let yaml = "ui:\n  attention_idle_secs: 30\n";
        let wire: Wire = serde_yml::from_str(yaml).unwrap();
        let cfg = merge(wire);
        assert!(cfg.notify_on_blocked);
    }

    #[test]
    fn missing_ui_section_yields_defaults() {
        let yaml = "theme: dracula\ngroups: {}\n";
        let wire: Wire = serde_yml::from_str(yaml).unwrap();
        assert!(wire.ui.attention_idle_secs.is_none());
        assert!(wire.ui.auto_popup_idle_secs.is_none());
    }

    #[test]
    fn unknown_ui_keys_are_ignored() {
        // Future-proofing: an unknown key shouldn't break parsing.
        let yaml = "ui:\n  attention_idle_secs: 7\n  some_future_key: 42\n";
        let wire: Wire = serde_yml::from_str(yaml).unwrap();
        assert_eq!(wire.ui.attention_idle_secs, Some(7));
    }

    #[test]
    fn legacy_keys_emit_deprecation_warning() {
        let yaml = "ui:\n  auto_popup: false\n  auto_popup_width: 50%\n";
        let wire: Wire = serde_yml::from_str(yaml).unwrap();
        let warning = deprecation_warning_for(&wire.ui).expect("should warn");
        assert!(warning.contains("auto_popup"));
        assert!(warning.contains("auto_popup_width"));
    }

    #[test]
    fn clean_config_has_no_warning() {
        let yaml = "ui:\n  attention_idle_secs: 30\n";
        let wire: Wire = serde_yml::from_str(yaml).unwrap();
        assert!(deprecation_warning_for(&wire.ui).is_none());
    }
}
