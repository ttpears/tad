//! Per-row-kind formatters for the sidebar cockpit, plus a few shared
//! display helpers (cwd → `~/…`, fixed-width truncation, FQDN → short
//! tmux-friendly name). Pure functions over `&AppData`; no side
//! effects, called once per visible row per render frame.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::agents::{self, AgentState};
use crate::theme::Theme;
use crate::transcript;

use super::rows::{Row, RowKind, Section};
use super::{AppData, PinnedPane};

/// Is `target` under an active (non-expired) snooze right now? Shared
/// by the agent row/group-header formatters and `agent_state`'s
/// `snoozed` input — one lookup path so they can never disagree.
pub(super) fn snoozed(data: &AppData, target: &str) -> bool {
    data.snoozes.snoozes.get(target).is_some_and(|until| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        *until > now
    })
}

/// Marker + status for an agent row, shared by the Agents-view row
/// formatter and the agent preview header so the two never disagree.
pub(super) fn agent_status(
    data: &AppData,
    agent: &crate::agents::Agent,
    theme: &Theme,
) -> (&'static str, Style, String, Style) {
    let active_window = std::time::Duration::from_secs(30);
    match agent.attention {
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
    }
}

// ---- sidebar cockpit row rendering ----

fn dot_color(state: AgentState, theme: &Theme) -> ratatui::style::Color {
    match state {
        AgentState::Blocked => theme.warning,
        AgentState::Working => theme.success,
        AgentState::Idle | AgentState::Away => theme.muted,
    }
}

fn format_section_header(
    data: &AppData,
    section: Section,
    theme: &Theme,
    width: usize,
) -> Line<'static> {
    let title = section.title();
    let (count_text, count_color) = match section {
        Section::Agents => {
            let states: Vec<AgentState> = data
                .agents
                .iter()
                .map(|a| agents::agent_state(a, snoozed(data, &a.target), data.ui.attention_idle))
                .collect();
            let counts = agents::state_counts(&states);
            let label = agents::header_count_label(&counts);
            let color = if counts.blocked > 0 {
                theme.warning
            } else {
                theme.muted
            };
            (label, color)
        }
        Section::Sessions => (data.sessions.len().to_string(), theme.muted),
        Section::Groups => (data.groups.len().to_string(), theme.muted),
        Section::Hosts => (data.hosts.len().to_string(), theme.muted),
    };
    let used = title.chars().count() + count_text.chars().count();
    let pad = width.saturating_sub(used).max(1);
    Line::from(vec![
        Span::styled(
            title,
            Style::default()
                .fg(theme.accent_bold)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" ".repeat(pad)),
        Span::styled(count_text, Style::default().fg(count_color)),
    ])
}

fn format_session_row(data: &AppData, name: &str, theme: &Theme) -> Line<'static> {
    let Some(s) = data.sessions.iter().find(|s| s.name == name) else {
        return Line::from(name.to_string());
    };
    let (dot, dot_color) = if s.attached {
        ("● ", theme.success)
    } else {
        ("○ ", theme.muted)
    };
    Line::from(vec![
        Span::styled(dot, Style::default().fg(dot_color)),
        Span::styled(
            name.to_string(),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(s.activity_str.clone(), Style::default().fg(theme.muted)),
    ])
}

/// `session:window_name` — the identity a `PinnedPane::label` carries.
/// Used to decide whether an Agent row should show the `◂` marker.
fn pin_label(session: &str, window_name: &str) -> String {
    format!("{session}:{window_name}")
}

fn format_agent_row(
    data: &AppData,
    target: &str,
    theme: &Theme,
    tick: u64,
    pins: &[PinnedPane],
) -> Line<'static> {
    let Some(agent) = data.agents.iter().find(|a| a.target == target) else {
        return Line::from(target.to_string());
    };
    let state = agents::agent_state(agent, snoozed(data, target), data.ui.attention_idle);
    let dot = agents::state_dot(state, tick);
    let color = dot_color(state, theme);
    // Derivable identity for a pinned Agent row: the pin's label is
    // `session:window_name`, which is exactly what an Agent row's
    // (session, window_name) pair reconstructs. Session/Group/Host
    // rows don't have an equally clean mapping — see format_row's
    // doc comment.
    let pinned = pins
        .iter()
        .any(|p| p.label == pin_label(&agent.session, &agent.window_name));

    let mut spans = vec![
        // Two-space indent so agent rows visually nest under their
        // session group header.
        Span::raw("  "),
        Span::styled(format!("{dot} "), Style::default().fg(color)),
        Span::styled(agent.window_name.clone(), Style::default().fg(theme.fg)),
    ];
    if pinned {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            "◂",
            Style::default()
                .fg(theme.accent_bold)
                .add_modifier(Modifier::BOLD),
        ));
    }
    Line::from(spans)
}

