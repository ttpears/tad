//! fzf-driven dashboard. State lives in /tmp/tad-dashboard.state so that
//! reload bindings (cycle / preview / prompt) can share it.

use anyhow::{anyhow, Context, Result};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::{config, groups, sessions, tmux};

pub const VIEWS: [&str; 3] = ["sessions", "groups", "hosts"];

fn state_path() -> PathBuf {
    PathBuf::from(format!(
        "/tmp/tad-dashboard-{}.state",
        std::env::var("USER").unwrap_or_else(|_| "shared".to_string())
    ))
}

pub fn read_state() -> String {
    fs::read_to_string(state_path())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| VIEWS.contains(&s.as_str()))
        .unwrap_or_else(|| "sessions".to_string())
}

fn write_state(view: &str) {
    let _ = fs::write(state_path(), view);
}

pub fn source(view: &str) -> Result<()> {
    match view {
        "sessions" => emit_sessions(),
        "groups" => emit_groups(),
        "hosts" => emit_hosts(),
        other => Err(anyhow!("unknown dashboard view: {}", other)),
    }
}

fn emit_sessions() -> Result<()> {
    let mut sessions = sessions::list()?;
    sessions.sort_by(|a, b| b.activity_ts.cmp(&a.activity_ts));
    let home = std::env::var("HOME").unwrap_or_default();
    for s in &sessions {
        let marker = if s.attached { "●" } else { " " };
        let win = truncate(if s.active_window.is_empty() {
            "—"
        } else {
            &s.active_window
        }, 10);
        let mut path = s.active_path.clone();
        if !home.is_empty() && path.starts_with(&home) {
            path = format!("~{}", &path[home.len()..]);
        }
        let path = truncate(&path, 38);
        let display = format!(
            "{} {:<20} {:>3}w  {:<10}  {:<38}  {:>8}",
            marker, truncate(&s.name, 20), s.windows, win, path, s.activity_str
        );
        println!("sessions\t{}\t{}", s.name, display);
    }
    Ok(())
}

fn emit_groups() -> Result<()> {
    let doc = config::load()?;
    let mut names: Vec<_> = doc.groups.keys().cloned().collect();
    names.sort();
    for name in names {
        let g = &doc.groups[&name];
        let display = format!(
            "{:<28} {:>3} hosts  · {}",
            truncate(&name, 28),
            g.hosts.len(),
            g.layout
        );
        println!("groups\t{}\t{}", name, display);
    }
    Ok(())
}

fn emit_hosts() -> Result<()> {
    let doc = config::load()?;
    let mut by_host: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();
    for (gname, g) in &doc.groups {
        for h in &g.hosts {
            by_host.entry(h.clone()).or_default().push(gname.clone());
        }
    }
    for (host, in_groups) in by_host {
        let mut g = in_groups;
        g.sort();
        let in_str = truncate(&g.join(", "), 40);
        let display = format!("{:<50} in: {}", truncate(&host, 50), in_str);
        println!("hosts\t{}\t{}", host, display);
    }
    Ok(())
}

pub fn cycle() -> Result<()> {
    let cur = read_state();
    let idx = VIEWS.iter().position(|v| v == &cur).unwrap_or(0);
    let next = VIEWS[(idx + 1) % VIEWS.len()];
    write_state(next);
    source(next)
}

pub fn prompt() {
    print!("{}> ", read_state());
}

pub fn header() {
    let cur = read_state();
    let mut bits = Vec::new();
    for v in VIEWS {
        bits.push(if v == cur {
            format!("[ {} ]", v)
        } else {
            format!("  {}  ", v)
        });
    }
    println!(
        "{}\n  tab: cycle view  ·  enter: open  ·  ctrl-k: kill (sessions)  ·  ctrl-r: reload  ·  esc: quit",
        bits.join("  ")
    );
}

