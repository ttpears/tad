//! Project discovery: derive the user's *projects* (typically git repo
//! roots) from tmux pane cwds + Claude transcript directories, with
//! per-project aggregation of sessions, agents, and last activity.
//!
//! The "project" frame is tad's primary noun: instead of asking "which
//! sessions am I running?", the user usually wants "what am I working
//! on, and what's the state of each thing I'm working on?" A project
//! has zero-or-more tmux sessions whose active pane lives inside its
//! root, zero-or-more agents (claude processes) running in its
//! subtree, and a most-recent-activity timestamp from those agents'
//! transcripts.
//!
//! Discovery is deliberately concrete — we only surface projects that
//! have *current* live state (sessions or agents). Historical-only
//! projects (transcripts but no live pane) are folded in too, sorted
//! after live ones, so "yesterday's work" is still browsable but
//! today's takes the top of the list.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::agents::Agent;
use crate::sessions::Session;

#[derive(Debug, Clone)]
pub struct Project {
    /// Filesystem root of the project. Typically a git repo root; if
    /// no `.git` was found walking up from any contributing cwd, the
    /// cwd itself is the root.
    pub root: PathBuf,
    /// Human-friendly name: usually the basename of `root`. Used as
    /// the list-view label and the `Project::name == X` filter.
    pub name: String,
    /// tmux sessions whose active pane is somewhere inside `root`.
    pub sessions: Vec<Session>,
    /// Claude agents whose cwd is somewhere inside `root`.
    pub agents: Vec<Agent>,
    /// Max(transcript mtime) across the project's agents. None when
    /// the project is known only from its filesystem layout (no live
    /// agents writing transcripts).
    pub last_activity: Option<SystemTime>,
}

/// Aggregate the already-scanned sessions and agents into Projects.
///
/// Earlier versions of this function ran their own `sessions::list()` +
/// `agents::scan()` internally — which meant every dashboard tick
/// triggered those (tmux subprocess + /proc walk + transcript reads)
/// *twice*. Callers now do the scan once and hand the slices in.
pub fn from_scanned(sessions: &[Session], agents: &[Agent]) -> Vec<Project> {
    // 1. Collect every cwd we can plausibly attribute to a project.
    let mut cwds: BTreeSet<PathBuf> = BTreeSet::new();
    for s in sessions {
        if !s.active_path.is_empty() {
            cwds.insert(PathBuf::from(&s.active_path));
        }
    }
    for a in agents {
        cwds.insert(a.cwd.clone());
    }

    // 2. Map each cwd to its project root (walk up to .git/, fall back
    // to the cwd itself for non-git directories).
    let mut by_root: HashMap<PathBuf, Project> = HashMap::new();
    for cwd in &cwds {
        let root = find_project_root(cwd).unwrap_or_else(|| cwd.clone());
        let name = project_name(&root);
        by_root.entry(root.clone()).or_insert(Project {
            root,
            name,
            sessions: Vec::new(),
            agents: Vec::new(),
            last_activity: None,
        });
    }

    // 3. Bucket each session and agent into the project containing its cwd.
    for s in sessions {
        if s.active_path.is_empty() {
            continue;
        }
        let cwd = PathBuf::from(&s.active_path);
        if let Some(p) = match_project(&mut by_root, &cwd) {
            p.sessions.push(s.clone());
        }
    }
    for a in agents {
        if let Some(p) = match_project(&mut by_root, &a.cwd) {
            p.agents.push(a.clone());
        }
    }

    // 4. Compute last_activity per project from its agents.
    let mut projects: Vec<Project> = by_root.into_values().collect();
    for p in &mut projects {
        p.last_activity = p.agents.iter().filter_map(|a| a.last_activity).max();
    }

    // 5. Most-recently-active first; projects without agent activity
    // sort alphabetically at the end so "what's hot right now" is at
    // the top of the list and "everything else I've touched" is below.
    projects.sort_by(|a, b| match (a.last_activity, b.last_activity) {
        (Some(la), Some(lb)) => lb.cmp(&la),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.name.cmp(&b.name),
    });

    projects
}

/// Find the longest project-root prefix of `cwd` and return a mutable
/// reference to that Project. We match longest-prefix because nested
/// projects can exist (a checkout inside a checkout); the inner one is
/// the right home for a cwd that sits below it.
fn match_project<'a>(
    by_root: &'a mut HashMap<PathBuf, Project>,
    cwd: &Path,
) -> Option<&'a mut Project> {
    let best = by_root
        .keys()
        .filter(|root| cwd.starts_with(root))
        .max_by_key(|root| root.as_os_str().len())
        .cloned()?;
    by_root.get_mut(&best)
}

