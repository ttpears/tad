//! On-demand group/host pickers. Groups and hosts are reference
//! material, not the day-to-day sidebar view, so they live behind a
//! filterable overlay (`g` / `h`) instead of an always-on section.
//! This module is the pure part: which names a picker shows for a
//! given filter, plus the per-kind labels. The overlay's rendering
//! lives in `modal.rs`, its key handling in `keys.rs`, and activation
//! in `action.rs` — all of which route the chosen name back through
//! the same `dispatch::OpenTarget::{Group,Host}` the old sidebar rows
//! used.

use super::AppData;

/// Which on-demand list a `InputMode::Picker` overlay is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PickerKind {
    Groups,
    Hosts,
}

impl PickerKind {
    /// Modal title, e.g. ` groups `.
    pub(super) fn title(self) -> &'static str {
        match self {
            PickerKind::Groups => " groups ",
            PickerKind::Hosts => " hosts ",
        }
    }

    /// Shown centered when the underlying list is empty.
    pub(super) fn empty_hint(self) -> &'static str {
        match self {
            PickerKind::Groups => "no groups configured — run `tad config`",
            PickerKind::Hosts => "no hosts discovered — add groups via `tad config`",
        }
    }
}

/// Case-insensitive substring match; an empty filter matches everything
/// (same semantics as the sidebar tree's own filter).
fn matches(name: &str, filter_lower: &str) -> bool {
    filter_lower.is_empty() || name.to_lowercase().contains(filter_lower)
}

/// The names a picker of `kind` shows under `filter`, in the same order
/// `AppData` holds them (groups sorted by name; hosts in discovery
/// order). Pure over `AppData` so it's shared by the renderer, the key
/// handler, and activation without any of them re-deriving the list.
pub(super) fn items(data: &AppData, kind: PickerKind, filter: &str) -> Vec<String> {
    let filter_lower = filter.to_lowercase();
    match kind {
        PickerKind::Groups => data
            .groups
            .iter()
            .map(|(name, _)| name.clone())
            .filter(|n| matches(n, &filter_lower))
            .collect(),
        PickerKind::Hosts => data
            .hosts
            .iter()
            .map(|h| h.name.clone())
            .filter(|n| matches(n, &filter_lower))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dashboard::testutil::mk_data;
    use crate::dashboard::HostRow;

    fn data_with_groups_and_hosts() -> AppData {
        let mut data = mk_data(vec![], vec![]);
        data.groups = vec![
            (
                "prod-web".to_string(),
                crate::config::Group {
                    layout: "panes".to_string(),
                    hosts: vec![],
                },
            ),
            (
                "staging".to_string(),
                crate::config::Group {
                    layout: "panes".to_string(),
                    hosts: vec![],
                },
            ),
        ];
        data.hosts = vec![
            HostRow {
                name: "web01".to_string(),
                groups: vec![],
                source: String::new(),
            },
            HostRow {
                name: "db01".to_string(),
                groups: vec![],
                source: String::new(),
            },
        ];
        data
    }

    #[test]
    fn empty_filter_returns_every_name_in_source_order() {
        let data = data_with_groups_and_hosts();
        assert_eq!(
            items(&data, PickerKind::Groups, ""),
            vec!["prod-web", "staging"]
        );
        assert_eq!(items(&data, PickerKind::Hosts, ""), vec!["web01", "db01"]);
    }

    #[test]
    fn filter_is_case_insensitive_substring() {
        let data = data_with_groups_and_hosts();
        assert_eq!(items(&data, PickerKind::Groups, "PROD"), vec!["prod-web"]);
        assert_eq!(items(&data, PickerKind::Hosts, "01"), vec!["web01", "db01"]);
        assert_eq!(items(&data, PickerKind::Hosts, "web"), vec!["web01"]);
    }

    #[test]
    fn no_match_yields_empty() {
        let data = data_with_groups_and_hosts();
        assert!(items(&data, PickerKind::Groups, "zzz").is_empty());
    }
}
