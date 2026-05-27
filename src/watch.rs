//! `tad watch` — long-running poller that keeps the per-window
//! `@tad-attn` tmux user-variable in sync with each Claude Code agent's
//! attention state. Rendering of that variable (window-status marker,
//! status-right segment, custom user formats) lives elsewhere; this
//! module is only the trigger side.
//!
//! Run it once per user session — `tad install` writes a tmux
//! `session-created` hook that does so. The pidfile guard means a
//! second `tad watch` exits immediately rather than racing on the
//! attention variable.

use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use crate::agents::{self, ActivityStatus, Agent};
use crate::notify::{AttentionNotifier, TmuxNotifier};
use crate::snooze::{self, SnoozeState};
use crate::transcript::Attention;
use crate::ui_config::{self, UiConfig};

/// Unified agent state. The marker is set when status is `Attention`
/// and there's no active snooze and the user hasn't visited the pane
/// since the attention period began.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    /// Agent finished its turn and is sitting at the prompt expecting
    /// you to reply, OR the mtime fallback says the pane has gone idle
    /// when we can't parse the transcript.
    Attention,
    /// Currently processing, or mtime is fresh enough that we believe
    /// work is in progress.
    Busy,
    /// Mtime stale and no precise signal. Fallback "nothing happening".
    Idle,
}

#[derive(Debug)]
struct AgentState {
    last_status: Status,
    /// Was this agent snoozed at the previous observation? Used to
    /// re-arm the marker when the snooze deadline expires while the
    /// agent is still in attention state — "remind me later" semantics.
    was_snoozed: bool,
    /// Sticky-true for the duration of an attention period once the
    /// user has visited the pane. Reset when the status leaves
    /// attention so the next entry shows the marker again.
    cleared_by_visit: bool,
}

fn classify(agent: &Agent, mtime_idle_threshold: Duration) -> Status {
    match agent.attention {
        Attention::AwaitingInput => Status::Attention,
        Attention::Working => Status::Busy,
        Attention::Unknown => match agent.activity_status(mtime_idle_threshold) {
            ActivityStatus::Active(_) => Status::Busy,
            // The mtime fallback for "agent stopped writing" — without
            // a precise signal we treat a stalled transcript as the
            // user-needs-to-look signal.
            _ => Status::Idle,
        },
    }
}

/// "Is this a state the marker should light up for?" Both
/// AwaitingInput (precise) and the mtime fallback Idle qualify;
/// Busy does not.
fn is_attention(s: Status) -> bool {
    matches!(s, Status::Attention | Status::Idle)
}

pub fn run(interval_secs: u64) -> Result<i32> {
    let pid_path = pid_path();
    enforce_singleton(&pid_path)?;
    let _guard = PidFileGuard(pid_path);

    let ui = ui_config::load();
    if let Some(warning) = ui_config::deprecation_warning() {
        eprintln!("tad watch: warning: {warning}");
    }
    let interval = Duration::from_secs(interval_secs.max(1));
    eprintln!(
        "tad watch: polling every {}s, attention-idle threshold {}s",
        interval.as_secs(),
        ui.attention_idle.as_secs(),
    );

    let mut state: HashMap<String, AgentState> = HashMap::new();
    let mut notifier = TmuxNotifier;
    let mut marked: std::collections::HashSet<String> = Default::default();
    loop {
        let agents = agents::scan();
        let snoozes = snooze::load(std::time::SystemTime::now());
        process_tick(
            &mut state,
            &mut marked,
            &agents,
            ui.attention_idle,
            &snoozes,
            std::time::SystemTime::now(),
            &mut notifier,
            &ui,
        );
        std::thread::sleep(interval);
    }
}

