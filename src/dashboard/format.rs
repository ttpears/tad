//! Per-view row formatters for the dashboard's main list, plus a few
//! shared display helpers (cwd → `~/…`, fixed-width truncation, FQDN
//! → short tmux-friendly name). Pure functions over `&AppData`; no
//! side effects, called once per visible row per render frame.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::agents;
use crate::snooze;
use crate::theme::Theme;
use crate::transcript;

use super::AppData;

pub(super) fn format_session_line(data: &AppData, name: &str, theme: &Theme) -> Line<'static> {
    let s = match data.sessions.iter().find(|s| s.name == name) {
        Some(s) => s,
        None => return Line::from(name.to_string()),
    };
    let marker = if s.attached {
        Span::styled("● ", Style::default().fg(theme.success))
    } else {
        Span::raw("  ")
    };
    Line::from(vec![
        marker,
        Span::styled(
            format!("{:<22}", truncate(&s.name, 22)),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{:>3}w  ", s.windows),
            Style::default().fg(theme.warning),
        ),
        Span::styled(
            format!("{:<12}", truncate(&s.active_window, 12)),
            Style::default().fg(theme.fg),
        ),
        Span::raw(" "),
        Span::styled(s.activity_str.clone(), Style::default().fg(theme.muted)),
    ])
}

pub(super) fn format_group_line(data: &AppData, name: &str, theme: &Theme) -> Line<'static> {
    let g = match data.groups.iter().find(|(n, _)| n == name) {
        Some((_, g)) => g,
        None => return Line::from(name.to_string()),
    };
    Line::from(vec![
        Span::styled(
            format!("{:<28}", truncate(name, 28)),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{:>3} hosts  ", g.hosts.len()),
            Style::default().fg(theme.warning),
        ),
        Span::styled(g.layout.clone(), Style::default().fg(theme.muted)),
    ])
}

pub(super) fn format_host_line(data: &AppData, name: &str, theme: &Theme) -> Line<'static> {
    let row = data.hosts.iter().find(|r| r.name == name);
    let groups = row.map(|r| r.groups.join(", ")).unwrap_or_default();
    let source = row.map(|r| r.source.clone()).unwrap_or_default();
    let trailing = if groups.is_empty() {
        source
    } else if source.is_empty() {
        groups
    } else {
        format!("{}  ·  {}", groups, source)
    };
    Line::from(vec![
        Span::styled(
            format!("{:<45}", truncate(name, 45)),
            Style::default().fg(theme.accent),
        ),
        Span::raw("  "),
        Span::styled(trailing, Style::default().fg(theme.muted)),
    ])
}

