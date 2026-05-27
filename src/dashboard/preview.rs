//! Per-view preview-pane builders. Each `preview_X` returns the
//! Vec<Line> for the right-hand pane when a row in view X is selected.
//! Heavier than the row formatters — they may shell out to `tmux` or
//! `git`, which is fine for one-selection-at-a-time but would be wrong
//! per-row (hence the format.rs / preview.rs split).

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::agents;
use crate::projects;
use crate::snooze;
use crate::theme::Theme;
use crate::tmux;
use crate::transcript;

use super::format::{cwd_for_display, truncate};
use super::AppData;

pub(super) fn preview_project(data: &AppData, name: &str, theme: &Theme) -> Vec<Line<'static>> {
    let Some(p) = data.projects.iter().find(|pr| pr.name == name) else {
        return vec![Line::from(Span::styled(
            "project gone — refresh",
            Style::default().fg(theme.muted),
        ))];
    };
    let kv = |k: &str, v: String| -> Line<'static> {
        Line::from(vec![
            Span::styled(format!("{:<10}", k), Style::default().fg(theme.muted)),
            Span::styled(v, Style::default().fg(theme.fg)),
        ])
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled("project: ", Style::default().fg(theme.muted)),
            Span::styled(
                name.to_string(),
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(""),
        kv("root", p.root.display().to_string()),
    ];
    // Branch + dirty count: lazy subprocess, fine for the preview pane.
    if let Some(status) = projects::git_status(&p.root) {
        let dirty = if status.dirty == 0 {
            "clean".to_string()
        } else {
            format!("{} dirty", status.dirty)
        };
        lines.push(kv("branch", format!("{} · {}", status.branch, dirty)));
    }
    if !p.sessions.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("sessions ({})", p.sessions.len()),
            Style::default()
                .fg(theme.warning)
                .add_modifier(Modifier::BOLD),
        )));
        for s in &p.sessions {
            let marker = if s.attached { "● " } else { "  " };
            let marker_style = if s.attached {
                Style::default().fg(theme.success)
            } else {
                Style::default().fg(theme.muted)
            };
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(marker, marker_style),
                Span::styled(s.name.clone(), Style::default().fg(theme.fg)),
                Span::raw("  "),
                Span::styled(s.activity_str.clone(), Style::default().fg(theme.muted)),
            ]));
        }
    }
    if !p.agents.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("agents ({})", p.agents.len()),
            Style::default()
                .fg(theme.warning)
                .add_modifier(Modifier::BOLD),
        )));
        for a in &p.agents {
            let (marker, marker_style) = match a.attention {
                transcript::Attention::AwaitingInput => (
                    "! ",
                    Style::default()
                        .fg(theme.warning)
                        .add_modifier(Modifier::BOLD),
                ),
                transcript::Attention::Working => ("● ", Style::default().fg(theme.success)),
                transcript::Attention::Away => ("· ", Style::default().fg(theme.muted)),
                transcript::Attention::Unknown => ("· ", Style::default().fg(theme.muted)),
            };
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(marker, marker_style),
                Span::styled(a.target.clone(), Style::default().fg(theme.fg)),
            ]));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "↵ attach to most-recent session  (or jump to most-recent agent pane)",
        Style::default().fg(theme.muted),
    )));
    lines
}

pub(super) fn preview_session(data: &AppData, name: &str, theme: &Theme) -> Vec<Line<'static>> {
    // Per-pane breakdown so the preview shows what's actually running where,
    // not just a window count. We cross-reference data.agents so panes
    // hosting a claude process get a marker — the Sessions view now hints
    // at the Agents view rather than feeling disconnected from it.
    let panes_raw = tmux::run([
        "list-panes",
        "-t",
        name,
        "-aF",
        // session\twindow_idx\twindow_name\tpane_idx\tpane_current_command\tpane_current_path
        "#{session_name}\t#{window_index}\t#{window_name}\t#{pane_index}\t#{pane_current_command}\t#{pane_current_path}",
    ])
    .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
    .unwrap_or_default();
    let meta = tmux::run([
        "display-message",
        "-p",
        "-t",
        name,
        "created: #{t:session_created}\nactivity: #{t:session_activity}",
    ])
    .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
    .unwrap_or_default();
    let attached = tmux::run(["display-message", "-p", "-t", name, "#{session_attached}"])
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();

    let mut lines = vec![Line::from(vec![
        Span::styled("session: ", Style::default().fg(theme.muted)),
        Span::styled(
            name.to_string(),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
    ])];

    if attached != "0" && !attached.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("  attached by {} client(s)", attached),
            Style::default().fg(theme.success),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            "  detached".to_string(),
            Style::default().fg(theme.muted),
        )));
    }
    lines.push(Line::from(""));

    // Group panes by window for a sensible visual layout.
    let mut by_window: std::collections::BTreeMap<String, Vec<(String, String, String, String)>> =
        std::collections::BTreeMap::new();
    for line in panes_raw.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() != 6 {
            continue;
        }
        let window_key = format!("{:>3}: {}", parts[1], parts[2]);
        by_window.entry(window_key).or_default().push((
            parts[3].to_string(),
            parts[4].to_string(),
            parts[5].to_string(),
            format!("{}:{}.{}", parts[0], parts[1], parts[3]),
        ));
    }

    if by_window.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (no panes — session probably just died)".to_string(),
            Style::default().fg(theme.muted),
        )));
    }

    for (window_label, panes) in by_window {
        lines.push(Line::from(Span::styled(
            format!(
                "  {} ({} pane{})",
                window_label,
                panes.len(),
                if panes.len() == 1 { "" } else { "s" }
            ),
            Style::default()
                .fg(theme.warning)
                .add_modifier(Modifier::BOLD),
        )));
        for (pane_idx, cmd, cwd, target) in panes {
            let is_agent = data.agents.iter().any(|a| a.target == target);
            // Lozenge marks claude-hosting panes so the Sessions view
            // surfaces what the Agents view tracks, without an emoji.
            let marker = if is_agent { "◆ " } else { "  " };
            let marker_style = if is_agent {
                Style::default().fg(theme.success)
            } else {
                Style::default().fg(theme.muted)
            };
            let cwd_short = cwd_for_display(std::path::Path::new(&cwd));
            lines.push(Line::from(vec![
                Span::styled(
                    format!("    {}.", pane_idx),
                    Style::default().fg(theme.muted),
                ),
                Span::raw(" "),
                Span::styled(marker.to_string(), marker_style),
                Span::styled(
                    format!("{:<14}", truncate(&cmd, 14)),
                    Style::default().fg(theme.fg),
                ),
                Span::raw(" "),
                Span::styled(cwd_short, Style::default().fg(theme.muted)),
            ]));
        }
    }

    lines.push(Line::from(""));
    for l in meta.lines() {
        lines.push(Line::from(Span::styled(
            l.to_string(),
            Style::default().fg(theme.muted),
        )));
    }
    lines
}