/// Pure tick: caller supplies agents, snooze view, wall-clock, and the
/// notifier so tests can drive the state machine without tmux.
///
/// `marked` is the set of targets that currently have the marker set
/// from this watcher's POV. It's used so the PidFileGuard's cleanup
/// can sweep the world clean on shutdown without re-scanning.
#[allow(clippy::too_many_arguments)]
fn process_tick<N: AttentionNotifier>(
    state: &mut HashMap<String, AgentState>,
    marked: &mut std::collections::HashSet<String>,
    agents: &[Agent],
    idle_threshold: Duration,
    snoozes: &SnoozeState,
    wall_now: std::time::SystemTime,
    notifier: &mut N,
    _ui: &UiConfig,
) {
    let alive: std::collections::HashSet<&str> = agents.iter().map(|a| a.target.as_str()).collect();
    // Evict state for agents that vanished from the scan. Also unset
    // their marker so a stale "needs attention" doesn't hang around
    // after a pane is closed.
    let dropped: Vec<String> = state
        .keys()
        .filter(|t| !alive.contains(t.as_str()))
        .cloned()
        .collect();
    for t in dropped {
        if marked.remove(&t) {
            notifier.set_attn(&t, false);
        }
        state.remove(&t);
    }

    for agent in agents {
        let new_status = classify(agent, idle_threshold);
        let is_snoozed_now = snooze::is_snoozed(snoozes, &agent.target, wall_now);
        let entry = state.entry(agent.target.clone()).or_insert(AgentState {
            last_status: new_status,
            was_snoozed: is_snoozed_now,
            cleared_by_visit: false,
        });

        let was = entry.last_status;
        let entering_attention = !is_attention(was) && is_attention(new_status);
        if entering_attention {
            // Fresh attention period — give the user a chance to see
            // the marker even if they were just in the pane.
            entry.cleared_by_visit = false;
        }
        entry.last_status = new_status;
        entry.was_snoozed = is_snoozed_now;

        if !is_attention(new_status) {
            if marked.remove(&agent.target) {
                notifier.set_attn(&agent.target, false);
            }
            entry.cleared_by_visit = false;
            continue;
        }

        if is_snoozed_now {
            if marked.remove(&agent.target) {
                notifier.set_attn(&agent.target, false);
            }
            continue;
        }

        if notifier.is_visited(&agent.target) {
            entry.cleared_by_visit = true;
        }

        let want_on = !entry.cleared_by_visit;
        let currently_on = marked.contains(&agent.target);
        if want_on != currently_on {
            notifier.set_attn(&agent.target, want_on);
            if want_on {
                marked.insert(agent.target.clone());
            } else {
                marked.remove(&agent.target);
            }
        }
    }
}

// ---- singleton (pidfile) ----

fn pid_path() -> PathBuf {
    dirs::state_dir()
        .or_else(dirs::cache_dir)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("tad")
        .join("watch.pid")
}