pub(super) fn format_agent_line(data: &AppData, target: &str, theme: &Theme) -> Line<'static> {
    // Session-header rows (emitted by the Agents view's grouped items
    // list) are visually distinct dividers — bold session name in the
    // accent colour, with the inline count summary. The sigil isn't
    // shown; it's just an in-band marker for `is_agent_header`.
    if let Some(rest) = target.strip_prefix(super::AGENT_HEADER_SIGIL) {
        let label = rest.trim_start();
        return Line::from(vec![
            Span::styled("── ", Style::default().fg(theme.border)),
            Span::styled(
                label.to_string(),
                Style::default()
                    .fg(theme.accent_bold)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" ─────────────", Style::default().fg(theme.border)),
        ]);
    }
    let Some(agent) = data.agents.iter().find(|a| a.target == target) else {
        return Line::from(target.to_string());
    };
    let active_window = std::time::Duration::from_secs(30);
    // Prefer the precise transcript signal when we have one — that's
    // the "is this agent actually waiting for me right now" answer,
    // independent of mtime. Fall back to mtime otherwise.
    let (marker_text, marker_style, status_text, status_style) = match agent.attention {
        transcript::Attention::AwaitingInput => {
            // Distinguish fresh-waiting (the loud signal: agent finished
            // just now, you should respond) from stale-waiting (the
            // muted signal: agent ended hours ago, you walked away).
            let freshness = data.ui.awaiting_freshness;
            let age = agent
                .last_activity
                .and_then(|t| std::time::SystemTime::now().duration_since(t).ok());
            let is_fresh = age.map(|a| a <= freshness).unwrap_or(false);
            let age_label = age
                .map(|a| format!(" · {}", agents::format_elapsed(a)))
                .unwrap_or_default();
            if is_fresh {
                (
                    "! ",
                    Style::default().fg(theme.warning),
                    format!("awaiting input{age_label}"),
                    Style::default()
                        .fg(theme.warning)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                (
                    "· ",
                    Style::default().fg(theme.muted),
                    format!("awaiting (stale){age_label}"),
                    Style::default().fg(theme.muted),
                )
            }
        }
        transcript::Attention::Working => (
            "● ",
            Style::default().fg(theme.success),
            "working".to_string(),
            Style::default().fg(theme.success),
        ),
        transcript::Attention::Away => {
            // Claude wrote an away_summary after the last end_turn —
            // the user has been recognized as away. Show the row (so
            // abandoned work isn't invisible) but render it muted so
            // it doesn't compete with active rows for attention.
            let age = agent
                .last_activity
                .and_then(|t| std::time::SystemTime::now().duration_since(t).ok())
                .map(|d| format!(" · {}", agents::format_elapsed(d)))
                .unwrap_or_default();
            (
                "· ",
                Style::default().fg(theme.muted),
                format!("user away{age}"),
                Style::default().fg(theme.muted),
            )
        }
        transcript::Attention::Unknown => match agent.activity_status(active_window) {
            agents::ActivityStatus::Active(d) => (
                "● ",
                Style::default().fg(theme.success),
                format!("active · {}", agents::format_elapsed(d)),
                Style::default().fg(theme.success),
            ),
            agents::ActivityStatus::Idle(d) => (
                "○ ",
                Style::default().fg(theme.muted),
                format!("idle {}", agents::format_elapsed(d)),
                Style::default().fg(theme.muted),
            ),
            agents::ActivityStatus::NoTranscript => (
                "? ",
                Style::default().fg(theme.warning),
                "no transcript".to_string(),
                Style::default().fg(theme.warning),
            ),
        },
    };
    let cwd_short = cwd_for_display(&agent.cwd);

    // If this target has an active snooze, append a "snoozed in Xm" badge
    // so the user can see at a glance which rows the watcher is
    // suppressing. Snoozes come from `data.snoozes` — loaded once per
    // refresh by AppData, not re-opened per visible row.
    let snooze_badge = data.snoozes.snoozes.get(target).and_then(|until| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs();
        if *until > now {
            Some(std::time::Duration::from_secs(*until - now))
        } else {
            None
        }
    });

    let mut spans = vec![
        // Two-space indent so agent rows visually nest under their
        // session header in the Agents view.
        Span::raw("  "),
        Span::styled(marker_text, marker_style),
        Span::styled(
            format!("{:<22}", truncate(target, 22)),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            format!("{:<28}", truncate(&cwd_short, 28)),
            Style::default().fg(theme.fg),
        ),
        Span::raw(" "),
        Span::styled(status_text, status_style),
    ];
    if let Some(d) = snooze_badge {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!("snoozed {}", snooze::format_duration(d)),
            Style::default().fg(theme.warning),
        ));
    }
    Line::from(spans)
}

// ---- shared display helpers ----

/// Fixed-width truncate. Counts unicode chars, not bytes, so emoji /
/// CJK don't blow past the budget.
pub(super) fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

/// Strip any FQDN suffix to make a tmux-friendly session name. Used
/// when the `n` modal on the Hosts view prefills the session name
/// field from the selected host.
pub(super) fn short_name(s: &str) -> String {
    s.split('.').next().unwrap_or(s).to_string()
}

/// Shorten a cwd for display: replace `$HOME` prefix with `~`. The
/// full path is still shown in the preview pane. Used by the Agents
/// row and the Sessions preview's per-pane breakdown.
pub(super) fn cwd_for_display(p: &std::path::Path) -> String {
    if let Some(home) = dirs::home_dir() {
        if let Ok(rest) = p.strip_prefix(&home) {
            if rest.as_os_str().is_empty() {
                return "~".to_string();
            }
            return format!("~/{}", rest.display());
        }
    }
    p.display().to_string()
}
