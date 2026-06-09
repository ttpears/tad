//! Local-only scanning for SSH hosts (shell history, ssh-config, known_hosts).

use std::collections::BTreeMap;

use serde::Deserialize;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SourceFlags {
    pub shell: bool,
    pub ssh_config: bool,
    pub known_hosts: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HostCandidate {
    pub host: String,
    pub sources: SourceFlags,
    /// Number of times this host appeared in shell history. 0 for hosts
    /// that came only from ssh-config / known_hosts.
    pub count: usize,
}

/// Tunables for discovery. All fields default so a config file with no
/// `discovery:` section behaves identically to all-on with threshold 2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiscoveryConfig {
    /// History-only hosts seen fewer than this many times are hidden
    /// (unless they also appear in ssh-config / known_hosts).
    pub min_history_uses: usize,
    pub shell_history: bool,
    pub ssh_config: bool,
    pub known_hosts: bool,
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        Self {
            min_history_uses: 2,
            shell_history: true,
            ssh_config: true,
            known_hosts: true,
        }
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct DiscoveryWire {
    min_history_uses: Option<usize>,
    shell_history: Option<bool>,
    ssh_config: Option<bool>,
    known_hosts: Option<bool>,
}

/// Top-level fragment: just the `discovery:` key. Other keys in the same
/// file (theme, ui, groups, _meta) are ignored.
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct Wire {
    discovery: DiscoveryWire,
}

impl DiscoveryConfig {
    pub(crate) fn load() -> Self {
        let path = match dirs::config_dir() {
            Some(p) => p.join("tad").join("config.yaml"),
            None => return Self::default(),
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(_) => return Self::default(),
        };
        let wire: Wire = serde_yml::from_str(&text).unwrap_or_default();
        Self::from_wire(wire.discovery)
    }

    fn from_wire(w: DiscoveryWire) -> Self {
        let d = Self::default();
        Self {
            min_history_uses: w.min_history_uses.unwrap_or(d.min_history_uses),
            shell_history: w.shell_history.unwrap_or(d.shell_history),
            ssh_config: w.ssh_config.unwrap_or(d.ssh_config),
            known_hosts: w.known_hosts.unwrap_or(d.known_hosts),
        }
    }
}

/// Human-readable source tag for a discovered host, e.g.
/// "ssh-config, known" or "history ×7". Empty if no sources set.
pub(crate) fn source_tag(h: &HostCandidate) -> String {
    let mut t = Vec::new();
    if h.sources.ssh_config {
        t.push("ssh-config".to_string());
    }
    if h.sources.known_hosts {
        t.push("known".to_string());
    }
    if h.sources.shell {
        t.push(format!("history \u{00d7}{}", h.count));
    }
    t.join(", ")
}

fn is_high_signal(h: &HostCandidate) -> bool {
    h.sources.ssh_config || h.sources.known_hosts
}

/// Drop low-signal noise, then order by signal: config/known first, then
/// shell hosts by frequency (desc), then case-insensitive name.
pub(crate) fn rank_and_filter(
    mut hosts: Vec<HostCandidate>,
    cfg: &DiscoveryConfig,
) -> Vec<HostCandidate> {
    hosts.retain(|h| is_high_signal(h) || h.count >= cfg.min_history_uses);
    hosts.sort_by(|a, b| {
        (is_high_signal(b) as u8)
            .cmp(&(is_high_signal(a) as u8))
            .then(b.count.cmp(&a.count))
            .then(a.host.to_lowercase().cmp(&b.host.to_lowercase()))
    });
    hosts
}

/// The one entry point for live discovery: scan enabled sources, rank,
/// and filter. Per-source read errors from `scan_hosts` are ignored here —
/// discovery is best-effort and never blocks on an unreadable file.
pub(crate) fn discover(cfg: &DiscoveryConfig) -> Vec<HostCandidate> {
    let (hosts, _errors) = scan_hosts(cfg);
    rank_and_filter(hosts, cfg)
}

pub(crate) fn scan_hosts(cfg: &DiscoveryConfig) -> (Vec<HostCandidate>, Vec<String>) {
    let mut shell = Vec::new();
    let mut ssh_cfg = Vec::new();
    let mut khosts = Vec::new();
    let mut errors = Vec::new();

    if cfg.shell_history {
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

    if cfg.ssh_config {
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

    if cfg.known_hosts {
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
    let mut map: BTreeMap<String, (String, SourceFlags, usize)> = BTreeMap::new();
    for h in shell {
        let key = h.to_lowercase();
        let entry = map
            .entry(key)
            .or_insert((h.clone(), SourceFlags::default(), 0));
        entry.1.shell = true;
        entry.2 += 1;
    }
    for h in ssh_config {
        let key = h.to_lowercase();
        let entry = map
            .entry(key)
            .or_insert((h.clone(), SourceFlags::default(), 0));
        entry.1.ssh_config = true;
    }
    for h in known_hosts {
        let key = h.to_lowercase();
        let entry = map
            .entry(key)
            .or_insert((h.clone(), SourceFlags::default(), 0));
        entry.1.known_hosts = true;
    }
    map.into_iter()
        .map(|(_, (host, sources, count))| HostCandidate {
            host,
            sources,
            count,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_tag_formats_all_combinations() {
        let h = HostCandidate {
            host: "x".into(),
            sources: SourceFlags {
                ssh_config: true,
                known_hosts: true,
                shell: false,
            },
            count: 0,
        };
        assert_eq!(source_tag(&h), "ssh-config, known");
        let h = HostCandidate {
            host: "x".into(),
            sources: SourceFlags {
                shell: true,
                ..Default::default()
            },
            count: 7,
        };
        assert_eq!(source_tag(&h), "history \u{00d7}7");
        let h = HostCandidate {
            host: "x".into(),
            sources: SourceFlags::default(),
            count: 0,
        };
        assert_eq!(source_tag(&h), "");
    }

    #[test]
    fn bash_history_extracts_hosts_strips_users_and_flags() {
        let text = include_str!("../tests/fixtures/wizard/shell_history_bash.txt");
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
        let text = include_str!("../tests/fixtures/wizard/shell_history_bash.txt");
        let hosts = parse_bash_zsh_history(text);
        assert!(!hosts.iter().any(|h| h.contains("nfs")));
        assert!(!hosts.iter().any(|h| h.starts_with("-")));
        assert!(!hosts.iter().any(|h| h.contains('/')));
        assert!(!hosts.iter().any(|h| h.contains('@')));
    }

    #[test]
    fn fish_history_extracts_hosts() {
        let text = include_str!("../tests/fixtures/wizard/shell_history_fish.txt");
        let hosts = parse_fish_history(text);
        assert!(hosts.contains(&"fish-host1.example.com".to_string()));
        assert!(hosts.contains(&"fish-db.example.com".to_string()));
        assert_eq!(hosts.len(), 2);
    }

    #[test]
    fn ssh_config_extracts_concrete_hosts() {
        let text = include_str!("../tests/fixtures/wizard/ssh_config.txt");
        let hosts = parse_ssh_config(text);
        assert!(hosts.contains(&"bastion.example.com".to_string()));
        assert!(hosts.contains(&"db1".to_string()));
        assert!(hosts.contains(&"db2".to_string()));
        assert!(hosts.contains(&"indented-host.example.com".to_string()));
    }

    #[test]
    fn ssh_config_skips_wildcards() {
        let text = include_str!("../tests/fixtures/wizard/ssh_config.txt");
        let hosts = parse_ssh_config(text);
        assert!(!hosts.iter().any(|h| h.contains('*')));
        assert!(!hosts.iter().any(|h| h.contains('?')));
        assert!(!hosts.contains(&"prod-*".to_string()));
    }

    #[test]
    fn known_hosts_parses_plain_and_comma_lists() {
        let text = include_str!("../tests/fixtures/wizard/known_hosts.txt");
        let hosts = parse_known_hosts(text);
        assert!(hosts.contains(&"host1.example.com".to_string()));
        assert!(hosts.contains(&"host2.example.com".to_string()));
        assert!(hosts.contains(&"10.0.0.2".to_string()));
        assert!(hosts.contains(&"host3".to_string()));
        assert!(hosts.contains(&"host3-alias.example.com".to_string()));
    }

    #[test]
    fn known_hosts_strips_brackets_and_skips_hashed_and_ca() {
        let text = include_str!("../tests/fixtures/wizard/known_hosts.txt");
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
    fn aggregate_counts_shell_frequency() {
        let result = aggregate(
            vec!["a".into(), "a".into(), "a".into(), "b".into()],
            vec![],
            vec![],
        );
        let a = result.iter().find(|c| c.host == "a").unwrap();
        let b = result.iter().find(|c| c.host == "b").unwrap();
        assert_eq!(a.count, 3);
        assert_eq!(b.count, 1);
    }

    #[test]
    fn aggregate_count_is_zero_for_non_shell_sources() {
        let result = aggregate(vec![], vec!["x".into()], vec!["y".into()]);
        assert!(result.iter().all(|c| c.count == 0));
    }

    #[test]
    fn scan_hosts_reports_missing_files_as_errors_not_panics() {
        let (candidates, errors) = scan_hosts(&DiscoveryConfig::default());
        for e in &errors {
            assert!(!e.is_empty());
        }
        let _ = candidates;
    }

    #[test]
    fn rank_orders_config_and_known_before_history() {
        let hosts = vec![
            HostCandidate {
                host: "hist".into(),
                sources: SourceFlags {
                    shell: true,
                    ..Default::default()
                },
                count: 9,
            },
            HostCandidate {
                host: "cfg".into(),
                sources: SourceFlags {
                    ssh_config: true,
                    ..Default::default()
                },
                count: 0,
            },
        ];
        let out = rank_and_filter(hosts, &DiscoveryConfig::default());
        assert_eq!(out[0].host, "cfg");
        assert_eq!(out[1].host, "hist");
    }

    #[test]
    fn rank_orders_history_by_count_desc() {
        let hosts = vec![
            HostCandidate {
                host: "low".into(),
                sources: SourceFlags {
                    shell: true,
                    ..Default::default()
                },
                count: 2,
            },
            HostCandidate {
                host: "high".into(),
                sources: SourceFlags {
                    shell: true,
                    ..Default::default()
                },
                count: 7,
            },
        ];
        let out = rank_and_filter(hosts, &DiscoveryConfig::default());
        assert_eq!(out[0].host, "high");
        assert_eq!(out[1].host, "low");
    }

    #[test]
    fn filter_drops_history_only_below_threshold_but_keeps_config_hosts() {
        let hosts = vec![
            HostCandidate {
                host: "oneoff".into(),
                sources: SourceFlags {
                    shell: true,
                    ..Default::default()
                },
                count: 1,
            },
            HostCandidate {
                host: "kept".into(),
                sources: SourceFlags {
                    shell: true,
                    ..Default::default()
                },
                count: 2,
            },
            HostCandidate {
                host: "incfg".into(),
                sources: SourceFlags {
                    shell: true,
                    ssh_config: true,
                    ..Default::default()
                },
                count: 1,
            },
        ];
        let out = rank_and_filter(hosts, &DiscoveryConfig::default());
        let names: Vec<_> = out.iter().map(|h| h.host.as_str()).collect();
        assert!(!names.contains(&"oneoff"));
        assert!(names.contains(&"kept"));
        assert!(names.contains(&"incfg"));
    }

    #[test]
    fn discovery_config_defaults() {
        let d = DiscoveryConfig::default();
        assert_eq!(d.min_history_uses, 2);
        assert!(d.shell_history && d.ssh_config && d.known_hosts);
    }

    #[test]
    fn discovery_config_from_wire_overrides_and_defaults() {
        let yaml = "discovery:\n  min_history_uses: 5\n  shell_history: false\n";
        let wire: Wire = serde_yml::from_str(yaml).unwrap();
        let cfg = DiscoveryConfig::from_wire(wire.discovery);
        assert_eq!(cfg.min_history_uses, 5);
        assert!(!cfg.shell_history);
        assert!(cfg.ssh_config);
    }

    #[test]
    fn discovery_config_missing_section_is_default() {
        let yaml = "theme: dracula\ngroups: {}\n";
        let wire: Wire = serde_yml::from_str(yaml).unwrap();
        assert_eq!(
            DiscoveryConfig::from_wire(wire.discovery),
            DiscoveryConfig::default()
        );
    }
}
