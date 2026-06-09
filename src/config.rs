//! Unified config IO. Lives at `~/.config/tad/config.yaml` with a
//! `theme:` / `ui:` / `groups:` section layout. `theme:` is owned by
//! `theme.rs` and read independently; `groups:` is what this module
//! exposes as [`Doc`] for the rest of the crate.
//!
//! Pre-v0.10 layouts kept groups in a separate `groups.yaml`. On startup
//! [`migrate_if_needed`] folds that file into `config.yaml` (preserving
//! whatever `theme:` was already there) and renames the legacy file to
//! `groups.yaml.migrated` so the migration is one-shot and reversible.
//!
//! Save/load operate as read-modify-write at the YAML level so writing
//! `groups:` never clobbers a user's `theme:` overrides or any future
//! `ui:` section (auto-popup knob, last-view, etc.).
//!
//! NOTE: `Doc` intentionally only carries `_meta` and `groups`. Callers
//! that care about the unified config (theme.rs, future ui prefs)
//! should read those keys directly via `serde_yml`; this module's
//! contract is just "give me/take from me the groups section."

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

/// The groups slice of the unified config, plus an optional `_meta`
/// block (description/schema hints inlined in the YAML for users who
/// hand-edit). Doesn't carry theme/ui — those live in the same file
/// but are owned by other modules.
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Doc {
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "serde_yml::Value::is_null"
    )]
    pub meta: serde_yml::Value,
    #[serde(default)]
    pub groups: IndexMap<String, Group>,
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("tad")
}

/// The single unified config file. (Pre-v0.10 there was a separate
/// `groups.yaml`; see [`legacy_groups_path`] and [`migrate_if_needed`].)
pub fn config_path() -> PathBuf {
    config_dir().join("config.yaml")
}

fn legacy_groups_path() -> PathBuf {
    config_dir().join("groups.yaml")
}

/// Load the groups + meta from `config.yaml`. Other top-level keys
/// (`theme:`, `ui:`, …) are present in the file but ignored here.
/// Missing file → empty Doc.
pub fn load() -> Result<Doc> {
    let path = config_path();
    if !path.exists() {
        return Ok(Doc::default());
    }
    let text = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    if text.trim().is_empty() {
        return Ok(Doc::default());
    }
    let doc: Doc =
        serde_yml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    Ok(doc)
}

/// Write groups + meta back to `config.yaml`, preserving every other
/// top-level key (theme, ui, …) exactly as it was on disk.
pub fn save(doc: &Doc) -> Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }

    // Read existing file as a raw mapping so we don't lose theme/ui keys.
    let mut root: serde_yml::Mapping = match fs::read_to_string(&path) {
        Ok(text) if !text.trim().is_empty() => serde_yml::from_str::<serde_yml::Value>(&text)
            .with_context(|| format!("parsing {}", path.display()))?
            .as_mapping()
            .cloned()
            .unwrap_or_default(),
        _ => serde_yml::Mapping::new(),
    };

    // Replace _meta (use the file's _meta if the caller didn't supply one;
    // otherwise the caller's value wins; otherwise our default).
    let meta = if !doc.meta.is_null() {
        doc.meta.clone()
    } else {
        default_meta()
    };
    root.insert("_meta".into(), meta);

    // Replace groups.
    let groups_val = serde_yml::to_value(&doc.groups)?;
    root.insert("groups".into(), groups_val);

    let text = serde_yml::to_string(&serde_yml::Value::Mapping(root))?;
    fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

fn default_meta() -> serde_yml::Value {
    let mut m = serde_yml::Mapping::new();
    m.insert(
        "description".into(),
        "Tad config. Edit directly, or `tad config` (TUI), or `tad groups <add|rm|edit>`.".into(),
    );
    m.insert(
        "schema".into(),
        "groups.<name>.layout = panes|synced-panes|windows|browse  (default: panes)".into(),
    );
    m.insert(
        "schema_continued".into(),
        "groups.<name>.hosts = [fqdn, ...]".into(),
    );
    m.insert(
        "discovery_1".into(),
        "discovery.min_history_uses = N  (history-only hosts below N uses are hidden; default 2)".into(),
    );
    m.insert(
        "discovery_2".into(),
        "discovery.shell_history|ssh_config|known_hosts = true|false  (toggle sources; default true)".into(),
    );
    serde_yml::Value::Mapping(m)
}

/// Pre-v0.10 → unified-config migration. Idempotent. Fire-and-forget
/// from main(): errors print to stderr and we continue with whatever
/// state we have.
///
/// Migrates iff:
///   1. legacy `groups.yaml` exists
///   2. unified `config.yaml` either doesn't exist, or has no `groups:` key
///
/// After migration the legacy file is renamed to `groups.yaml.migrated`
/// so re-runs are no-ops and the user can verify / roll back.
pub fn migrate_if_needed() {
    if let Err(e) = try_migrate() {
        eprintln!("tad: config migration failed ({e:#}); continuing with current state");
    }
}