/// `<session> · N agents · M awaiting` — M omitted when zero, singular
/// "agent" when N==1. Ported from the pre-rows
/// `agent_items_grouped_by_session`; "awaiting" now counts agents
/// currently `AgentState::Blocked` (attention-based, snooze-aware)
/// rather than the raw `Attention::AwaitingInput` tally, so a snoozed
/// agent no longer counts as awaiting.
fn format_agent_group_header(data: &AppData, session: &str, theme: &Theme) -> Line<'static> {
    let agents_in_session: Vec<&crate::agents::Agent> = data
        .agents
        .iter()
        .filter(|a| a.session == session)
        .collect();
    let n = agents_in_session.len();
    let blocked = agents_in_session
        .iter()
        .filter(|a| {
            let state = agents::agent_state(a, snoozed(data, &a.target), data.ui.attention_idle);
            state == AgentState::Blocked
        })
        .count();
    let plural = if n == 1 { "" } else { "s" };
    let mut text = format!("{session} · {n} agent{plural}");
    if blocked > 0 {
        text.push_str(&format!(" · {blocked} awaiting"));
    }
    Line::from(Span::styled(text, Style::default().fg(theme.muted)))
}

fn format_group_row(data: &AppData, name: &str, theme: &Theme) -> Line<'static> {
    let Some((_, g)) = data.groups.iter().find(|(n, _)| n == name) else {
        return Line::from(name.to_string());
    };
    Line::from(vec![
        Span::styled(
            name.to_string(),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            format!("{} hosts · {}", g.hosts.len(), g.layout),
            Style::default().fg(theme.muted),
        ),
    ])
}

fn format_host_row(data: &AppData, name: &str, theme: &Theme) -> Line<'static> {
    let row = data.hosts.iter().find(|r| r.name == name);
    let groups = row.map(|r| r.groups.join(", ")).unwrap_or_default();
    let source = row.map(|r| r.source.clone()).unwrap_or_default();
    let trailing = if groups.is_empty() {
        source
    } else if source.is_empty() {
        groups
    } else {
        format!("{groups} · {source}")
    };
    Line::from(vec![
        Span::styled(name.to_string(), Style::default().fg(theme.accent)),
        Span::raw("  "),
        Span::styled(trailing, Style::default().fg(theme.muted)),
    ])
}

/// Compact sidebar row renderer, dispatched per `RowKind`:
/// - `SectionHeader` — bold accent title, right-aligned count
///   (`header_count_label` for Agents, plain item count otherwise).
/// - `Session` — attached-dot + name + activity.
/// - `Agent` — state dot + window name + `◂` when pinned.
/// - `AgentGroupHeader` — muted `session · N agents · M awaiting`.
/// - `Group`/`Host` — name + muted meta.
///
/// Always clipped to `width` display columns.
pub(super) fn format_row(
    data: &AppData,
    row: &Row,
    theme: &Theme,
    tick: u64,
    pins: &[PinnedPane],
    width: u16,
) -> Line<'static> {
    let width = width as usize;
    let line = match &row.kind {
        RowKind::SectionHeader(section) => format_section_header(data, *section, theme, width),
        RowKind::Session(name) => format_session_row(data, name, theme),
        RowKind::Agent(target) => format_agent_row(data, target, theme, tick, pins),
        RowKind::AgentGroupHeader(session) => format_agent_group_header(data, session, theme),
        RowKind::Group(name) => format_group_row(data, name, theme),
        RowKind::Host(name) => format_host_row(data, name, theme),
    };
    clip_line(line, width)
}

// ---- shared display helpers ----

