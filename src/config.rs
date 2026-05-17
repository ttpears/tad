//! ~/.config/tad/groups.yaml schema and IO.

use anyhow::{Context, Result};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Group {
    /// One of: panes | synced-panes | windows | browse
    #[serde(default = "default_layout")]
    pub layout: String,
    #[serde(default)]
    pub hosts: Vec<String>,
}

fn default_layout() -> String {
    "panes".to_string()
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Doc {
    #[serde(rename = "_meta", default)]
    pub meta: serde_yml::Value,
    #[serde(default)]
    pub groups: IndexMap<String, Group>,
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("tad")
}

pub fn config_path() -> PathBuf {
    config_dir().join("groups.yaml")
}

pub fn load() -> Result<Doc> {
    let path = config_path();
    if !path.exists() {
        return Ok(Doc::default());
    }
    let text = fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?;
    let doc: Doc = serde_yml::from_str(&text)
        .with_context(|| format!("parsing {}", path.display()))?;
    Ok(doc)
}

pub fn save(doc: &Doc) -> Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let mut out = Doc {
        meta: default_meta(),
        groups: doc.groups.clone(),
    };
    if !doc.meta.is_null() {
        out.meta = doc.meta.clone();
    }
    let text = serde_yml::to_string(&out)?;
    fs::write(&path, text)
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_dir_is_parent_of_config_path() {
        let dir = config_dir();
        let path = config_path();
        assert_eq!(path.parent().unwrap(), dir);
        assert!(dir.ends_with("tad"));
    }
}

fn default_meta() -> serde_yml::Value {
    let mut m = serde_yml::Mapping::new();
    m.insert(
        "description".into(),
        "Groups for `tad -g <name>`. Edit directly or via tad groups-{add,rm,edit}.".into(),
    );
    m.insert(
        "schema".into(),
        "groups.<name>.layout = panes|synced-panes|windows|browse  (default: panes)".into(),
    );
    m.insert(
        "schema_continued".into(),
        "groups.<name>.hosts = [fqdn, ...]".into(),
    );
    serde_yml::Value::Mapping(m)
}
