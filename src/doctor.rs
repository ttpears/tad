//! `tad doctor` — diagnose the half-installed / silently-broken states
//! a user can land in when the cockpit is wired across tmux conf,
//! config.yaml, the watch pidfile, and the live tmux server.
//!
//! Each check returns one of {Pass, Warn, Fail}. Warn/Fail carry a
//! human-readable message and a suggested fix the user can copy-paste.
//! Pure-diagnose for v1; auto-`--fix` is a follow-up.

use anyhow::Result;
use std::process::Command;

use crate::{snooze, tmux_conf, ui_config};

#[derive(Debug)]
enum Verdict {
    Pass(String),
    Warn { msg: String, fix: Option<String> },
    Fail { msg: String, fix: Option<String> },
}

struct Report {
    checks: Vec<(String, Verdict)>,
}

impl Report {
    fn new() -> Self {
        Self { checks: vec![] }
    }
    fn add(&mut self, label: impl Into<String>, v: Verdict) {
        self.checks.push((label.into(), v));
    }
    /// Print the report. Returns the number of Warns and Fails so the
    /// process exit code can reflect overall health.
    fn print(&self) -> (usize, usize) {
        let mut warns = 0;
        let mut fails = 0;
        for (label, v) in &self.checks {
            match v {
                Verdict::Pass(detail) => {
                    println!("✓ {label}   {detail}");
                }
                Verdict::Warn { msg, fix } => {
                    warns += 1;
                    println!("! {label}");
                    println!("    {msg}");
                    if let Some(f) = fix {
                        println!("    fix: {f}");
                    }
                }
                Verdict::Fail { msg, fix } => {
                    fails += 1;
                    println!("✗ {label}");
                    println!("    {msg}");
                    if let Some(f) = fix {
                        println!("    fix: {f}");
                    }
                }
            }
        }
        (warns, fails)
    }
}

pub fn run() -> Result<i32> {
    let mut r = Report::new();

    check_claude(&mut r);
    check_tmux_version(&mut r);
    check_config_yaml(&mut r);
    check_marker_blocks(&mut r);
    check_watch_pidfile(&mut r);
    check_auto_popup_consistency(&mut r);
    check_legacy_groups_yaml(&mut r);
    check_snooze_count(&mut r);

    println!();
    let (warns, fails) = r.print();
    println!();
    if fails > 0 {
        println!("{fails} failure(s), {warns} warning(s) — fix the ✗ entries above");
        Ok(1)
    } else if warns > 0 {
        println!("{warns} warning(s) — tad will work, but the ! entries deserve a look");
        Ok(0)
    } else {
        println!("all checks passed");
        Ok(0)
    }
}

fn check_claude(r: &mut Report) {
    match Command::new("claude").arg("--version").output() {
        Ok(out) if out.status.success() => {
            let ver = String::from_utf8_lossy(&out.stdout).trim().to_string();
            r.add("claude in PATH", Verdict::Pass(ver));
        }
        Ok(out) => r.add(
            "claude in PATH",
            Verdict::Warn {
                msg: format!(
                    "`claude --version` exited {}",
                    out.status.code().unwrap_or(-1)
                ),
                fix: None,
            },
        ),
        Err(_) => r.add(
            "claude in PATH",
            Verdict::Fail {
                msg: "no `claude` binary on PATH — process-tree detection \
                      will still work for sessions that find one, but the \
                      Agents view will be empty if none exist"
                    .into(),
                fix: Some(
                    "install Claude Code: https://docs.claude.com/en/docs/claude-code".into(),
                ),
            },
        ),
    }
}

fn check_tmux_version(r: &mut Report) {
    match Command::new("tmux").arg("-V").output() {
        Ok(out) if out.status.success() => {
            let raw = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let parsed = parse_tmux_version(&raw);
            match parsed {
                Some((maj, min)) if (maj, min) >= (3, 2) => {
                    r.add("tmux 3.2+ for display-popup", Verdict::Pass(raw));
                }
                Some((maj, min)) => r.add(
                    "tmux 3.2+ for display-popup",
                    Verdict::Fail {
                        msg: format!(
                            "tmux {maj}.{min} — display-popup needs 3.2 or newer, \
                             so the popup keybind and `tad watch` won't work"
                        ),
                        fix: Some("upgrade tmux".into()),
                    },
                ),
                None => r.add(
                    "tmux 3.2+ for display-popup",
                    Verdict::Warn {
                        msg: format!("couldn't parse tmux version: {raw:?}"),
                        fix: None,
                    },
                ),
            }
        }
        _ => r.add(
            "tmux 3.2+ for display-popup",
            Verdict::Fail {
                msg: "no `tmux` binary on PATH".into(),
                fix: Some("install tmux".into()),
            },
        ),
    }
}

fn parse_tmux_version(s: &str) -> Option<(u32, u32)> {
    // `tmux 3.4` → ("3", "4"); also handles `tmux master-...` and the
    // `tmux 3.3a` suffix by stripping non-digit trailers.
    let rest = s.strip_prefix("tmux ")?.trim();
    let mut parts = rest.split('.');
    let maj: u32 = parts.next()?.parse().ok()?;
    let min_raw = parts.next()?;
    let min_digits: String = min_raw.chars().take_while(|c| c.is_ascii_digit()).collect();
    let min: u32 = min_digits.parse().ok()?;
    Some((maj, min))
}

