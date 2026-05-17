//! Local-only scanning for SSH hosts and importable tmux sessions.

#![allow(dead_code)]

use std::collections::BTreeMap;

use crate::wizard::SourceSet;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceFlags {
    pub shell: bool,
    pub ssh_config: bool,
    pub known_hosts: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostCandidate {
    pub host: String,
    pub sources: SourceFlags,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionCandidate {
    pub name: String,
    pub windows: Vec<String>,
    pub usable: bool,
}

pub fn scan_hosts(sources: SourceSet) -> (Vec<HostCandidate>, Vec<String>) {
    let mut shell = Vec::new();
    let mut ssh_cfg = Vec::new();
    let mut khosts = Vec::new();
    let mut errors = Vec::new();

    if sources.shell {
        let paths = shell_history_paths();
        let mut any_ok = false;
        for (label, path) in &paths {
            if let Ok(text) = std::fs::read_to_string(path) {
                any_ok = true;
                if label == &"fish" {
                    shell.extend(parse_fish_history(&text));
                } else {
                    shell.extend(parse_bash_zsh_history(&text));
                }
            }
        }
        if !any_ok && !paths.is_empty() {
            errors.push("couldn't read shell history (no readable files)".to_string());
        }
    }

    if sources.ssh_config {
        if let Some(home) = dirs::home_dir() {
            let path = home.join(".ssh").join("config");
            match std::fs::read_to_string(&path) {
                Ok(text) => {
                    ssh_cfg.extend(parse_ssh_config(&text));
                    for include in parse_ssh_config_includes(&text) {
                        let inc_path = if include.starts_with('/') {
                            std::path::PathBuf::from(&include)
                        } else if let Some(rest) = include.strip_prefix("~/") {
                            home.join(rest)
                        } else {
                            home.join(".ssh").join(&include)
                        };
                        if let Ok(t) = std::fs::read_to_string(&inc_path) {
                            ssh_cfg.extend(parse_ssh_config(&t));
                        }
                    }
                }
                Err(_) => errors.push("couldn't read ~/.ssh/config".to_string()),
            }
        }
    }

    if sources.known_hosts {
        if let Some(home) = dirs::home_dir() {
            let path = home.join(".ssh").join("known_hosts");
            match std::fs::read_to_string(&path) {
                Ok(text) => khosts.extend(parse_known_hosts(&text)),
                Err(_) => errors.push("couldn't read ~/.ssh/known_hosts".to_string()),
            }
        }
    }

    (aggregate(shell, ssh_cfg, khosts), errors)
}

fn shell_history_paths() -> Vec<(&'static str, std::path::PathBuf)> {
    let mut out: Vec<(&'static str, std::path::PathBuf)> = Vec::new();
    if let Ok(h) = std::env::var("HISTFILE") {
        if !h.is_empty() {
            out.push(("bash", std::path::PathBuf::from(h)));
        }
    }
    if let Some(home) = dirs::home_dir() {
        out.push(("bash", home.join(".bash_history")));
        out.push(("zsh", home.join(".zsh_history")));
        out.push(("fish", home.join(".local/share/fish/fish_history")));
    }
    out
}

pub(crate) fn parse_ssh_config_includes(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.splitn(2, char::is_whitespace);
        let key = parts.next().unwrap_or("");
        if key.eq_ignore_ascii_case("include") {
            if let Some(rest) = parts.next() {
                for tok in rest.split_whitespace() {
                    out.push(tok.to_string());
                }
            }
        }
    }
    out
}

pub fn scan_tmux_sessions() -> Vec<SessionCandidate> {
    use crate::tmux;
    let out = match tmux::run(["list-sessions", "-F", "#{session_name}"]) {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };
    let names: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(|s| s.to_string())
        .collect();
    let mut raw = Vec::new();
    for name in names {
        let wins = match tmux::run(["list-windows", "-t", &name, "-F", "#{window_name}"]) {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter(|l| !l.is_empty())
                .map(|s| s.to_string())
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        };
        raw.push((name, wins));
    }
    scan_tmux_sessions_from(raw)
}

pub(crate) fn scan_tmux_sessions_from(raw: Vec<(String, Vec<String>)>) -> Vec<SessionCandidate> {
    raw.into_iter()
        .map(|(name, windows)| {
            let usable = windows.iter().any(|w| is_usable_window_name(w));
            SessionCandidate {
                name,
                windows,
                usable,
            }
        })
        .collect()
}

fn is_usable_window_name(name: &str) -> bool {
    let n = name.trim();
    if n.is_empty() {
        return false;
    }
    !matches!(n, "bash" | "zsh" | "sh" | "fish") && !n.chars().all(|c| c.is_ascii_digit())
}

pub(crate) fn parse_bash_zsh_history(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in text.lines() {
        // zsh extended history lines look like `: 1700000000:0;ssh host`
        let line = match raw.find(';') {
            Some(idx) if raw.starts_with(": ") => &raw[idx + 1..],
            _ => raw,
        };
        if let Some(host) = extract_ssh_host(line) {
            out.push(host);
        }
    }
    out
}

fn extract_ssh_host(line: &str) -> Option<String> {
    let mut tokens = line.split_whitespace();
    let cmd = tokens.next()?;
    if cmd != "ssh" {
        return None;
    }
    const FLAGS_WITH_VALUE: &[&str] = &[
        "-p", "-i", "-o", "-l", "-J", "-L", "-R", "-D", "-b", "-c", "-e", "-F", "-I", "-m", "-O",
        "-Q", "-S", "-W", "-w", "-B",
    ];
    let mut skip_next = false;
    for tok in tokens {
        if skip_next {
            skip_next = false;
            continue;
        }
        if FLAGS_WITH_VALUE.contains(&tok) {
            skip_next = true;
            continue;
        }
        if tok == "--" {
            continue;
        }
        if tok.starts_with('-') {
            continue;
        }
        if tok.is_empty() || tok.contains('/') {
            return None;
        }
        let host = tok.split_once('@').map(|(_, h)| h).unwrap_or(tok);
        if host.is_empty() {
            return None;
        }
        return Some(host.to_string());
    }
    None
}

pub(crate) fn parse_fish_history(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("- cmd:") {
            let cmd = rest.trim();
            if let Some(host) = extract_ssh_host(cmd) {
                out.push(host);
            }
        }
    }
    out
}

pub(crate) fn parse_ssh_config(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.splitn(2, char::is_whitespace);
        let key = parts.next().unwrap_or("");
        if !key.eq_ignore_ascii_case("host") {
            continue;
        }
        let rest = parts.next().unwrap_or("").trim();
        for pat in rest.split_whitespace() {
            if pat.contains('*') || pat.contains('?') {
                continue;
            }
            if pat.is_empty() {
                continue;
            }
            out.push(pat.to_string());
        }
    }
    out
}