/// Clip a rendered line to at most `width` display columns, preserving
/// each span's style up to the cut point. Assumes single-width glyphs
/// (ascii, box-drawing, the state dots) — true for everything the
/// sidebar renders today.
pub(super) fn clip_line(line: Line<'static>, width: usize) -> Line<'static> {
    let mut budget = width;
    let mut spans = Vec::new();
    for span in line.spans {
        if budget == 0 {
            break;
        }
        let content = span.content.as_ref();
        let count = content.chars().count();
        if count <= budget {
            budget -= count;
            spans.push(span);
        } else {
            let clipped: String = content.chars().take(budget).collect();
            budget = 0;
            spans.push(Span::styled(clipped, span.style));
        }
    }
    Line::from(spans)
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dashboard::testutil::{mk_agent, mk_data, mk_session};
    use crate::dashboard::PinnedPane;
    use std::path::PathBuf;
    use std::time::SystemTime;

    fn mk_blocked_agent(target: &str, session: &str) -> crate::agents::Agent {
        crate::agents::Agent {
            target: target.into(),
            session: session.into(),
            window_index: "0".into(),
            window_name: "w".into(),
            pane_index: "0".into(),
            cwd: PathBuf::from("/repo"),
            agent_pid: 1,
            provider_id: "claude",
            last_activity: Some(SystemTime::now()),
            transcript_path: None,
            attention: crate::transcript::Attention::AwaitingInput,
        }
    }

    fn mk_pin(label: &str) -> PinnedPane {
        PinnedPane {
            pane_id: "%1".into(),
            origin_window_id: "@1".into(),
            origin_session: "s".into(),
            origin_window_name: "w".into(),
            origin_window_index: "0".into(),
            label: label.to_string(),
        }
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn format_row_renders_blocked_agent_with_dot_and_pin_marker() {
        let agent = mk_blocked_agent("work:0.0", "work");
        let data = mk_data(vec![], vec![agent]);
        let theme = crate::theme::load();
        let row = Row {
            kind: RowKind::Agent("work:0.0".into()),
            selectable: true,
        };
        // pin_label(session="work", window_name="w") == "work:w"
        let pins = vec![mk_pin("work:w")];
        let line = format_row(&data, &row, &theme, 0, &pins, 40);
        let text = line_text(&line);
        assert!(text.contains('●'), "expected blocked dot in {text:?}");
        assert!(text.contains('◂'), "expected pin marker in {text:?}");
    }

    #[test]
    fn format_row_agent_not_pinned_has_no_marker() {
        let agent = mk_blocked_agent("work:0.0", "work");
        let data = mk_data(vec![], vec![agent]);
        let theme = crate::theme::load();
        let row = Row {
            kind: RowKind::Agent("work:0.0".into()),
            selectable: true,
        };
        let line = format_row(&data, &row, &theme, 0, &[], 40);
        assert!(!line_text(&line).contains('◂'));
    }

    #[test]
    fn section_header_shows_plain_count_for_sessions() {
        let data = mk_data(vec![mk_session("a"), mk_session("b")], vec![]);
        let theme = crate::theme::load();
        let row = Row {
            kind: RowKind::SectionHeader(Section::Sessions),
            selectable: true,
        };
        let line = format_row(&data, &row, &theme, 0, &[], 40);
        let text = line_text(&line);
        assert!(text.contains("SESSIONS"));
        assert!(text.trim_end().ends_with('2'));
    }

    #[test]
    fn section_header_agents_uses_blocked_over_total_label() {
        let agent = mk_blocked_agent("work:0.0", "work");
        let data = mk_data(vec![], vec![agent]);
        let theme = crate::theme::load();
        let row = Row {
            kind: RowKind::SectionHeader(Section::Agents),
            selectable: true,
        };
        let line = format_row(&data, &row, &theme, 0, &[], 40);
        let text = line_text(&line);
        assert!(text.trim_end().ends_with("1/1"), "got {text:?}");
    }

    #[test]
    fn format_row_clips_to_requested_width() {
        let data = mk_data(vec![mk_session("a-very-long-session-name")], vec![]);
        let theme = crate::theme::load();
        let row = Row {
            kind: RowKind::Session("a-very-long-session-name".into()),
            selectable: true,
        };
        let line = format_row(&data, &row, &theme, 0, &[], 10);
        let width: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
        assert!(
            width <= 10,
            "line was {width} cols wide: {:?}",
            line_text(&line)
        );
    }

    #[test]
    fn agent_group_header_singular_no_awaiting() {
        let data = mk_data(vec![], vec![mk_agent("s1:0.0", "s1", 1)]);
        let theme = crate::theme::load();
        let line = format_agent_group_header(&data, "s1", &theme);
        assert_eq!(line_text(&line), "s1 · 1 agent");
    }

    #[test]
    fn agent_group_header_plural_with_awaiting_count() {
        let blocked = mk_blocked_agent("s1:0.0", "s1");
        let idle = mk_agent("s1:1.0", "s1", 1);
        let data = mk_data(vec![], vec![blocked, idle]);
        let theme = crate::theme::load();
        let line = format_agent_group_header(&data, "s1", &theme);
        assert_eq!(line_text(&line), "s1 · 2 agents · 1 awaiting");
    }

    #[test]
    fn agent_group_header_snoozed_blocked_agent_not_counted_as_awaiting() {
        let blocked = mk_blocked_agent("s1:0.0", "s1");
        let mut data = mk_data(vec![], vec![blocked]);
        let until = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3600;
        data.snoozes.snoozes.insert("s1:0.0".into(), until);
        let theme = crate::theme::load();
        let line = format_agent_group_header(&data, "s1", &theme);
        assert_eq!(line_text(&line), "s1 · 1 agent");
    }
}
