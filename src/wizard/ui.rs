//! Ratatui state machine and render loop for the wizard.

#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;

use crate::config;
use crate::wizard::discovery::{HostCandidate, SessionCandidate};
use crate::wizard::SourceSet;

pub const LAYOUTS: &[&str] = &["panes", "synced-panes", "windows", "browse"];

#[derive(Debug, Clone, Copy)]
pub enum Entry {
    FirstLaunch,
    Config,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    EditMode,
    Welcome,
    Sessions,
    Hosts,
    BuildGroups,
    Confirm,
    Done,
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct GroupForm {
    pub name: String,
    pub layout_idx: usize,
    pub members: BTreeSet<String>,
}

impl Default for GroupForm {
    fn default() -> Self {
        Self {
            name: String::new(),
            layout_idx: 0,
            members: BTreeSet::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct WizardState {
    pub stage: Stage,
    pub sources: SourceSet,
    pub host_candidates: Vec<HostCandidate>,
    pub session_candidates: Vec<SessionCandidate>,
    pub selected_hosts: BTreeSet<String>,
    pub selected_sessions: BTreeSet<String>,
    pub session_overrides: BTreeMap<String, (String, usize)>,
    pub filter: String,
    pub built: Vec<(String, config::Group)>,
    pub form: GroupForm,
    pub scan_errors: Vec<String>,
    pub config_exists: bool,
    pub status_flash: Option<String>,
}

impl WizardState {
    pub fn for_first_launch() -> Self {
        Self {
            stage: Stage::Welcome,
            sources: SourceSet::ALL,
            host_candidates: Vec::new(),
            session_candidates: Vec::new(),
            selected_hosts: BTreeSet::new(),
            selected_sessions: BTreeSet::new(),
            session_overrides: BTreeMap::new(),
            filter: String::new(),
            built: Vec::new(),
            form: GroupForm::default(),
            scan_errors: Vec::new(),
            config_exists: false,
            status_flash: None,
        }
    }

    pub fn for_config(config_exists: bool) -> Self {
        let mut s = Self::for_first_launch();
        s.config_exists = config_exists;
        s.stage = if config_exists { Stage::EditMode } else { Stage::Welcome };
        s
    }

    pub fn next_stage_from(&self, current: Stage) -> Option<Stage> {
        match current {
            Stage::EditMode => Some(Stage::Welcome),
            Stage::Welcome => {
                if self.sources.tmux_sessions {
                    Some(Stage::Sessions)
                } else if self.sources.shell
                    || self.sources.ssh_config
                    || self.sources.known_hosts
                {
                    Some(Stage::Hosts)
                } else {
                    None
                }
            }
            Stage::Sessions => {
                if self.sources.shell
                    || self.sources.ssh_config
                    || self.sources.known_hosts
                {
                    Some(Stage::Hosts)
                } else {
                    Some(Stage::Confirm)
                }
            }
            Stage::Hosts => Some(Stage::BuildGroups),
            Stage::BuildGroups => Some(Stage::Confirm),
            Stage::Confirm => Some(Stage::Done),
            Stage::Done | Stage::Cancelled => None,
        }
    }

    pub fn can_advance(&self, stage: Stage) -> Result<(), &'static str> {
        match stage {
            Stage::Welcome => {
                if self.sources.count() == 0 {
                    Err("select at least one source")
                } else {
                    Ok(())
                }
            }
            Stage::Hosts => {
                if self.selected_hosts.is_empty() {
                    Err("select at least one host")
                } else {
                    Ok(())
                }
            }
            _ => Ok(()),
        }
    }

    pub fn toggle_source(&mut self, idx: usize) {
        match idx {
            0 => self.sources.shell = !self.sources.shell,
            1 => self.sources.ssh_config = !self.sources.ssh_config,
            2 => self.sources.known_hosts = !self.sources.known_hosts,
            3 => self.sources.tmux_sessions = !self.sources.tmux_sessions,
            _ => {}
        }
    }

    pub fn toggle_host(&mut self, host: &str) {
        if !self.selected_hosts.remove(host) {
            self.selected_hosts.insert(host.to_string());
        }
    }

    pub fn commit_form(&mut self) -> Result<(), &'static str> {
        let name = self.form.name.trim().to_string();
        if name.is_empty() {
            return Err("group name required");
        }
        if self.built.iter().any(|(n, _)| n == &name) {
            return Err("group name already used in this session");
        }
        if self.form.members.is_empty() {
            return Err("pick at least one host for the group");
        }
        let layout = LAYOUTS[self.form.layout_idx].to_string();
        let hosts: Vec<String> = self.form.members.iter().cloned().collect();
        self.built.push((name, config::Group { layout, hosts }));
        self.form = GroupForm::default();
        Ok(())
    }

    pub fn assemble_groups(&self) -> Vec<(String, config::Group)> {
        let mut out: Vec<(String, config::Group)> = Vec::new();
        for s in &self.session_candidates {
            if !self.selected_sessions.contains(&s.name) {
                continue;
            }
            let (name, layout_idx) = self
                .session_overrides
                .get(&s.name)
                .cloned()
                .unwrap_or_else(|| (s.name.clone(), 2));
            out.push((
                name,
                config::Group {
                    layout: LAYOUTS[layout_idx].to_string(),
                    hosts: s.windows.clone(),
                },
            ));
        }
        out.extend(self.built.clone());
        out
    }
}

pub fn merge_into_doc(
    doc: &mut config::Doc,
    incoming: Vec<(String, config::Group)>,
) -> Vec<(String, String)> {
    let mut renames = Vec::new();
    for (name, group) in incoming {
        if !doc.groups.contains_key(&name) {
            doc.groups.insert(name, group);
            continue;
        }
        let mut suffix = 2;
        let final_name = loop {
            let candidate = format!("{}-{}", name, suffix);
            if !doc.groups.contains_key(&candidate) {
                break candidate;
            }
            suffix += 1;
        };
        renames.push((name.clone(), final_name.clone()));
        doc.groups.insert(final_name, group);
    }
    renames
}

pub fn run(_entry: Entry) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn welcome_requires_at_least_one_source_to_advance() {
        let mut s = WizardState::for_first_launch();
        s.sources = SourceSet::NONE;
        assert!(s.can_advance(Stage::Welcome).is_err());
        s.sources.shell = true;
        assert!(s.can_advance(Stage::Welcome).is_ok());
    }

