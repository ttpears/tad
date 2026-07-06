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

    check_agent_runtime(&mut r);
    check_tmux_version(&mut r);
    check_config_yaml(&mut r);
    check_marker_blocks(&mut r);
    check_watch_pidfile(&mut r);
    check_legacy_ui_keys(&mut r);
    check_legacy_groups_yaml(&mut r);
    check_snooze_count(&mut r);
    check_shell_completions(&mut r);
    check_mouse_mode(&mut r);

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

fn check_agent_runtime(r: &mut Report) {
    // Today the default provider's id happens to equal its binary name
    // (`claude`). Future providers may have a separate "preferred CLI
    // binary" property; for v1 we just probe `<id> --version`.
    let provider = crate::provider::default_provider();
    let bin = provider.id();
    let label = format!("{} in PATH", provider.label());
    match Command::new(bin).arg("--version").output() {
        Ok(out) if out.status.success() => {
            let ver = String::from_utf8_lossy(&out.stdout).trim().to_string();
            r.add(label, Verdict::Pass(ver));
        }
        Ok(out) => r.add(
            label,
            Verdict::Warn {
                msg: format!(
                    "`{bin} --version` exited {}",
                    out.status.code().unwrap_or(-1)
                ),
                fix: None,
            },
        ),
        Err(_) => r.add(
            label,
            Verdict::Fail {
                msg: format!(
                    "no `{bin}` binary on PATH — process-tree detection will \
                     still work for sessions that find one, but the Agents \
                     view will be empty if none exist"
                ),
                fix: Some(format!(
                    "install {}: https://docs.claude.com/en/docs/claude-code",
                    provider.label()
                )),
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

    // (label, begin marker, end marker). Checking both ends matters:
    // a hand-edit that deletes only the closing marker would make
    // `tad install` (and any other tad-conf operation on this file)
    // bail with "markers malformed" — that's a `Fail`, not a `Warn`,
    // because tad's own tooling can't unblock the user there.
    let cases = [
        (
            "popup keybind block in tmux conf",
            "# >>> tad tmux-keybind >>>",
            "# <<< tad tmux-keybind <<<",
        ),
        (
            "#(tad status) segment block in tmux conf",
            "# >>> tad status segment >>>",
            "# <<< tad status segment <<<",
        ),
        (
            "tad watch startup hook in tmux conf",
            "# >>> tad watch startup >>>",
            "# <<< tad watch startup <<<",
        ),
        (
            "attention marker block in tmux conf",
            "# >>> tad attention marker >>>",
            "# <<< tad attention marker <<<",
        ),
    ];
    for (label, begin, end) in cases {
        let has_begin = text.contains(begin);
        let has_end = text.contains(end);
        match (has_begin, has_end) {
            (true, true) => {
                r.add(label, Verdict::Pass(path.display().to_string()));
            }
            (false, false) => {
                r.add(
                    label,
                    Verdict::Warn {
                        msg: format!("not installed in {}", path.display()),
                        fix: Some("run `tad install`".into()),
                    },
                );
            }
            (true, false) | (false, true) => {
                let which = if has_begin { "begin" } else { "end" };
                r.add(
                    label,
                    Verdict::Fail {
                        msg: format!(
                            "{} has only the {which} marker; `tad install` and \
                             `--uninstall` will refuse to touch the file until \
                             this is fixed",
                            path.display()
                        ),
                        fix: Some(format!(
                            "open {} and either restore the missing marker or \
                             delete the block by hand",
                            path.display()
                        )),
                    },
                );
            }
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
    let alive = crate::proc_util::pid_is_alive(pid);
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

fn check_legacy_ui_keys(r: &mut Report) {
    match ui_config::deprecation_warning() {
        Some(msg) => r.add(
            "legacy ui.auto_popup* keys",
            Verdict::Warn {
                msg,
                fix: Some(
                    "edit ~/.config/tad/config.yaml: rename auto_popup_idle_secs \
                     to attention_idle_secs and delete the rest"
                        .into(),
                ),
            },
        ),
        None => r.add("legacy ui.auto_popup* keys", Verdict::Pass("none".into())),
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

/// Version the shipped completion scripts declare (the `# tad-completions:
/// vN` marker). Bump together with the marker lines in completions/ when
/// either script changes, so `tad doctor` can flag stale installed copies —
/// package upgrades replace them, but manual installs linger forever.
const COMPLETIONS_VERSION: u32 = 2;

/// Parse the `# tad-completions: vN` marker out of a completion script.
/// None = unmarked, i.e. a pre-v0.14.1 copy.
fn completion_file_version(text: &str) -> Option<u32> {
    let idx = text.find("# tad-completions: v")?;
    let rest = &text[idx + "# tad-completions: v".len()..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

fn check_shell_completions(r: &mut Report) {
    let home = dirs::home_dir();

    let mut candidates: Vec<std::path::PathBuf> = vec![
        std::path::PathBuf::from("/usr/share/bash-completion/completions/tad"),
        std::path::PathBuf::from("/etc/bash_completion.d/tad"),
        std::path::PathBuf::from("/usr/share/zsh/site-functions/_tad"),
        std::path::PathBuf::from("/usr/local/share/zsh/site-functions/_tad"),
    ];
    if let Some(ref h) = home {
        candidates.push(h.join(".local/share/bash-completion/completions/tad"));
        candidates.push(h.join(".local/share/zsh/site-functions/_tad"));
        candidates.push(h.join(".zsh/completions/_tad"));
    }

    let found: Vec<&std::path::PathBuf> = candidates.iter().filter(|p| p.is_file()).collect();
    if found.is_empty() {
        r.add(
            "shell completions",
            Verdict::Warn {
                msg: "shell completions not found — install from completions/ (tad.bash, _tad) \
                      or via your package for tab-completion of sessions/hosts/groups"
                    .into(),
                fix: Some(
                    "copy completions/tad.bash to a bash-completion dir, or completions/_tad \
                     to a zsh site-functions dir"
                        .into(),
                ),
            },
        );
        return;
    }

    // Stale = installed copy older than what this binary shipped with.
    // Unmarked files predate the version marker entirely (≤ v0.14.0,
    // which includes the zsh host-completion bug).
    let stale: Vec<String> = found
        .iter()
        .filter(|p| {
            let text = std::fs::read_to_string(p).unwrap_or_default();
            completion_file_version(&text).unwrap_or(0) < COMPLETIONS_VERSION
        })
        .map(|p| p.display().to_string())
        .collect();

    if stale.is_empty() {
        r.add(
            "shell completions",
            Verdict::Pass(format!("found, current (v{COMPLETIONS_VERSION})")),
        );
    } else {
        r.add(
            "shell completions",
            Verdict::Warn {
                msg: format!(
                    "outdated completion script(s): {} — package upgrades replace \
                     these automatically, but manually-installed copies keep old \
                     bugs (e.g. zsh host completion leaking the tab separator)",
                    stale.join(", ")
                ),
                fix: Some(
                    "replace with completions/_tad and completions/tad.bash from the \
                     current release, then start a new shell"
                        .into(),
                ),
            },
        );
    }
}

/// Classify the output of `tmux show -gv mouse` (a bare `on`/`off`, possibly
/// with trailing whitespace/newline). Pure so it's testable without a tmux
/// server; `None` means the command failed or produced unparseable output
/// (e.g. no server running, or a `tmux` too old to have the option).
fn mouse_check_result(raw: Option<&str>) -> Verdict {
    let fix = Some("set -g mouse on".to_string());
    match raw.map(str::trim) {
        Some("on") => Verdict::Pass("on".into()),
        Some("off") => Verdict::Warn {
            msg: "mouse mode is off — `set -g mouse on` enables click-to-focus \
                  for pinned panes, plus clicking, scrolling, and dragging the \
                  divider in the sidebar"
                .into(),
            fix,
        },
        Some(other) => Verdict::Warn {
            msg: format!("couldn't parse `tmux show -gv mouse` output: {other:?}"),
            fix,
        },
        None => Verdict::Warn {
            msg: "couldn't read tmux's mouse setting (no server running, or \
                  `tmux` not on PATH) — can't confirm click-to-focus will work"
                .into(),
            fix,
        },
    }
}

fn check_mouse_mode(r: &mut Report) {
    let label = "tmux mouse mode";
    let out = Command::new("tmux").args(["show", "-gv", "mouse"]).output();
    let raw = match out {
        Ok(o) if o.status.success() => Some(String::from_utf8_lossy(&o.stdout).into_owned()),
        _ => None,
    };
    r.add(label, mouse_check_result(raw.as_deref()));
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

    #[test]
    fn completion_version_parses_marker() {
        assert_eq!(
            completion_file_version("#compdef tad\n# tad-completions: v2  (notes)\n"),
            Some(2)
        );
        assert_eq!(
            completion_file_version("# tad-completions: v17\n"),
            Some(17)
        );
    }

    #[test]
    fn completion_version_none_for_unmarked_legacy_files() {
        assert_eq!(completion_file_version("#compdef tad\n_tad() { }\n"), None);
        assert_eq!(completion_file_version("# tad-completions: vX\n"), None);
    }

    #[test]
    fn mouse_check_result_pass_when_on() {
        assert!(matches!(mouse_check_result(Some("on")), Verdict::Pass(_)));
        // tmux appends a trailing newline to `show -gv` output.
        assert!(matches!(mouse_check_result(Some("on\n")), Verdict::Pass(_)));
    }

    #[test]
    fn mouse_check_result_warns_when_off() {
        match mouse_check_result(Some("off\n")) {
            Verdict::Warn { msg, fix } => {
                assert!(msg.contains("set -g mouse on"));
                assert_eq!(fix.as_deref(), Some("set -g mouse on"));
            }
            other => panic!("expected Warn, got {other:?}"),
        }
    }

    #[test]
    fn mouse_check_result_warns_on_unparseable_output() {
        assert!(matches!(
            mouse_check_result(Some("garbage")),
            Verdict::Warn { .. }
        ));
    }

    #[test]
    fn mouse_check_result_warns_when_none() {
        assert!(matches!(mouse_check_result(None), Verdict::Warn { .. }));
    }

    /// The scripts this repo ships must carry the version doctor expects —
    /// fails when someone edits completions/ without bumping both the
    /// marker and COMPLETIONS_VERSION.
    #[test]
    fn shipped_completion_scripts_match_expected_version() {
        for f in ["completions/_tad", "completions/tad.bash"] {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(f);
            let text = std::fs::read_to_string(&path).unwrap();
            assert_eq!(
                completion_file_version(&text),
                Some(COMPLETIONS_VERSION),
                "{f} marker out of sync with doctor::COMPLETIONS_VERSION"
            );
        }
    }
}