/// Walk up from `start` looking for a `.git` entry. Returns the
/// directory containing `.git` if found. We also short-circuit on a
/// `.tad/` marker so users without git can still mark a project root
/// explicitly.
pub fn find_project_root(start: &Path) -> Option<PathBuf> {
    let mut cur = start.to_path_buf();
    loop {
        if cur.join(".git").exists() || cur.join(".tad").exists() {
            return Some(cur);
        }
        if !cur.pop() || cur.as_os_str().is_empty() {
            return None;
        }
    }
}

fn project_name(root: &Path) -> String {
    root.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| root.display().to_string())
}

/// Cheap git-status: current branch and how many files have unstaged
/// or untracked changes. Subprocess per call, so for the dashboard
/// we only invoke this when the user *previews* a project, not for
/// every row. None when `root` isn't a git repo or git isn't installed.
pub fn git_status(root: &Path) -> Option<GitStatus> {
    if !root.join(".git").exists() {
        return None;
    }
    let branch = std::process::Command::new("git")
        .args([
            "-C",
            &root.to_string_lossy(),
            "rev-parse",
            "--abbrev-ref",
            "HEAD",
        ])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())?;
    let dirty = std::process::Command::new("git")
        .args(["-C", &root.to_string_lossy(), "status", "--porcelain"])
        .output()
        .ok()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter(|l| !l.is_empty())
                .count()
        })
        .unwrap_or(0);
    Some(GitStatus { branch, dirty })
}

#[derive(Debug, Clone)]
pub struct GitStatus {
    pub branch: String,
    pub dirty: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Mutex;

    static TMP_LOCK: Mutex<()> = Mutex::new(());

    fn tempdir() -> PathBuf {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let pid = std::process::id();
        let p = std::env::temp_dir().join(format!("tad-projects-test-{pid}-{nanos}"));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn project_name_is_basename_of_root() {
        assert_eq!(project_name(Path::new("/home/me/tad-github")), "tad-github");
        assert_eq!(project_name(Path::new("/home/me/foo/bar")), "bar");
    }

    #[test]
    fn find_project_root_walks_up_to_git_dir() {
        let _g = TMP_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let root = tempdir();
        let nested = root.join("a/b/c");
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        assert_eq!(find_project_root(&nested), Some(root.clone()));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn find_project_root_accepts_dot_tad_marker_for_non_git_projects() {
        let _g = TMP_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let root = tempdir();
        let nested = root.join("x/y");
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir_all(root.join(".tad")).unwrap();
        assert_eq!(find_project_root(&nested), Some(root.clone()));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn find_project_root_returns_none_when_no_marker_found() {
        let _g = TMP_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let root = tempdir();
        let nested = root.join("a");
        fs::create_dir_all(&nested).unwrap();
        // No .git, no .tad — walks all the way up to /, returns None.
        assert_eq!(find_project_root(&nested), None);
        let _ = fs::remove_dir_all(&root);
    }

    /// Hand-construct a few Projects and check that the matching algorithm
    /// picks the longest prefix (so a checkout-inside-a-checkout puts the
    /// cwd in the inner project, not the outer).
    #[test]
    fn match_project_prefers_longest_prefix() {
        let mut by_root = HashMap::new();
        by_root.insert(
            PathBuf::from("/repo"),
            Project {
                root: PathBuf::from("/repo"),
                name: "repo".into(),
                sessions: vec![],
                agents: vec![],
                last_activity: None,
            },
        );
        by_root.insert(
            PathBuf::from("/repo/sub/inner"),
            Project {
                root: PathBuf::from("/repo/sub/inner"),
                name: "inner".into(),
                sessions: vec![],
                agents: vec![],
                last_activity: None,
            },
        );
        let matched = match_project(&mut by_root, Path::new("/repo/sub/inner/x/y"))
            .unwrap()
            .name
            .clone();
        assert_eq!(matched, "inner");
        let matched_outer = match_project(&mut by_root, Path::new("/repo/sub/elsewhere"))
            .unwrap()
            .name
            .clone();
        assert_eq!(matched_outer, "repo");
    }
}
