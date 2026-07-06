//! Sidebar row-tree model: a pure function from `AppData` plus a bit of
//! UI state (which sections are collapsed, the current filter text) to
//! a flat list of `Row`s. The sidebar (a later task) just walks this
//! list to render lines and move the cursor — keeping the tree-shaping
//! logic here, pure and IO-free, means it can be exhaustively unit
//! tested without tmux and without touching the renderer at all.

// TODO(herdr-cockpit): consumed by the sidebar-render task — remove this
// allow once wired up.
#![allow(dead_code)]

use std::collections::HashSet;

use crate::agents::Agent;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Section {
    Sessions,
    Agents,
    Groups,
    Hosts,
}

impl Section {
    pub(super) const ALL: [Section; 4] = [
        Section::Sessions,
        Section::Agents,
        Section::Groups,
        Section::Hosts,
    ];

    pub(super) fn title(self) -> &'static str {
        match self {
            Section::Sessions => "SESSIONS",
            Section::Agents => "AGENTS",
            Section::Groups => "GROUPS",
            Section::Hosts => "HOSTS",
        }
    }

    pub(super) fn slug(self) -> &'static str {
        match self {
            Section::Sessions => "sessions",
            Section::Agents => "agents",
            Section::Groups => "groups",
            Section::Hosts => "hosts",
        }
    }

    pub(super) fn from_slug(s: &str) -> Option<Section> {
        match s {
            "sessions" => Some(Section::Sessions),
            "agents" => Some(Section::Agents),
            "groups" => Some(Section::Groups),
            "hosts" => Some(Section::Hosts),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RowKind {
    SectionHeader(Section),
    /// Session name.
    Session(String),
    /// tmux session grouping agents; NOT cursor-selectable.
    AgentGroupHeader(String),
    /// Agent target `session:win.pane`.
    Agent(String),
    Group(String),
    Host(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Row {
    pub(super) kind: RowKind,
    /// Cursor can land here. True for everything except AgentGroupHeader.
    pub(super) selectable: bool,
}

impl Row {
    fn new(kind: RowKind, selectable: bool) -> Self {
        Row { kind, selectable }
    }
}

/// Case-insensitive substring match; an empty filter matches everything.
fn matches(name: &str, filter_lower: &str) -> bool {
    filter_lower.is_empty() || name.to_lowercase().contains(filter_lower)
}

/// Agents grouped by tmux session, most-recently-active session first,
/// each session's agents most-recent-first. Ported from the pre-rows
/// `agent_items_grouped_by_session` in `dashboard.rs`.
fn agents_by_session(data: &super::AppData) -> Vec<(String, Vec<&Agent>)> {
    let mut by_session: Vec<(String, Vec<&Agent>)> = Vec::new();
    for a in &data.agents {
        match by_session.iter_mut().find(|(s, _)| s == &a.session) {
            Some((_, v)) => v.push(a),
            None => by_session.push((a.session.clone(), vec![a])),
        }
    }
    by_session.sort_by_key(|(_, agents)| {
        std::cmp::Reverse(agents.iter().filter_map(|a| a.last_activity).max())
    });
    for (_, agents) in &mut by_session {
        agents.sort_by_key(|a| std::cmp::Reverse(a.last_activity));
    }
    by_session
}

/// Row kinds for the Agents section. Unfiltered: grouped presentation
/// with an `AgentGroupHeader` per session. Filtered: a flat list of
/// matching `Agent` rows only — group headers are dropped while a
/// filter is active.
fn agent_rows(data: &super::AppData, filter_lower: &str, filter_active: bool) -> Vec<RowKind> {
    let mut out = Vec::new();
    for (session, agents) in agents_by_session(data) {
        if filter_active {
            for a in agents {
                if matches(&a.target, filter_lower) {
                    out.push(RowKind::Agent(a.target.clone()));
                }
            }
        } else {
            out.push(RowKind::AgentGroupHeader(session));
            for a in agents {
                out.push(RowKind::Agent(a.target.clone()));
            }
        }
    }
    out
}

/// Build the full sidebar row list. Order: Sessions, Agents, Groups, Hosts.
/// - Every section always emits its SectionHeader row.
/// - A collapsed section emits only its header.
/// - With a non-empty filter (case-insensitive substring on the item name /
///   agent target), sections behave as if collapsed when they have zero
///   matches; matching items show even in sections the user collapsed
///   (filter overrides collapse).
/// - Agents section keeps grouped-by-session presentation (unless a
///   filter is active, in which case group headers are dropped).
pub(super) fn build_rows(
    data: &super::AppData,
    collapsed: &HashSet<Section>,
    filter: &str,
) -> Vec<Row> {
    let filter_lower = filter.to_lowercase();
    let filter_active = !filter_lower.is_empty();

    let mut rows = Vec::new();
    for section in Section::ALL {
        rows.push(Row::new(RowKind::SectionHeader(section), true));

        let items: Vec<RowKind> = match section {
            Section::Sessions => data
                .sessions
                .iter()
                .filter(|s| matches(&s.name, &filter_lower))
                .map(|s| RowKind::Session(s.name.clone()))
                .collect(),
            Section::Agents => agent_rows(data, &filter_lower, filter_active),
            Section::Groups => data
                .groups
                .iter()
                .filter(|(name, _)| matches(name, &filter_lower))
                .map(|(name, _)| RowKind::Group(name.clone()))
                .collect(),
            Section::Hosts => data
                .hosts
                .iter()
                .filter(|h| matches(&h.name, &filter_lower))
                .map(|h| RowKind::Host(h.name.clone()))
                .collect(),
        };

        let hide = if filter_active {
            items.is_empty()
        } else {
            collapsed.contains(&section)
        };

        if !hide {
            for kind in items {
                let selectable = !matches!(kind, RowKind::AgentGroupHeader(_));
                rows.push(Row::new(kind, selectable));
            }
        }
    }
    rows
}

/// Move the cursor delta steps over selectable rows, wrapping. Returns the
/// new index, or None when no row is selectable.
///
/// `cur` may be out of range or land on a non-selectable row (e.g. an
/// `AgentGroupHeader`); it is clamped to `rows.len() - 1` first. When the
/// (clamped) `cur` is *not* itself selectable, it is treated as an
/// insertion point between two selectable rows:
/// - `delta >= 1` lands on the `delta`-th selectable row *after* cur, so
///   `delta == 1` lands on the immediately-next selectable row (wrapping
///   to the first selectable row if there is none after cur).
/// - `delta <= -1` lands on the `|delta|`-th selectable row *before* cur,
///   so `delta == -1` lands on the immediately-previous selectable row
///   (wrapping to the last selectable row if there is none before cur).
/// - `delta == 0` snaps forward to the next selectable row.
///
/// When `cur` is itself selectable, this steps `delta` positions through
/// the selectable sequence, wrapping, unchanged from before.
pub(super) fn step_selectable(rows: &[Row], cur: usize, delta: i32) -> Option<usize> {
    let selectable: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.selectable)
        .map(|(i, _)| i)
        .collect();
    if selectable.is_empty() {
        return None;
    }
    let clamped = cur.min(rows.len().saturating_sub(1));
    let n = selectable.len() as i32;

    let new_pos = match selectable.iter().position(|&i| i == clamped) {
        Some(p) => (p as i32 + delta).rem_euclid(n),
        None => {
            // `clamped` is an insertion point between selectable rows:
            // `next_pos` is the index (into `selectable`) of the first
            // selectable row after it, wrapping to 0 if there is none.
            let next_pos = selectable.iter().position(|&i| i > clamped).unwrap_or(0) as i32;
            match delta.cmp(&0) {
                std::cmp::Ordering::Greater => (next_pos + delta - 1).rem_euclid(n),
                std::cmp::Ordering::Less => (next_pos + delta).rem_euclid(n),
                std::cmp::Ordering::Equal => next_pos,
            }
        }
    };
    Some(selectable[new_pos as usize])
}

/// Index of a section's header row.
pub(super) fn section_header_index(rows: &[Row], section: Section) -> Option<usize> {
    rows.iter()
        .position(|r| r.kind == RowKind::SectionHeader(section))
}

fn is_item(row: &Row) -> bool {
    row.selectable && !matches!(row.kind, RowKind::SectionHeader(_))
}

/// First selectable NON-header row (used by Home/g); last one (End/G).
pub(super) fn first_item_index(rows: &[Row]) -> Option<usize> {
    rows.iter().position(is_item)
}

pub(super) fn last_item_index(rows: &[Row]) -> Option<usize> {
    rows.iter().rposition(is_item)
}

/// Find the row index for a RowKind (used to restore persisted selection
/// and --select-agent).
pub(super) fn index_of(rows: &[Row], kind: &RowKind) -> Option<usize> {
    rows.iter().position(|r| &r.kind == kind)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dashboard::testutil::{mk_agent, mk_data, mk_session};
    use std::collections::HashSet;

    #[test]
    fn build_rows_emits_four_section_headers_in_order_even_empty() {
        let data = mk_data(vec![], vec![]);
        let rows = build_rows(&data, &HashSet::new(), "");
        let headers: Vec<Section> = rows
            .iter()
            .filter_map(|r| match r.kind {
                RowKind::SectionHeader(s) => Some(s),
                _ => None,
            })
            .collect();
        assert_eq!(
            headers,
            vec![
                Section::Sessions,
                Section::Agents,
                Section::Groups,
                Section::Hosts
            ]
        );
    }

    #[test]
    fn sessions_items_under_header_and_collapse_hides_them() {
        let data = mk_data(vec![mk_session("alpha"), mk_session("beta")], vec![]);
        let rows = build_rows(&data, &HashSet::new(), "");
        let idx = section_header_index(&rows, Section::Sessions).unwrap();
        assert_eq!(rows[idx + 1].kind, RowKind::Session("alpha".into()));
        assert_eq!(rows[idx + 2].kind, RowKind::Session("beta".into()));

        let mut collapsed = HashSet::new();
        collapsed.insert(Section::Sessions);
        let rows = build_rows(&data, &collapsed, "");
        let idx = section_header_index(&rows, Section::Sessions).unwrap();
        assert_eq!(rows[idx + 1].kind, RowKind::SectionHeader(Section::Agents));
    }

    #[test]
    fn agents_grouped_by_session_most_recent_first() {
        let agents = vec![
            mk_agent("s1:0.0", "s1", 10),
            mk_agent("s2:0.0", "s2", 30),
            mk_agent("s1:1.0", "s1", 20),
        ];
        let data = mk_data(vec![], agents);
        let rows = build_rows(&data, &HashSet::new(), "");
        let idx = section_header_index(&rows, Section::Agents).unwrap();
        let after: Vec<&Row> = rows[idx + 1..].iter().take(4).collect();
        assert_eq!(after[0].kind, RowKind::AgentGroupHeader("s2".into()));
        assert!(!after[0].selectable);
        assert_eq!(after[1].kind, RowKind::Agent("s2:0.0".into()));
        assert_eq!(after[2].kind, RowKind::AgentGroupHeader("s1".into()));
        assert!(!after[2].selectable);
        assert_eq!(after[3].kind, RowKind::Agent("s1:1.0".into()));
    }

    #[test]
    fn filter_shows_only_matches_hides_empty_sections_overrides_collapse() {
        let data = mk_data(
            vec![mk_session("web"), mk_session("db")],
            vec![mk_agent("web:0.0", "web", 5)],
        );
        let mut collapsed = HashSet::new();
        collapsed.insert(Section::Sessions);
        let rows = build_rows(&data, &collapsed, "web");

        let s_idx = section_header_index(&rows, Section::Sessions).unwrap();
        assert_eq!(rows[s_idx + 1].kind, RowKind::Session("web".into()));

        let g_idx = section_header_index(&rows, Section::Groups).unwrap();
        assert_eq!(rows[g_idx + 1].kind, RowKind::SectionHeader(Section::Hosts));

        let a_idx = section_header_index(&rows, Section::Agents).unwrap();
        assert_eq!(rows[a_idx + 1].kind, RowKind::Agent("web:0.0".into()));
        assert!(!rows
            .iter()
            .any(|r| matches!(r.kind, RowKind::AgentGroupHeader(_))));
    }

    #[test]
    fn step_selectable_skips_group_headers_and_wraps() {
        let rows = vec![
            Row::new(RowKind::SectionHeader(Section::Agents), true), // 0
            Row::new(RowKind::AgentGroupHeader("s2".into()), false), // 1
            Row::new(RowKind::Agent("s2:0.0".into()), true),         // 2
            Row::new(RowKind::AgentGroupHeader("s1".into()), false), // 3
            Row::new(RowKind::Agent("s1:0.0".into()), true),         // 4
        ];

        assert_eq!(step_selectable(&rows, 2, 1), Some(4));
        assert_eq!(step_selectable(&rows, 4, 1), Some(0));
        assert_eq!(step_selectable(&rows, 0, -1), Some(4));
    }

    #[test]
    fn step_selectable_from_unselectable_cursor_lands_on_adjacent_selectable() {
        let rows = vec![
            Row::new(RowKind::SectionHeader(Section::Agents), true), // 0
            Row::new(RowKind::AgentGroupHeader("s2".into()), false), // 1
            Row::new(RowKind::Agent("s2:0.0".into()), true),         // 2
            Row::new(RowKind::AgentGroupHeader("s1".into()), false), // 3
            Row::new(RowKind::Agent("s1:0.0".into()), true),         // 4
        ];

        // cur on a non-selectable AgentGroupHeader row: delta +1 lands on
        // the immediately-next selectable row, not one past it.
        assert_eq!(step_selectable(&rows, 1, 1), Some(2));
        // delta -1 lands on the immediately-previous selectable row.
        assert_eq!(step_selectable(&rows, 1, -1), Some(0));

        // Same, from the other non-selectable row, with wrap-forward.
        assert_eq!(step_selectable(&rows, 3, 1), Some(4));
        assert_eq!(step_selectable(&rows, 3, -1), Some(2));
    }

    #[test]
    fn step_selectable_clamps_out_of_range_cursor() {
        let rows = vec![
            Row::new(RowKind::Agent("s1:0.0".into()), true), // 0
            Row::new(RowKind::AgentGroupHeader("s1".into()), false), // 1 (last row, non-selectable)
        ];

        // cur is out of range; clamped to the last row (non-selectable),
        // so delta +1 wraps to the first selectable row.
        assert_eq!(step_selectable(&rows, rows.len() + 10, 1), Some(0));
    }

    #[test]
    fn step_selectable_returns_none_when_nothing_selectable() {
        let rows = vec![
            Row::new(RowKind::AgentGroupHeader("s1".into()), false),
            Row::new(RowKind::AgentGroupHeader("s2".into()), false),
        ];
        assert_eq!(step_selectable(&rows, 0, 1), None);
    }

    #[test]
    fn first_and_last_item_index_skip_headers() {
        let data = mk_data(
            vec![mk_session("alpha")],
            vec![mk_agent("s1:0.0", "s1", 10)],
        );
        let rows = build_rows(&data, &HashSet::new(), "");
        let first = first_item_index(&rows).unwrap();
        assert_eq!(rows[first].kind, RowKind::Session("alpha".into()));
        assert!(rows[first].selectable);

        let last = last_item_index(&rows).unwrap();
        assert_eq!(rows[last].kind, RowKind::Agent("s1:0.0".into()));
    }

    #[test]
    fn index_of_finds_agent_row() {
        let data = mk_data(vec![], vec![mk_agent("s1:0.0", "s1", 10)]);
        let rows = build_rows(&data, &HashSet::new(), "");
        let idx = index_of(&rows, &RowKind::Agent("s1:0.0".into()));
        assert!(idx.is_some());
        assert_eq!(rows[idx.unwrap()].kind, RowKind::Agent("s1:0.0".into()));
    }
}