pub(crate) fn parse_known_hosts(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with("|1|") || line.starts_with("@cert-authority") {
            continue;
        }
        let first = line.split_whitespace().next().unwrap_or("");
        for entry in first.split(',') {
            let mut e = entry.trim();
            if let Some(stripped) = e.strip_prefix('[') {
                if let Some(idx) = stripped.rfind(']') {
                    e = &stripped[..idx];
                }
            }
            if e.is_empty() {
                continue;
            }
            out.push(e.to_string());
        }
    }
    out
}

pub(crate) fn aggregate(
    shell: Vec<String>,
    ssh_config: Vec<String>,
    known_hosts: Vec<String>,
) -> Vec<HostCandidate> {
    let mut map: BTreeMap<String, (String, SourceFlags)> = BTreeMap::new();
    let mut record = |host: String, set: fn(&mut SourceFlags)| {
        let key = host.to_lowercase();
        let entry = map
            .entry(key)
            .or_insert((host.clone(), SourceFlags::default()));
        set(&mut entry.1);
    };
    for h in shell {
        record(h, |f| f.shell = true);
    }
    for h in ssh_config {
        record(h, |f| f.ssh_config = true);
    }
    for h in known_hosts {
        record(h, |f| f.known_hosts = true);
    }
    map.into_iter()
        .map(|(_, (display, sources))| HostCandidate {
            host: display,
            sources,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bash_history_extracts_hosts_strips_users_and_flags() {
        let text = include_str!("../../tests/fixtures/wizard/shell_history_bash.txt");
        let hosts = parse_bash_zsh_history(text);
        assert!(hosts.contains(&"prod-web1.example.com".to_string()));
        assert!(hosts.contains(&"db1".to_string()));
        assert!(hosts.contains(&"bastion.example.com".to_string()));
        assert!(hosts.contains(&"jumpbox".to_string()));
        assert!(hosts.contains(&"final.example.com".to_string()));
        assert!(hosts.contains(&"PROD-web1.example.com".to_string()));
    }

    #[test]
    fn bash_history_rejects_non_ssh_and_garbage() {
        let text = include_str!("../../tests/fixtures/wizard/shell_history_bash.txt");
        let hosts = parse_bash_zsh_history(text);
        assert!(!hosts.iter().any(|h| h.contains("nfs")));
        assert!(!hosts.iter().any(|h| h.starts_with("-")));
        assert!(!hosts.iter().any(|h| h.contains('/')));
        assert!(!hosts.iter().any(|h| h.contains('@')));
    }

    #[test]
    fn fish_history_extracts_hosts() {
        let text = include_str!("../../tests/fixtures/wizard/shell_history_fish.txt");
        let hosts = parse_fish_history(text);
        assert!(hosts.contains(&"fish-host1.example.com".to_string()));
        assert!(hosts.contains(&"fish-db.example.com".to_string()));
        assert_eq!(hosts.len(), 2);
    }

    #[test]
    fn ssh_config_extracts_concrete_hosts() {
        let text = include_str!("../../tests/fixtures/wizard/ssh_config.txt");
        let hosts = parse_ssh_config(text);
        assert!(hosts.contains(&"bastion.example.com".to_string()));
        assert!(hosts.contains(&"db1".to_string()));
        assert!(hosts.contains(&"db2".to_string()));
        assert!(hosts.contains(&"indented-host.example.com".to_string()));
    }

    #[test]
    fn ssh_config_skips_wildcards() {
        let text = include_str!("../../tests/fixtures/wizard/ssh_config.txt");
        let hosts = parse_ssh_config(text);
        assert!(!hosts.iter().any(|h| h.contains('*')));
        assert!(!hosts.iter().any(|h| h.contains('?')));
        assert!(!hosts.contains(&"prod-*".to_string()));
    }

    #[test]
    fn known_hosts_parses_plain_and_comma_lists() {
        let text = include_str!("../../tests/fixtures/wizard/known_hosts.txt");
        let hosts = parse_known_hosts(text);
        assert!(hosts.contains(&"host1.example.com".to_string()));
        assert!(hosts.contains(&"host2.example.com".to_string()));
        assert!(hosts.contains(&"10.0.0.2".to_string()));
        assert!(hosts.contains(&"host3".to_string()));
        assert!(hosts.contains(&"host3-alias.example.com".to_string()));
    }

    #[test]
    fn known_hosts_strips_brackets_and_skips_hashed_and_ca() {
        let text = include_str!("../../tests/fixtures/wizard/known_hosts.txt");
        let hosts = parse_known_hosts(text);
        assert!(hosts.contains(&"bracketed.example.com".to_string()));
        assert!(!hosts.iter().any(|h| h.starts_with('|')));
        assert!(!hosts.iter().any(|h| h.starts_with('@')));
        assert!(!hosts.iter().any(|h| h.contains('[')));
        assert!(!hosts.iter().any(|h| h.contains(']')));
    }

    #[test]
    fn aggregate_dedupes_case_insensitively_and_unions_sources() {
        let result = aggregate(
            vec!["Foo.Example.com".to_string(), "bar".to_string()],
            vec!["foo.example.com".to_string()],
            vec!["BAR".to_string(), "baz".to_string()],
        );
        let foo = result
            .iter()
            .find(|c| c.host.eq_ignore_ascii_case("foo.example.com"))
            .unwrap();
        assert_eq!(foo.host, "Foo.Example.com"); // first-seen casing preserved
        assert!(foo.sources.shell);
        assert!(foo.sources.ssh_config);
        assert!(!foo.sources.known_hosts);

        let bar = result.iter().find(|c| c.host == "bar").unwrap();
        assert!(bar.sources.shell);
        assert!(!bar.sources.ssh_config);
        assert!(bar.sources.known_hosts);

        assert_eq!(result.len(), 3);
    }

    #[test]
    fn ssh_history_skips_double_dash_separator() {
        let hosts = parse_bash_zsh_history("ssh host.example.com -- -ignored\nssh -- realdest\n");
        assert!(hosts.contains(&"host.example.com".to_string()));
        assert!(hosts.contains(&"realdest".to_string()));
        assert!(!hosts.iter().any(|h| h == "--" || h == "-ignored"));
    }

    #[test]
    fn scan_hosts_reports_missing_files_as_errors_not_panics() {
        let (candidates, errors) = scan_hosts(SourceSet::ALL);
        for e in &errors {
            assert!(!e.is_empty());
        }
        let _ = candidates;
    }

    #[test]
    fn tmux_sessions_marks_unusable_when_all_windows_are_shell_names() {
        let raw = vec![
            (
                "prod-web".to_string(),
                vec!["host1".to_string(), "host2".to_string()],
            ),
            (
                "just-shell".to_string(),
                vec!["bash".to_string(), "zsh".to_string()],
            ),
            (
                "numbers".to_string(),
                vec!["1".to_string(), "2".to_string()],
            ),
            (
                "mixed".to_string(),
                vec!["bash".to_string(), "realhost".to_string()],
            ),
        ];
        let candidates = scan_tmux_sessions_from(raw);
        let by_name: std::collections::HashMap<_, _> =
            candidates.iter().map(|c| (c.name.clone(), c)).collect();
        assert!(by_name["prod-web"].usable);
        assert!(!by_name["just-shell"].usable);
        assert!(!by_name["numbers"].usable);
        assert!(by_name["mixed"].usable);
    }
}
