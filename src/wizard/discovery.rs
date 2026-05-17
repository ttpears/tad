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

pub(crate) fn parse_bash_zsh_history(_text: &str) -> Vec<String> {
    Vec::new()
}

pub(crate) fn parse_fish_history(_text: &str) -> Vec<String> {
    Vec::new()
}

pub(crate) fn parse_ssh_config(_text: &str) -> Vec<String> {
    Vec::new()
}

pub(crate) fn parse_known_hosts(_text: &str) -> Vec<String> {
    Vec::new()
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
