//! Group CRUD and `tad -g` open behavior.

use anyhow::{bail, Result};
use std::os::unix::process::CommandExt;
use std::process::Command;

use crate::{config, sessions, tmux};

const LAYOUTS: &[&str] = &["panes", "synced-panes", "windows", "browse"];

pub fn print_index() -> Result<()> {
    let doc = config::load()?;
    let mut names: Vec<_> = doc.groups.keys().cloned().collect();
    names.sort();
    for n in names {
        let g = &doc.groups[&n];
        println!("{}:{} hosts  ·  {}", n, g.hosts.len(), g.layout);
    }
    Ok(())
}

pub fn print_hosts(group: &str) -> Result<()> {
    let doc = config::load()?;
    if let Some(g) = doc.groups.get(group) {
        for h in &g.hosts {
            println!("{}", h);
        }
    }
    Ok(())
}

pub fn add(name: &str, layout: &str, hosts: &[String]) -> Result<i32> {
    if !LAYOUTS.contains(&layout) {
        bail!("layout must be one of: {}", LAYOUTS.join(", "));
    }
    let mut doc = config::load()?;
    if doc.groups.contains_key(name) {
        bail!(
            "group {} exists (use `tad groups edit` or `tad groups rm` first)",
            name
        );
    }
    doc.groups.insert(
        name.to_string(),
        config::Group {
            layout: layout.to_string(),
            hosts: hosts.to_vec(),
        },
    );
    config::save(&doc)?;
    println!("added group: {} ({}, {} hosts)", name, layout, hosts.len());
    Ok(0)
}

/// Interactive group-add wizard. Prompts for name, layout, and hosts.
pub fn add_interactive() -> Result<i32> {
    use inquire::{Select, Text};

    let existing = config::load()?;
    let name = Text::new("Group name:")
        .with_validator(move |s: &str| {
            if s.trim().is_empty() {
                Ok(inquire::validator::Validation::Invalid("required".into()))
            } else if existing.groups.contains_key(s.trim()) {
                Ok(inquire::validator::Validation::Invalid(
                    "group already exists".into(),
                ))
            } else {
                Ok(inquire::validator::Validation::Valid)
            }
        })
        .prompt()?;
    let name = name.trim().to_string();

    let layout = Select::new("Layout:", LAYOUTS.iter().map(|s| s.to_string()).collect())
        .with_starting_cursor(0)
        .with_help_message(
            "panes=tiled split (text-sync prompt, default off in scripts), synced-panes=same (default on in scripts), windows=one per host, browse=list-only",
        )
        .prompt()?;

    let hosts_str = Text::new("Hosts (comma or space separated, FQDN or short):")
        .with_validator(|s: &str| {
            if s.trim().is_empty() {
                Ok(inquire::validator::Validation::Invalid(
                    "at least one host required".into(),
                ))
            } else {
                Ok(inquire::validator::Validation::Valid)
            }
        })
        .prompt()?;
    let hosts: Vec<String> = hosts_str
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();

    add(&name, &layout, &hosts)
}

pub fn remove(name: &str) -> Result<i32> {
    let mut doc = config::load()?;
    if doc.groups.shift_remove(name).is_none() {
        bail!("no such group: {}", name);
    }
    config::save(&doc)?;
    println!("removed group: {}", name);
    Ok(0)
}

pub fn edit() -> Result<i32> {
    let path = config::config_path();
    if !path.exists() {
        // Ensure the file exists so $EDITOR has something to open.
        config::save(&config::Doc::default())?;
    }
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vim".to_string());
    let err = Command::new(&editor).arg(&path).exec();
    Err(err.into())
}

/// `tad -g GROUP [HOST]`.
pub fn open(name: &str, host: Option<&str>) -> Result<i32> {
    let doc = config::load()?;
    let g = doc
        .groups
        .get(name)
        .ok_or_else(|| anyhow::anyhow!("no such group: {}", name))?;

    if let Some(h) = host {
        if !g.hosts.iter().any(|x| x == h) {
            bail!("host {} not in group {}", h, name);
        }
        return sessions::attach_or_create_remote(h);
    }

    if g.layout == "browse" {
        for h in &g.hosts {
            println!("{}", h);
        }
        return Ok(0);
    }

    if tmux::has_session(name) {
        return tmux::enter(name);
    }
    if g.hosts.is_empty() {
        bail!("group {} has no hosts", name);
    }

    let first = &g.hosts[0];
    let first_short = short_host(first);
    tmux::new_session_detached(name, &first_short, &format!("ssh {}", first))?;

    match g.layout.as_str() {
        "panes" | "synced-panes" => {
            for h in &g.hosts[1..] {
                tmux::split_window(name, &format!("ssh {}", h))?;
            }
            tmux::select_layout(name, "tiled")?;
            // With 2+ panes, default to enabling text-sync but let the user
            // disable it interactively. Without a tty, fall back to the
            // stored layout's intent so scripted invocations stay deterministic.
            let layout_default = g.layout == "synced-panes";
            let want_sync = if g.hosts.len() > 1 {
                sessions::confirm_tty("Enable text-sync across panes?", true)
                    .unwrap_or(layout_default)
            } else {
                layout_default
            };
            if want_sync {
                tmux::set_window_option(name, "synchronize-panes", "on")?;
            }
        }
        "windows" => {
            for h in &g.hosts[1..] {
                let sh = short_host(h);
                tmux::new_window(name, &sh, &format!("ssh {}", h))?;
            }
            tmux::select_first_window(name)?;
        }
        other => bail!("unsupported layout: {}", other),
    }

    tmux::enter(name)
}

fn short_host(fqdn: &str) -> String {
    fqdn.split('.').next().unwrap_or(fqdn).to_string()
}