    #[test]
    fn next_stage_skips_sessions_when_off() {
        let mut s = WizardState::for_first_launch();
        s.sources = SourceSet { shell: true, ssh_config: false, known_hosts: false, tmux_sessions: false };
        assert_eq!(s.next_stage_from(Stage::Welcome), Some(Stage::Hosts));
    }

    #[test]
    fn next_stage_skips_hosts_when_only_sessions_on() {
        let mut s = WizardState::for_first_launch();
        s.sources = SourceSet { shell: false, ssh_config: false, known_hosts: false, tmux_sessions: true };
        assert_eq!(s.next_stage_from(Stage::Welcome), Some(Stage::Sessions));
        assert_eq!(s.next_stage_from(Stage::Sessions), Some(Stage::Confirm));
    }

    #[test]
    fn host_screen_requires_selection() {
        let mut s = WizardState::for_first_launch();
        assert!(s.can_advance(Stage::Hosts).is_err());
        s.selected_hosts.insert("h1".to_string());
        assert!(s.can_advance(Stage::Hosts).is_ok());
    }

    #[test]
    fn commit_form_validates_and_clears() {
        let mut s = WizardState::for_first_launch();
        s.form.name = "".into();
        assert!(s.commit_form().is_err());
        s.form.name = "prod".into();
        assert!(s.commit_form().is_err());
        s.form.members.insert("h1".into());
        s.form.layout_idx = 2;
        s.commit_form().unwrap();
        assert_eq!(s.built.len(), 1);
        assert_eq!(s.built[0].0, "prod");
        assert_eq!(s.built[0].1.layout, "windows");
        assert_eq!(s.built[0].1.hosts, vec!["h1".to_string()]);
        assert!(s.form.name.is_empty());
        assert!(s.form.members.is_empty());
    }

    #[test]
    fn commit_form_rejects_duplicate_name_within_run() {
        let mut s = WizardState::for_first_launch();
        s.form.name = "g".into();
        s.form.members.insert("h".into());
        s.commit_form().unwrap();
        s.form.name = "g".into();
        s.form.members.insert("h2".into());
        assert!(s.commit_form().is_err());
    }

    #[test]
    fn assemble_groups_applies_session_overrides_and_appends_built() {
        let mut s = WizardState::for_first_launch();
        s.session_candidates = vec![SessionCandidate {
            name: "tmuxA".into(),
            windows: vec!["h1".into(), "h2".into()],
            usable: true,
        }];
        s.selected_sessions.insert("tmuxA".into());
        s.session_overrides.insert("tmuxA".into(), ("renamed".into(), 0));
        s.form.name = "hand".into();
        s.form.members.insert("h3".into());
        s.commit_form().unwrap();
        let out = s.assemble_groups();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].0, "renamed");
        assert_eq!(out[0].1.layout, "panes");
        assert_eq!(out[1].0, "hand");
    }

    #[test]
    fn merge_into_doc_resolves_collisions() {
        let mut doc = config::Doc::default();
        doc.groups.insert("g".into(), config::Group { layout: "windows".into(), hosts: vec!["x".into()] });
        let incoming = vec![("g".into(), config::Group { layout: "panes".into(), hosts: vec!["y".into()] })];
        let renames = merge_into_doc(&mut doc, incoming);
        assert_eq!(renames, vec![("g".into(), "g-2".into())]);
        assert!(doc.groups.contains_key("g"));
        assert!(doc.groups.contains_key("g-2"));
    }
}