fn try_migrate() -> Result<()> {
    let legacy = legacy_groups_path();
    if !legacy.exists() {
        return Ok(());
    }

    // If config.yaml already has groups, the user's already on the new
    // layout — leave the legacy file alone (it'll be reviewed by hand).
    let unified = config_path();
    if unified.exists() {
        if let Ok(existing) = load() {
            if !existing.groups.is_empty() {
                return Ok(());
            }
        }
    }

    let legacy_text =
        fs::read_to_string(&legacy).with_context(|| format!("reading {}", legacy.display()))?;
    if legacy_text.trim().is_empty() {
        // Empty legacy file — just rename it so we don't keep checking.
        let migrated = legacy.with_extension("yaml.migrated");
        let _ = fs::rename(&legacy, &migrated);
        return Ok(());
    }

    let legacy_doc: Doc = serde_yml::from_str(&legacy_text)
        .with_context(|| format!("parsing legacy {}", legacy.display()))?;

    // Write the groups into unified config.yaml, preserving any existing
    // theme/ui keys.
    save(&legacy_doc)?;

    let migrated = legacy.with_extension("yaml.migrated");
    fs::rename(&legacy, &migrated)
        .with_context(|| format!("renaming {} → {}", legacy.display(), migrated.display()))?;

    eprintln!(
        "tad: migrated {} → {} ({} groups). Old file kept as {}.",
        legacy.display(),
        unified.display(),
        legacy_doc.groups.len(),
        migrated.display(),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::sync::Mutex;

    /// All tests in this module that mutate `$XDG_CONFIG_HOME` lock this
    /// mutex. Otherwise cargo's parallel test runner has them racing on a
    /// shared process-global env var (intermittently observed: two tests
    /// see each other's temp dirs and the "no legacy file" test trips on
    /// a legacy file written by the other test).
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn config_dir_is_parent_of_config_path() {
        let dir = config_dir();
        let path = config_path();
        assert_eq!(path.parent().unwrap(), dir);
        assert!(dir.ends_with("tad"));
    }

    #[test]
    fn config_path_is_config_yaml_not_groups_yaml() {
        assert_eq!(
            config_path().file_name().and_then(|s| s.to_str()),
            Some("config.yaml")
        );
    }

    /// Drive save/load against a temp `$XDG_CONFIG_HOME` and assert that:
    ///   1. a pre-existing `theme:` key survives a `save(doc with groups)`
    ///   2. `load()` returns just the groups (theme is invisible to this module)
    #[test]
    fn save_preserves_sibling_theme_key() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempdir();
        env::set_var("XDG_CONFIG_HOME", tmp.path());

        let conf = config_path();
        fs::create_dir_all(conf.parent().unwrap()).unwrap();
        fs::write(&conf, "theme: dracula\n").unwrap();

        let mut groups = IndexMap::new();
        groups.insert(
            "prod".to_string(),
            Group {
                layout: "panes".to_string(),
                hosts: vec!["a.example".to_string(), "b.example".to_string()],
            },
        );
        save(&Doc {
            meta: serde_yml::Value::Null,
            groups,
        })
        .unwrap();

        let final_text = fs::read_to_string(&conf).unwrap();
        assert!(
            final_text.contains("dracula"),
            "theme: dracula was lost!\nfile:\n{}",
            final_text
        );
        assert!(final_text.contains("prod"));

        let loaded = load().unwrap();
        assert_eq!(loaded.groups.len(), 1);
        assert!(loaded.groups.contains_key("prod"));

        env::remove_var("XDG_CONFIG_HOME");
    }

    /// migrate_if_needed: legacy groups.yaml present, no unified config →
    /// content lands in config.yaml and legacy file is renamed.
    #[test]
    fn migrate_moves_legacy_groups_into_config_yaml() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempdir();
        env::set_var("XDG_CONFIG_HOME", tmp.path());

        let legacy = legacy_groups_path();
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        fs::write(
            &legacy,
            "groups:\n  legacy-group:\n    layout: windows\n    hosts: [h1, h2, h3]\n",
        )
        .unwrap();

        try_migrate().unwrap();

        let conf = config_path();
        assert!(conf.exists(), "config.yaml should have been created");
        let text = fs::read_to_string(&conf).unwrap();
        assert!(text.contains("legacy-group"));
        assert!(text.contains("windows"));

        assert!(!legacy.exists(), "legacy file should be renamed");
        assert!(
            legacy.with_extension("yaml.migrated").exists(),
            "legacy file should be renamed to .migrated"
        );

        env::remove_var("XDG_CONFIG_HOME");
    }

    /// migrate_if_needed: unified config.yaml already has groups →
    /// migration is a no-op; legacy file stays put for user review.
    #[test]
    fn migrate_is_noop_when_unified_already_has_groups() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempdir();
        env::set_var("XDG_CONFIG_HOME", tmp.path());

        let legacy = legacy_groups_path();
        let conf = config_path();
        fs::create_dir_all(conf.parent().unwrap()).unwrap();
        fs::write(&legacy, "groups:\n  oldname:\n    hosts: [h]\n").unwrap();
        fs::write(
            &conf,
            "theme: nord\ngroups:\n  newname:\n    hosts: [a, b]\n",
        )
        .unwrap();

        try_migrate().unwrap();

        // legacy should still be there
        assert!(legacy.exists(), "legacy should not be renamed");
        // unified config should be unchanged
        let text = fs::read_to_string(&conf).unwrap();
        assert!(text.contains("newname"));
        assert!(!text.contains("oldname"));

        env::remove_var("XDG_CONFIG_HOME");
    }

    /// migrate_if_needed: no legacy file → no-op.
    #[test]
    fn migrate_is_noop_when_no_legacy_file() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempdir();
        env::set_var("XDG_CONFIG_HOME", tmp.path());

        try_migrate().unwrap();
        assert!(!config_path().exists());

        env::remove_var("XDG_CONFIG_HOME");
    }

    /// Minimal in-test tempdir helper so we don't pull in `tempfile` as a
    /// dep. Path is deleted on Drop.
    struct TmpDir(PathBuf);
    impl TmpDir {
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }
    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn tempdir() -> TmpDir {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let pid = std::process::id();
        let p = env::temp_dir().join(format!("tad-config-test-{pid}-{nanos}"));
        fs::create_dir_all(&p).unwrap();
        TmpDir(p)
    }
}
