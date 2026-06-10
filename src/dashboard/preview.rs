//! Per-view preview-pane builders. Each `preview_X` returns the
//! Vec<Line> for the right-hand pane when a row in view X is selected.
//! Heavier than the row formatters — they may shell out to `tmux` or
//! `git`, which is fine for one-selection-at-a-time but would be wrong
//! per-row (hence the format.rs / preview.rs split).

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::snooze;
use crate::theme::Theme;
use crate::tmux;

use super::format::{cwd_for_display, truncate};
use super::AppData;

/// How many trailing lines of the active pane the session preview shows.
const PANE_PREVIEW_LINES: usize = 12;

pub(super) fn preview_session(data: &AppData, name: &str, theme: &Theme) -> Vec<Line<'static>> {
    // Per-pane breakdown so the preview shows what's actually running where,
    // not just a window count. We cross-reference data.agents so panes
    // hosting a claude process get a marker — the Sessions view now hints
    // at the Agents view rather than feeling disconnected from it.
    let target = tmux::exact_target(name);
    let panes_raw = tmux::run([
        "list-panes",
        "-t",
        &target,
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
        &target,
        "created: #{t:session_created}\nactivity: #{t:session_activity}",
    ])
    .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
    .unwrap_or_default();
    let attached = tmux::run([
        "display-message",
        "-p",
        "-t",
        &target,
        "#{session_attached}",
    ])
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

    // Live look at the session's active pane so the user sees what's
    // on screen before attaching. capture-pane with a session target
    // captures that session's active pane.
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "── active pane ───────────────".to_string(),
        Style::default().fg(theme.border),
    )));
    let capture = tmux::run(["capture-pane", "-p", "-t", &target])
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
    match capture {
        Some(text) => {
            // Strip trailing blank lines first — a mostly-empty pane
            // would otherwise preview as 12 blank rows.
            let all: Vec<&str> = text.lines().collect();
            let end = all
                .iter()
                .rposition(|l| !l.trim().is_empty())
                .map(|i| i + 1)
                .unwrap_or(0);
            let shown = &all[end.saturating_sub(PANE_PREVIEW_LINES)..end];
            if shown.is_empty() {
                lines.push(Line::from(Span::styled(
                    "(pane is blank)".to_string(),
                    Style::default().fg(theme.muted),
                )));
            } else {
                for l in shown {
                    // capture-pane -p strips colours but not all control
                    // chars (\r, \x08, …) — drop them so ratatui doesn't
                    // render garbage; keep \t.
                    let clean: String = l
                        .chars()
                        .filter(|c| !c.is_control() || *c == '\t')
                        .collect();
                    lines.push(Line::from(Span::styled(
                        clean,
                        Style::default().fg(theme.muted),
                    )));
                }
            }
        }
        None => lines.push(Line::from(Span::styled(
            "no capture — pane gone?".to_string(),
            Style::default().fg(theme.muted),
        ))),
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
        .find(|r| r.name == name)
        .map(|r| r.groups.clone())
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
    if let Some(src) = data
        .hosts
        .iter()
        .find(|r| r.name == name)
        .map(|r| r.source.clone())
    {
        if !src.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("source: {}", src),
                Style::default().fg(theme.muted),
            )));
        }
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
    let (marker_text, marker_style, status_text, status_style) =
        super::format::agent_status(data, agent, theme);

    // Compact header: who, then status · cwd · [snoozed].
    let mut status_spans = vec![
        Span::styled(marker_text, marker_style),
        Span::styled(status_text, status_style),
        Span::styled(
            format!(" · {}", cwd_for_display(&agent.cwd)),
            Style::default().fg(theme.muted),
        ),
    ];
    if let Some(until) = data.snoozes.snoozes.get(target) {
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if *until > now_secs {
            status_spans.push(Span::styled(
                format!(
                    " · snoozed {}",
                    snooze::format_duration(std::time::Duration::from_secs(*until - now_secs))
                ),
                Style::default().fg(theme.warning),
            ));
        }
    }
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
        Line::from(status_spans),
        Line::from(""),
    ];

    // Body: the last thing the agent said. When awaiting input this is
    // naturally the question / approval it's blocked on. Falls back to
    // the old metadata card so the pane is never bare.
    // Build the fallback metadata card once; both no-tail arms use it.
    let push_card = |lines: &mut Vec<Line<'static>>, why: &str| {
        lines.push(Line::from(Span::styled(
            why.to_string(),
            Style::default().fg(theme.muted),
        )));
        lines.push(Line::from(""));
        let kv = |k: &str, v: String| -> Line<'static> {
            Line::from(vec![
                Span::styled(format!("{:<10}", k), Style::default().fg(theme.muted)),
                Span::styled(v, Style::default().fg(theme.fg)),
            ])
        };
        lines.push(kv("session", agent.session.clone()));
        lines.push(kv(
            "window",
            format!("{} ({})", agent.window_name, agent.window_index),
        ));
        lines.push(kv("pane", agent.pane_index.clone()));
        lines.push(kv("pid", agent.agent_pid.to_string()));
        lines.push(kv(
            "provider",
            crate::provider::by_id(agent.provider_id)
                .map(|p| p.label().to_string())
                .unwrap_or_else(|| agent.provider_id.to_string()),
        ));
    };
    let tail = agent
        .transcript_path
        .as_deref()
        .map(crate::transcript::last_assistant_text);
    match tail {
        Some(Some(text)) => {
            lines.push(Line::from(Span::styled(
                "── last message ──────────────".to_string(),
                Style::default().fg(theme.border),
            )));
            // str::lines() drops a trailing newline's empty element —
            // intentional; messages often end with a bare newline.
            for l in text.lines() {
                lines.push(Line::from(Span::styled(
                    l.to_string(),
                    Style::default().fg(theme.fg),
                )));
            }
        }
        // Transcript exists but no assistant text in the tail window.
        Some(None) => push_card(&mut lines, "no recent message"),
        // Agent has no transcript on disk at all.
        None => push_card(&mut lines, "no transcript — can't show last message"),
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "↵ jump to pane   s snooze   S clear snooze".to_string(),
        Style::default().fg(theme.muted),
    )));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dashboard::testutil::{mk_agent, mk_data};

    fn theme() -> crate::theme::Theme {
        crate::theme::load()
    }

    fn lines_text(lines: &[ratatui::text::Line<'static>]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn write_temp_transcript(content: &str) -> std::path::PathBuf {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!(
            "tad-preview-test-{}-{nanos}.jsonl",
            std::process::id()
        ));
        std::fs::write(&p, content).unwrap();
        p
    }

    #[test]
    fn agent_preview_shows_transcript_tail() {
        let path = write_temp_transcript(
            "{\"type\":\"assistant\",\"message\":{\"stop_reason\":\"end_turn\",\"content\":[{\"type\":\"text\",\"text\":\"shall I deploy?\"}]}}\n",
        );
        let mut a = mk_agent("work:1.0", "work", 100);
        a.transcript_path = Some(path.clone());
        let data = mk_data(vec![], vec![a]);
        let text = lines_text(&preview_agent(&data, "work:1.0", &theme()));
        assert!(text.contains("last message"));
        assert!(text.contains("shall I deploy?"));
        // The metadata card is gone in the tail case.
        assert!(!text.contains("provider"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn agent_preview_without_transcript_falls_back_to_card() {
        let data = mk_data(vec![], vec![mk_agent("work:1.0", "work", 100)]);
        let text = lines_text(&preview_agent(&data, "work:1.0", &theme()));
        assert!(text.contains("no transcript"));
        // Card facts still present so the pane isn't bare.
        assert!(text.contains("session"));
        assert!(text.contains("pid"));
    }

    #[test]
    fn agent_preview_with_empty_transcript_says_no_recent_message() {
        let path = write_temp_transcript("{\"type\":\"agent-name\",\"name\":\"alpha\"}\n");
        let mut a = mk_agent("work:1.0", "work", 100);
        a.transcript_path = Some(path.clone());
        let data = mk_data(vec![], vec![a]);
        let text = lines_text(&preview_agent(&data, "work:1.0", &theme()));
        assert!(text.contains("no recent message"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn agent_preview_gone_agent_degrades() {
        let data = mk_data(vec![], vec![]);
        let text = lines_text(&preview_agent(&data, "nope:0.0", &theme()));
        assert!(text.contains("gone"));
    }
}