fn check_config_yaml(r: &mut Report) {
    match crate::config::load() {
        Ok(doc) => r.add(
            "config.yaml parses",
            Verdict::Pass(format!("{} group(s) defined", doc.groups.len())),
        ),
        Err(e) => r.add(
            "config.yaml parses",
            Verdict::Fail {
                msg: format!("{e:#}"),
                fix: Some(format!(
                    "open {} in $EDITOR and fix the YAML",
                    crate::config::config_path().display()
                )),
            },
        ),
    }
}

fn check_marker_blocks(r: &mut Report) {
    let path = tmux_conf::resolve_path(None);
    let text = std::fs::read_to_string(&path).unwrap_or_default();

    let cases = [
        (
            "popup keybind block in tmux conf",
            "# >>> tad tmux-keybind >>>",
        ),
        (
            "#(tad status) segment block in tmux conf",
            "# >>> tad status segment >>>",
        ),
        (
            "tad watch startup hook in tmux conf",
            "# >>> tad watch startup >>>",
        ),
    ];
    for (label, marker) in cases {
        if text.contains(marker) {
            r.add(label, Verdict::Pass(path.display().to_string()));
        } else {
            r.add(
                label,
                Verdict::Warn {
                    msg: format!("not installed in {}", path.display()),
                    fix: Some("run `tad install`".into()),
                },
            );
        }
    }
}

fn check_watch_pidfile(r: &mut Report) {
    let pidfile = dirs::state_dir()
        .or_else(dirs::cache_dir)
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join("tad")
        .join("watch.pid");
    let Ok(text) = std::fs::read_to_string(&pidfile) else {
        r.add(
            "tad watch pidfile",
            Verdict::Pass("none (no watcher tracked)".into()),
        );
        return;
    };
    let pid: i32 = match text.trim().parse() {
        Ok(p) => p,
        Err(_) => {
            r.add(
                "tad watch pidfile",
                Verdict::Warn {
                    msg: format!("{} contains garbage: {:?}", pidfile.display(), text.trim()),
                    fix: Some(format!("rm {}", pidfile.display())),
                },
            );
            return;
        }
    };
    let alive = pid_is_alive(pid);
    if alive {
        r.add(
            "tad watch pidfile",
            Verdict::Pass(format!("pid {pid} alive")),
        );
    } else {
        r.add(
            "tad watch pidfile",
            Verdict::Warn {
                msg: format!(
                    "{} points at pid {pid} but that process isn't running",
                    pidfile.display()
                ),
                fix: Some(format!("rm {}", pidfile.display())),
            },
        );
    }
}

fn pid_is_alive(pid: i32) -> bool {
    let rc = unsafe { libc::kill(pid, 0) };
    if rc == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn check_auto_popup_consistency(r: &mut Report) {
    let ui = ui_config::load();
    let path = tmux_conf::resolve_path(None);
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let hook_installed = text.contains("# >>> tad watch startup >>>");
    if hook_installed && !ui.auto_popup {
        r.add(
            "ui.auto_popup vs watch hook",
            Verdict::Warn {
                msg: "the watch hook is installed but `ui.auto_popup: false` in \
                      config.yaml silences every popup — the watcher will run \
                      and do nothing"
                    .into(),
                fix: Some(
                    "either remove the hook (`tad install --uninstall`) or set \
                     `ui.auto_popup: true` (it's the default)"
                        .into(),
                ),
            },
        );
    } else {
        r.add(
            "ui.auto_popup vs watch hook",
            Verdict::Pass(format!(
                "auto_popup={}, hook {}",
                ui.auto_popup,
                if hook_installed {
                    "installed"
                } else {
                    "absent"
                }
            )),
        );
    }
}

fn check_legacy_groups_yaml(r: &mut Report) {
    let legacy = crate::config::config_dir().join("groups.yaml.migrated");
    if legacy.exists() {
        r.add(
            "legacy groups.yaml.migrated",
            Verdict::Warn {
                msg: format!("leftover from pre-v0.10 migration at {}", legacy.display()),
                fix: Some(format!(
                    "safe to delete once you've verified config.yaml is correct: rm {}",
                    legacy.display()
                )),
            },
        );
    } else {
        r.add(
            "legacy groups.yaml.migrated",
            Verdict::Pass("absent".into()),
        );
    }
}

fn check_snooze_count(r: &mut Report) {
    let s = snooze::load(std::time::SystemTime::now());
    if s.snoozes.is_empty() {
        r.add("snooze file", Verdict::Pass("no active snoozes".into()));
    } else {
        r.add(
            "snooze file",
            Verdict::Pass(format!("{} active snooze(s)", s.snoozes.len())),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_canonical_tmux_version() {
        assert_eq!(parse_tmux_version("tmux 3.4"), Some((3, 4)));
        assert_eq!(parse_tmux_version("tmux 3.3a"), Some((3, 3)));
        assert_eq!(parse_tmux_version("tmux 2.9a"), Some((2, 9)));
    }

    #[test]
    fn parse_tmux_version_handles_garbage() {
        assert_eq!(parse_tmux_version("nope"), None);
        assert_eq!(parse_tmux_version("tmux"), None);
        assert_eq!(parse_tmux_version("tmux master"), None);
    }
}
