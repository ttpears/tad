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
    // `-s` scopes to every pane in the targeted session. The previous
    // `-a` listed every pane on the SERVER (ignoring -t), so with 2+
    // sessions the breakdown showed other sessions' windows too.
    let panes_raw = tmux::run([
        "list-panes",
        "-s",
        "-t",
        &target,
        "-F",
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
            let shown = tail_window(&all, PANE_PREVIEW_LINES);
            if shown.is_empty() {
                lines.push(Line::from(Span::styled(
                    "(pane is blank)".to_string(),
                    Style::default().fg(theme.muted),
                )));
            } else {
                for l in shown {
                    lines.push(Line::from(Span::styled(
                        clean_capture_line(l),
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

    // Body: a live look at the agent's OWN pane. transcript_path is
    // discovered per-CWD (agents::scan calls provider.latest_transcript
    // per cwd), so agents sharing a cwd — the common case for two
    // panes in one tmux session — resolved to the same newest
    // transcript file and previewed identically. The pane is the only
    // source that's always unique per agent, since agent.target
    // (`session:window.pane`) is unique by construction. Falls back to
    // the metadata card so the pane is never bare when capture fails
    // (e.g. the pane vanished between scan and render).
    let capture = tmux::run(["capture-pane", "-p", "-t", &agent.target])
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
    match capture {
        Some(text) => {
            lines.push(Line::from(Span::styled(
                "── pane ──────────────────────".to_string(),
                Style::default().fg(theme.border),
            )));
            let all: Vec<&str> = text.lines().collect();
            let shown = tail_window(&all, PANE_PREVIEW_LINES);
            if shown.is_empty() {
                lines.push(Line::from(Span::styled(
                    "(pane is blank)".to_string(),
                    Style::default().fg(theme.muted),
                )));
            } else {
                for l in shown {
                    lines.push(Line::from(Span::styled(
                        clean_capture_line(l),
                        Style::default().fg(theme.fg),
                    )));
                }
            }
        }
        None => push_metadata_card(&mut lines, agent, "no capture — pane gone?", theme),
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "↵ jump to pane   s snooze   S clear snooze".to_string(),
        Style::default().fg(theme.muted),
    )));
    lines
}

/// Make one line of `capture-pane -p` output safe for ratatui.
/// capture-pane strips colours but returns the pane's cells verbatim:
/// real tabs survive (ratatui renders them as garbage — expand to
/// spaces) and other control chars (\r, \x08, \x07, …) are dropped.
fn clean_capture_line(l: &str) -> String {
    l.replace('\t', "    ")
        .chars()
        .filter(|c| !c.is_control())
        .collect()
}

/// Trailing window of a `capture-pane` dump: trims trailing blank
/// lines first (a mostly-empty pane would otherwise preview as `max`
/// blank rows), then keeps at most `max` of what's left. Pure so it's
/// testable without a real tmux pane.
fn tail_window<'a>(all: &'a [&'a str], max: usize) -> &'a [&'a str] {
    let end = all
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .map(|i| i + 1)
        .unwrap_or(0);
    &all[end.saturating_sub(max)..end]
}

/// Metadata card shown in place of a live pane capture — when the
/// pane vanished (target gone) there's nothing else to show. Pure
/// (no tmux) so it's directly testable.
fn push_metadata_card(
    lines: &mut Vec<Line<'static>>,
    agent: &crate::agents::Agent,
    why: &str,
    theme: &Theme,
) {
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

    #[test]
    fn agent_preview_gone_agent_degrades() {
        // Early-return path — no agent found, so preview_agent never
        // reaches the pane-capture code and stays tmux-free.
        let data = mk_data(vec![], vec![]);
        let text = lines_text(&preview_agent(&data, "nope:0.0", &theme()));
        assert!(text.contains("gone"));
    }

    // preview_agent's live-pane body (like preview_session's) shells out
    // to tmux and can't be exercised in a hermetic test. Its two extracted
    // pure pieces — the tail-window trim and the metadata fallback card —
    // are tested directly below instead.

    #[test]
    fn tail_window_trims_trailing_blanks_before_capping() {
        let all = ["one", "two", "three", "", ""];
        // Trailing blanks are dropped first, then capped to `max` — a
        // mostly-empty pane must not preview as rows of blank lines.
        assert_eq!(tail_window(&all, 2), ["two", "three"]);
    }

    #[test]
    fn tail_window_keeps_everything_under_the_cap() {
        let all = ["only one line"];
        assert_eq!(tail_window(&all, 12), ["only one line"]);
    }

    #[test]
    fn tail_window_all_blank_is_empty() {
        let all = ["", "  ", ""];
        assert!(tail_window(&all, 12).is_empty());
    }

    #[test]
    fn metadata_card_shows_agent_facts_and_reason() {
        let a = mk_agent("work:1.0", "work", 100);
        let mut lines = vec![];
        push_metadata_card(&mut lines, &a, "no capture — pane gone?", &theme());
        let text = lines_text(&lines);
        assert!(text.contains("no capture"));
        assert!(text.contains("session"));
        assert!(text.contains("pid"));
        assert!(text.contains("provider"));
    }

    /// Real tabs survive capture-pane (tmux returns the pane's cells
    /// verbatim) and ratatui renders them as garbage — they must be
    /// expanded to spaces, not kept.
    #[test]
    fn capture_line_expands_tabs_to_spaces() {
        assert_eq!(clean_capture_line("a\tb"), "a    b");
    }

    #[test]
    fn capture_line_strips_other_control_chars() {
        assert_eq!(clean_capture_line("a\rb\x08c\x07d"), "abcd");
    }

    #[test]
    fn capture_line_passes_plain_text_through() {
        assert_eq!(clean_capture_line("plain · text"), "plain · text");
    }
}