fn enforce_singleton(path: &std::path::Path) -> Result<()> {
    if let Ok(text) = fs::read_to_string(path) {
        if let Ok(pid) = text.trim().parse::<i32>() {
            if crate::proc_util::pid_is_alive(pid) {
                bail!(
                    "tad watch already running as pid {pid} (delete {} to override)",
                    path.display()
                );
            }
        }
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    let me = std::process::id();
    fs::write(path, me.to_string()).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

struct PidFileGuard(PathBuf);
impl Drop for PidFileGuard {
    fn drop(&mut self) {
        if let Ok(text) = fs::read_to_string(&self.0) {
            if text.trim().parse::<u32>().ok() == Some(std::process::id()) {
                let _ = fs::remove_file(&self.0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Recording notifier — captures every set_attn call and answers
    /// is_visited from a scripted queue so tests don't need a tmux
    /// server.
    #[derive(Default)]
    struct RecordingNotifier {
        calls: Vec<(String, bool)>,
        /// Queue of (target, visited) answers. Pops front on each call;
        /// if empty returns false.
        visit_answers: std::collections::VecDeque<bool>,
    }
    impl AttentionNotifier for RecordingNotifier {
        fn set_attn(&mut self, target: &str, on: bool) {
            self.calls.push((target.to_string(), on));
        }
        fn is_visited(&mut self, _target: &str) -> bool {
            self.visit_answers.pop_front().unwrap_or(false)
        }
    }

    use crate::agents::Agent;
    use std::path::PathBuf;
    use std::time::SystemTime;

    fn mk_agent(target: &str, last_activity_age: Option<Duration>) -> Agent {
        mk_agent_with_attention(target, last_activity_age, Attention::Unknown)
    }

    fn mk_agent_with_attention(
        target: &str,
        last_activity_age: Option<Duration>,
        attention: Attention,
    ) -> Agent {
        Agent {
            target: target.into(),
            session: "s".into(),
            window_index: "0".into(),
            window_name: "w".into(),
            pane_index: "0".into(),
            cwd: PathBuf::from("/tmp"),
            agent_pid: 1,
            provider_id: "claude",
            last_activity: last_activity_age.map(|age| SystemTime::now() - age),
            transcript_path: None,
            attention,
        }
    }

    fn empty_marked() -> std::collections::HashSet<String> {
        Default::default()
    }

    /// Entering attention → marker on, exactly once.
    #[test]
    fn entering_attention_sets_marker() {
        let mut state = HashMap::new();
        let mut marked = empty_marked();
        let mut n = RecordingNotifier::default();
        let ui = UiConfig::default();
        let wall = SystemTime::now();

        // tick 1: Busy (seed state, no call)
        process_tick(
            &mut state,
            &mut marked,
            &[mk_agent("s:0.0", Some(Duration::from_secs(1)))],
            Duration::from_secs(30),
            &SnoozeState::default(),
            wall,
            &mut n,
            &ui,
        );
        assert!(n.calls.is_empty());

        // tick 2: Idle (mtime fallback) — entering attention → set on
        process_tick(
            &mut state,
            &mut marked,
            &[mk_agent("s:0.0", Some(Duration::from_secs(120)))],
            Duration::from_secs(30),
            &SnoozeState::default(),
            wall + Duration::from_secs(60),
            &mut n,
            &ui,
        );
        assert_eq!(n.calls, vec![("s:0.0".to_string(), true)]);
        assert!(marked.contains("s:0.0"));
    }

    /// While in attention, a visit clears the marker and it stays off
    /// for subsequent ticks (sticky).
    #[test]
    fn visit_clears_marker_and_stays_clear() {
        let mut state = HashMap::new();
        let mut marked = empty_marked();
        let mut n = RecordingNotifier::default();
        let ui = UiConfig::default();
        let wall = SystemTime::now();

        // tick 1: seed Busy
        process_tick(
            &mut state,
            &mut marked,
            &[mk_agent_with_attention(
                "s:0.0",
                Some(Duration::from_secs(1)),
                Attention::Working,
            )],
            Duration::from_secs(30),
            &SnoozeState::default(),
            wall,
            &mut n,
            &ui,
        );
        // tick 2: Attention, not visited → marker on
        process_tick(
            &mut state,
            &mut marked,
            &[mk_agent_with_attention(
                "s:0.0",
                Some(Duration::from_secs(2)),
                Attention::AwaitingInput,
            )],
            Duration::from_secs(30),
            &SnoozeState::default(),
            wall + Duration::from_secs(10),
            &mut n,
            &ui,
        );
        assert_eq!(n.calls, vec![("s:0.0".into(), true)]);

        // tick 3: still Attention, visit registered → marker off
        n.visit_answers.push_back(true);
        process_tick(
            &mut state,
            &mut marked,
            &[mk_agent_with_attention(
                "s:0.0",
                Some(Duration::from_secs(3)),
                Attention::AwaitingInput,
            )],
            Duration::from_secs(30),
            &SnoozeState::default(),
            wall + Duration::from_secs(20),
            &mut n,
            &ui,
        );
        assert_eq!(
            n.calls,
            vec![("s:0.0".into(), true), ("s:0.0".into(), false)]
        );

        // tick 4: still Attention, not visited this tick → stays off
        process_tick(
            &mut state,
            &mut marked,
            &[mk_agent_with_attention(
                "s:0.0",
                Some(Duration::from_secs(4)),
                Attention::AwaitingInput,
            )],
            Duration::from_secs(30),
            &SnoozeState::default(),
            wall + Duration::from_secs(30),
            &mut n,
            &ui,
        );
        // Still only the two calls — no re-set.
        assert_eq!(n.calls.len(), 2);
    }

    /// Status leaves attention → marker off, sticky flag reset so the
    /// next attention period gets a fresh marker.
    #[test]
    fn leaving_attention_clears_and_re_arms() {
        let mut state = HashMap::new();
        let mut marked = empty_marked();
        let mut n = RecordingNotifier::default();
        let ui = UiConfig::default();
        let wall = SystemTime::now();

        // attention, marker on
        process_tick(
            &mut state,
            &mut marked,
            &[mk_agent_with_attention(
                "s:0.0",
                Some(Duration::from_secs(1)),
                Attention::AwaitingInput,
            )],
            Duration::from_secs(30),
            &SnoozeState::default(),
            wall,
            &mut n,
            &ui,
        );
        // visited → marker off
        n.visit_answers.push_back(true);
        process_tick(
            &mut state,
            &mut marked,
            &[mk_agent_with_attention(
                "s:0.0",
                Some(Duration::from_secs(2)),
                Attention::AwaitingInput,
            )],
            Duration::from_secs(30),
            &SnoozeState::default(),
            wall + Duration::from_secs(5),
            &mut n,
            &ui,
        );
        // back to Busy — already off so no extra call
        let calls_before = n.calls.len();
        process_tick(
            &mut state,
            &mut marked,
            &[mk_agent_with_attention(
                "s:0.0",
                Some(Duration::from_secs(1)),
                Attention::Working,
            )],
            Duration::from_secs(30),
            &SnoozeState::default(),
            wall + Duration::from_secs(10),
            &mut n,
            &ui,
        );
        assert_eq!(n.calls.len(), calls_before);

        // back to Attention, not visited → marker on again (sticky cleared)
        process_tick(
            &mut state,
            &mut marked,
            &[mk_agent_with_attention(
                "s:0.0",
                Some(Duration::from_secs(2)),
                Attention::AwaitingInput,
            )],
            Duration::from_secs(30),
            &SnoozeState::default(),
            wall + Duration::from_secs(20),
            &mut n,
            &ui,
        );
        assert!(n.calls.last() == Some(&("s:0.0".into(), true)));
    }

    /// Snooze suppresses the marker, expiry re-arms it.
    #[test]
    fn snooze_suppresses_marker_until_expiry() {
        let mut state = HashMap::new();
        let mut marked = empty_marked();
        let mut n = RecordingNotifier::default();
        let ui = UiConfig::default();
        let wall0 = SystemTime::now();
        let snooze_until = wall0
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
            + 60;
        let mut snoozes = SnoozeState::default();
        snoozes.snoozes.insert("s:0.0".into(), snooze_until);

        // seed Busy under snooze
        process_tick(
            &mut state,
            &mut marked,
            &[mk_agent_with_attention(
                "s:0.0",
                Some(Duration::from_secs(1)),
                Attention::Working,
            )],
            Duration::from_secs(30),
            &snoozes,
            wall0,
            &mut n,
            &ui,
        );
        // entering Attention while snoozed → no marker
        process_tick(
            &mut state,
            &mut marked,
            &[mk_agent_with_attention(
                "s:0.0",
                Some(Duration::from_secs(2)),
                Attention::AwaitingInput,
            )],
            Duration::from_secs(30),
            &snoozes,
            wall0 + Duration::from_secs(10),
            &mut n,
            &ui,
        );
        assert!(n.calls.is_empty());

        // snooze expired, still Attention → marker on
        process_tick(
            &mut state,
            &mut marked,
            &[mk_agent_with_attention(
                "s:0.0",
                Some(Duration::from_secs(120)),
                Attention::AwaitingInput,
            )],
            Duration::from_secs(30),
            &SnoozeState::default(),
            wall0 + Duration::from_secs(120),
            &mut n,
            &ui,
        );
        assert_eq!(n.calls, vec![("s:0.0".into(), true)]);
    }

    /// Vanished agents are evicted, and if they were marked we unset
    /// the marker on the way out so stale state doesn't linger.
    #[test]
    fn vanished_agents_clear_their_marker() {
        let mut state = HashMap::new();
        let mut marked = empty_marked();
        let mut n = RecordingNotifier::default();
        let ui = UiConfig::default();
        let wall = SystemTime::now();

        process_tick(
            &mut state,
            &mut marked,
            &[mk_agent_with_attention(
                "s:0.0",
                Some(Duration::from_secs(2)),
                Attention::AwaitingInput,
            )],
            Duration::from_secs(30),
            &SnoozeState::default(),
            wall,
            &mut n,
            &ui,
        );
        assert_eq!(n.calls, vec![("s:0.0".into(), true)]);

        // pane vanished
        process_tick(
            &mut state,
            &mut marked,
            &[],
            Duration::from_secs(30),
            &SnoozeState::default(),
            wall + Duration::from_secs(10),
            &mut n,
            &ui,
        );
        assert!(state.is_empty());
        assert!(marked.is_empty());
        assert_eq!(
            n.calls,
            vec![("s:0.0".into(), true), ("s:0.0".into(), false)]
        );
    }
}