pub(super) fn preview_group(data: &AppData, name: &str, theme: &Theme) -> Vec<Line<'static>> {
    let g = match data.groups.iter().find(|(n, _)| n == name) {
        Some((_, g)) => g,
        None => return vec![Line::from("?")],
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled("group: ", Style::default().fg(theme.muted)),
            Span::styled(
                name.to_string(),
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("layout: ", Style::default().fg(theme.muted)),
            Span::styled(g.layout.clone(), Style::default().fg(theme.warning)),
        ]),
        Line::from(Span::styled(
            format!("hosts ({}):", g.hosts.len()),
            Style::default().fg(theme.fg),
        )),
    ];
    for h in &g.hosts {
        lines.push(Line::from(Span::styled(
            format!("  {}", h),
            Style::default().fg(theme.fg),
        )));
    }
    lines
}

pub(super) fn preview_host(data: &AppData, name: &str, theme: &Theme) -> Vec<Line<'static>> {
    let in_groups: Vec<String> = data
        .hosts
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, g)| g.clone())
        .unwrap_or_default();
    let mut lines = vec![
        Line::from(vec![
            Span::styled("host: ", Style::default().fg(theme.muted)),
            Span::styled(
                name.to_string(),
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(Span::styled("member of:", Style::default().fg(theme.fg))),
    ];
    for g in &in_groups {
        lines.push(Line::from(Span::styled(
            format!("  {}", g),
            Style::default().fg(theme.fg),
        )));
    }
    lines
}

pub(super) fn preview_agent(data: &AppData, target: &str, theme: &Theme) -> Vec<Line<'static>> {
    let Some(agent) = data.agents.iter().find(|a| a.target == target) else {
        return vec![Line::from(Span::styled(
            "agent gone — refresh",
            Style::default().fg(theme.muted),
        ))];
    };
    let active_window = std::time::Duration::from_secs(30);
    let status = match agent.activity_status(active_window) {
        agents::ActivityStatus::Active(d) => {
            format!("● active ({})", agents::format_elapsed(d))
        }
        agents::ActivityStatus::Idle(d) => {
            format!("○ idle for {}", agents::format_elapsed(d))
        }
        agents::ActivityStatus::NoTranscript => "? no transcript on disk".to_string(),
    };
    let kv = |k: &str, v: String| -> Line<'static> {
        Line::from(vec![
            Span::styled(format!("{:<10}", k), Style::default().fg(theme.muted)),
            Span::styled(v, Style::default().fg(theme.fg)),
        ])
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled("agent: ", Style::default().fg(theme.muted)),
            Span::styled(
                target.to_string(),
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(""),
        kv("status", status),
        kv("session", agent.session.clone()),
        kv(
            "window",
            format!("{} ({})", agent.window_name, agent.window_index),
        ),
        kv("pane", agent.pane_index.clone()),
        kv("cwd", agent.cwd.display().to_string()),
        kv("pid", agent.agent_pid.to_string()),
        kv(
            "provider",
            crate::provider::by_id(agent.provider_id)
                .map(|p| p.label().to_string())
                .unwrap_or_else(|| agent.provider_id.to_string()),
        ),
    ];
    if let Some(tp) = &agent.transcript_path {
        let short = tp.file_name().map(|s| s.to_string_lossy().into_owned());
        if let Some(name) = short {
            lines.push(kv("transcript", name));
        }
    }
    // Surface an active snooze in the preview alongside the line badge,
    // so a user previewing a row knows the watcher is suppressing it.
    // Read from the per-refresh AppData cache, not the on-disk file.
    if let Some(until) = data.snoozes.snoozes.get(target) {
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if *until > now_secs {
            lines.push(kv(
                "snoozed",
                snooze::format_duration(std::time::Duration::from_secs(*until - now_secs)),
            ));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "↵ jump to pane   s snooze   S clear snooze".to_string(),
        Style::default().fg(theme.muted),
    )));
    lines
}