pub fn preview(view: &str, name: &str) -> Result<()> {
    match view {
        "sessions" => {
            let target = tmux::exact_target(name);
            let out = tmux::run([
                "list-windows",
                "-t",
                &target,
                "-F",
                "  #{window_index}: #{window_name}  (#{window_panes} panes)",
            ])?;
            println!("session: {}\n", name);
            std::io::stdout().write_all(&out.stdout).ok();
            let out = tmux::run([
                "display-message",
                "-p",
                "-t",
                &target,
                "created: #{t:session_created}\nactivity: #{t:session_activity}",
            ])?;
            println!();
            std::io::stdout().write_all(&out.stdout).ok();
        }
        "groups" => {
            let doc = config::load()?;
            if let Some(g) = doc.groups.get(name) {
                println!("group: {}\nlayout: {}\nhosts ({}):", name, g.layout, g.hosts.len());
                for h in &g.hosts {
                    println!("  {}", h);
                }
            }
        }
        "hosts" => {
            let doc = config::load()?;
            let in_groups: Vec<&String> = doc
                .groups
                .iter()
                .filter(|(_, g)| g.hosts.iter().any(|h| h == name))
                .map(|(gn, _)| gn)
                .collect();
            println!("host: {}\nmember of:", name);
            for gn in in_groups {
                println!("  {}", gn);
            }
        }
        _ => {}
    }
    Ok(())
}

pub fn kill(view: &str, name: &str) {
    if view == "sessions" {
        tmux::kill_session(name);
    }
}

pub fn run() -> Result<i32> {
    if !in_path("fzf") {
        return Err(anyhow!("fzf not installed"));
    }
    write_state("sessions");
    let exe = std::env::current_exe().context("getting current_exe")?;
    let exe_s = exe.to_string_lossy().to_string();
    let header_text = format!(
        "[ sessions ]    groups     hosts  \n  tab: cycle view  ·  enter: open  ·  ctrl-k: kill (sessions)  ·  ctrl-r: reload  ·  esc: quit"
    );
    let tab_bind = format!(
        "tab:reload({0} dash-cycle)+transform-prompt({0} dash-prompt)+transform-header({0} dash-header)",
        exe_s
    );
    let reload_bind = format!(
        "ctrl-r:reload({0} dash-source $({0} dash-state))",
        exe_s
    );
    let kill_bind = format!(
        "ctrl-k:execute-silent({0} dash-kill {{1}} {{2}})+reload({0} dash-source $({0} dash-state))",
        exe_s
    );
    let preview_cmd = format!("{} dash-preview {{1}} {{2}}", exe_s);

    let initial = Command::new(&exe)
        .args(["dash-source", "sessions"])
        .output()
        .context("emitting initial source")?
        .stdout;

    let mut child = Command::new("fzf")
        .args([
            "--ansi",
            "--reverse",
            "--height=80%",
            "--border=rounded",
            "--delimiter=\t",
            "--with-nth=3..",
            "--nth=2..",
            "--prompt=sessions> ",
            "--header",
            &header_text,
            "--preview",
            &preview_cmd,
            "--preview-window=right:50%:wrap",
            "--bind",
            &tab_bind,
            "--bind",
            &reload_bind,
            "--bind",
            &kill_bind,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("spawning fzf")?;
    {
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(&initial).ok();
    }
    let output = child.wait_with_output()?;
    if !output.status.success() || output.stdout.is_empty() {
        return Ok(1);
    }
    let line = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let parts: Vec<&str> = line.splitn(3, '\t').collect();
    if parts.len() < 2 {
        return Ok(1);
    }
    match parts[0] {
        "sessions" => sessions::attach_or_create(parts[1]),
        "groups" => groups::open(parts[1], None),
        "hosts" => sessions::attach_or_create_remote(parts[1]),
        _ => Ok(1),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

fn in_path(cmd: &str) -> bool {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .any(|p| p.join(cmd).is_file())
}
