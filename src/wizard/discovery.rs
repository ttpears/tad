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

pub fn scan_hosts(_sources: SourceSet) -> (Vec<HostCandidate>, Vec<String>) {
    (Vec::new(), Vec::new())
}

pub fn scan_tmux_sessions() -> Vec<SessionCandidate> {
    Vec::new()
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
        "-p", "-i", "-o", "-l", "-J", "-L", "-R", "-D", "-b", "-c", "-e", "-F",
        "-I", "-m", "-O", "-Q", "-S", "-W", "-w", "-B",
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

pub(crate) fn parse_known_hosts(_text: &str) -> Vec<String> {
    Vec::new()
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
}

pub(crate) fn aggregate(
    shell: Vec<String>,
    ssh_config: Vec<String>,
    known_hosts: Vec<String>,
) -> Vec<HostCandidate> {
    let mut map: BTreeMap<String, SourceFlags> = BTreeMap::new();
    for h in shell {
        map.entry(h.to_lowercase()).or_default().shell = true;
    }
    for h in ssh_config {
        map.entry(h.to_lowercase()).or_default().ssh_config = true;
    }
    for h in known_hosts {
        map.entry(h.to_lowercase()).or_default().known_hosts = true;
    }
    map.into_iter()
        .map(|(host, sources)| HostCandidate { host, sources })
        .collect()
}
