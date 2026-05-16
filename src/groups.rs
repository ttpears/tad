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
            "group {} exists (use groups-edit or groups-rm first)",
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
            if g.layout == "synced-panes" {
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
