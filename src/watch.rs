//! `tad watch` — long-running poller that auto-pops the dashboard when a
//! Claude Code agent transitions from Active to Idle (most-recent
//! transcript write age crosses the configured threshold). That's the
//! "agent is no longer thinking, probably awaiting input" signal.
//!
//! Run it once per user session: in your tmux startup hook, as a
//! systemd-user service, or just `tad watch &` in your shell rc. The
//! pidfile guard means a second `tad watch` exits immediately rather
//! than double-popping. Set `ui.auto_popup: false` in
//! `~/.config/tad/config.yaml` to fully silence it without unsetting
//! the startup hook.

use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::agents::{self, ActivityStatus, Agent};
use crate::snooze::{self, SnoozeState};
use crate::transcript::Attention;
use crate::ui_config::{self, UiConfig};

/// Unified agent state combining the precise transcript signal (when
/// parseable) with the mtime-based fallback. Pop triggers are defined
/// against transitions in this state, not in either raw input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    /// Claude finished its turn and is sitting at the prompt expecting
    /// you to reply. This is the precise "needs attention" signal.
    AwaitingInput,
    /// Either currently processing (Attention::Working) or, when the
    /// transcript can't be classified, the mtime-based "wrote recently"
    /// signal.
    Busy,
    /// Mtime stale and we don't have a precise signal. Fallback bucket
    /// for "nothing seems to be happening."
    Idle,
}

#[derive(Debug)]
struct AgentState {
    last_status: Status,
    last_popped: Option<Instant>,
    /// Was this agent snoozed at the previous observation? Used to fire
    /// a "snooze just expired and you're still idle" pop — the snooze
    /// semantics are *remind me in X*, not *mute for X*.
    was_snoozed: bool,
}

fn classify(agent: &Agent, mtime_idle_threshold: Duration) -> Status {
    match agent.attention {
        Attention::AwaitingInput => Status::AwaitingInput,
        Attention::Working => Status::Busy,
        Attention::Unknown => match agent.activity_status(mtime_idle_threshold) {
            ActivityStatus::Active(_) => Status::Busy,
            // No transcript / stale transcript both fall into Idle when
            // the precise signal isn't available.
            _ => Status::Idle,
        },
    }
}

/// Decide whether a status transition deserves a popup. The precise
/// signal (anything → AwaitingInput) is preferred; the mtime fallback
/// (Busy → Idle) is the legacy heuristic for agents whose transcripts
/// we can't parse.
fn is_pop_transition(prev: Status, curr: Status) -> bool {
    match (prev, curr) {
        (_, Status::AwaitingInput) if prev != Status::AwaitingInput => true,
        (Status::Busy, Status::Idle) => true,
        _ => false,
    }
}

pub fn run(interval_secs: u64) -> Result<i32> {
    let pid_path = pid_path();
    enforce_singleton(&pid_path)?;
    // Best-effort cleanup on graceful exit. If we crash the pidfile is
    // stale, but the next watcher detects a dead pid and overwrites.
    let _guard = PidFileGuard(pid_path);

    let ui = ui_config::load();
    if !ui.auto_popup {
        eprintln!("tad watch: ui.auto_popup is false in config.yaml — nothing to do");
        return Ok(0);
    }

    let interval = Duration::from_secs(interval_secs.max(1));
    eprintln!(
        "tad watch: polling every {}s, idle threshold {}s, cooldown {}s",
        interval.as_secs(),
        ui.auto_popup_idle.as_secs(),
        ui.auto_popup_cooldown.as_secs(),
    );

    let mut state: HashMap<String, AgentState> = HashMap::new();
    loop {
        let agents = agents::scan();
        // Snoozes are user-set from the dashboard's `s` modal and live in
        // a shared file so this process picks up changes without IPC.
        // The file is small (one line per active snooze, pruned lazily),
        // so re-reading every tick is fine.
        let snoozes = snooze::load(std::time::SystemTime::now());
        process_tick(
            &mut state,
            &agents,
            ui.auto_popup_idle,
            ui.auto_popup_cooldown,
            Instant::now(),
            &snoozes,
            std::time::SystemTime::now(),
            &ui,
            &mut RealPopper,
        );
        std::thread::sleep(interval);
    }
}

/// Pure-ish tick: caller supplies the agent list and popper so tests can
/// drive the state machine without spawning tmux or waiting on real
/// timestamps.
#[allow(clippy::too_many_arguments)]
fn process_tick<P: Popper>(
    state: &mut HashMap<String, AgentState>,
    agents: &[Agent],
    idle_threshold: Duration,
    cooldown: Duration,
    now: Instant,
    snoozes: &SnoozeState,
    wall_now: std::time::SystemTime,
    ui: &UiConfig,
    popper: &mut P,
) {
    let alive: std::collections::HashSet<&str> = agents.iter().map(|a| a.target.as_str()).collect();
    state.retain(|target, _| alive.contains(target.as_str()));

    for agent in agents {
        let new_status = classify(agent, idle_threshold);
        let is_snoozed_now = snooze::is_snoozed(snoozes, &agent.target, wall_now);
        let entry = state.entry(agent.target.clone()).or_insert(AgentState {
            // First time we see the agent: seed its state without firing.
            // If they're already AwaitingInput at first sight we don't
            // know whether they need us or they're a stale-leftover
            // session the user already replied to.
            last_status: new_status,
            last_popped: None,
            was_snoozed: is_snoozed_now,
        });

        let was = entry.last_status;
        let was_snoozed = entry.was_snoozed;
        let just_transitioned = is_pop_transition(was, new_status);
        // Snooze-expiry: deadline passed and the agent is still in a
        // pop-worthy state → remind the user. "remind me in 5m"
        // semantics, not "mute for 5m".
        let snooze_just_expired = was_snoozed
            && !is_snoozed_now
            && matches!(new_status, Status::AwaitingInput | Status::Idle);
        entry.last_status = new_status;
        entry.was_snoozed = is_snoozed_now;

        let pop_now = !is_snoozed_now && (just_transitioned || snooze_just_expired);
        if pop_now && should_pop(entry.last_popped, cooldown, now) {
            popper.pop(&agent.target, ui);
            entry.last_popped = Some(now);
        }

        // Re-arm cooldown when work resumes — if the user replies and
        // the agent starts writing again, the next AwaitingInput should
        // pop immediately rather than wait out the leftover cooldown.
        if new_status == Status::Busy {
            entry.last_popped = None;
        }
    }
}

fn should_pop(last_popped: Option<Instant>, cooldown: Duration, now: Instant) -> bool {
    match last_popped {
        None => true,
        Some(t) => now.duration_since(t) >= cooldown,
    }
}

/// Indirection so tests don't shell out to tmux.
trait Popper {
    fn pop(&mut self, target: &str, ui: &UiConfig);
}

struct RealPopper;
impl Popper for RealPopper {
    fn pop(&mut self, target: &str, ui: &UiConfig) {
        // tmux picks the most-recently-active client when -t isn't given;
        // good enough for the single-user case. If no client is attached
        // (user not currently viewing tmux), display-popup fails silently
        // and we'll try again on the next Active→Idle transition.
        let _ = Command::new("tmux")
            .args([
                "display-popup",
                "-E",
                "-w",
                &ui.auto_popup_width,
                "-h",
                &ui.auto_popup_height,
                &format!("tad --select-agent {target}"),
            ])
            .status();
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
            if pid_is_alive(pid) {
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

fn pid_is_alive(pid: i32) -> bool {
    // kill(pid, 0) returns 0 if the process exists and we have permission
    // to signal it. ESRCH (no such process) → false; EPERM (exists but
    // not signal-able) → true (still alive, just not ours).
    let rc = unsafe { libc::kill(pid, 0) };
    if rc == 0 {
        return true;
    }
    let err = std::io::Error::last_os_error();
    err.raw_os_error() == Some(libc::EPERM)
}

struct PidFileGuard(PathBuf);
impl Drop for PidFileGuard {
    fn drop(&mut self) {
        // Only remove the pidfile if it still contains our pid — don't
        // race with a successor watcher that already replaced it.
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

    /// Observe-only popper: records every (target, popup_dims) it's
    /// asked to pop, so tests can assert on the pop decisions made by
    /// `tick` without actually spawning tmux.
    #[derive(Default)]
    struct RecordingPopper {
        calls: Vec<String>,
    }
    impl Popper for RecordingPopper {
        fn pop(&mut self, target: &str, _ui: &UiConfig) {
            self.calls.push(target.to_string());
        }
    }

    #[test]
    fn should_pop_returns_true_when_never_popped() {
        assert!(should_pop(None, Duration::from_secs(60), Instant::now()));
    }

    #[test]
    fn should_pop_respects_cooldown() {
        let now = Instant::now();
        let recent = now - Duration::from_secs(10);
        let stale = now - Duration::from_secs(120);
        assert!(!should_pop(Some(recent), Duration::from_secs(60), now));
        assert!(should_pop(Some(stale), Duration::from_secs(60), now));
    }

    #[test]
    fn pidfile_detects_alive_self() {
        let me = std::process::id() as i32;
        assert!(pid_is_alive(me));
    }

    #[test]
    fn pidfile_detects_dead_pid() {
        // PID 1 always exists on a real system. To test a definitely-not-
        // alive PID we use a deliberately wild number. There's a small
        // chance an unlucky pid roll picks this, so we accept either
        // outcome but verify the call doesn't panic.
        let _ = pid_is_alive(2_147_483_640);
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
            claude_pid: 1,
            last_activity: last_activity_age.map(|age| SystemTime::now() - age),
            transcript_path: None,
            attention,
        }
    }

    /// First time we see an agent we should NOT pop, even if they're
    /// already idle — we'd be popping on a state we never observed
    /// transitioning, which is just startup noise.
    #[test]
    fn no_pop_on_first_observation_even_if_idle() {
        let mut state = HashMap::new();
        let mut popper = RecordingPopper::default();
        let ui = UiConfig::default();
        process_tick(
            &mut state,
            &[mk_agent("s:0.0", Some(Duration::from_secs(120)))],
            Duration::from_secs(30),
            Duration::from_secs(300),
            Instant::now(),
            &SnoozeState::default(),
            std::time::SystemTime::now(),
            &ui,
            &mut popper,
        );
        assert!(popper.calls.is_empty(), "should not pop on first sighting");
        assert_eq!(state.len(), 1);
    }

    /// The canonical case: agent was active last tick, idle this tick. Pop.
    #[test]
    fn pops_on_active_to_idle_transition() {
        let mut state = HashMap::new();
        let mut popper = RecordingPopper::default();
        let ui = UiConfig::default();
        let now0 = Instant::now();

        // tick 1: agent is active
        process_tick(
            &mut state,
            &[mk_agent("s:0.0", Some(Duration::from_secs(1)))],
            Duration::from_secs(30),
            Duration::from_secs(300),
            now0,
            &SnoozeState::default(),
            std::time::SystemTime::now(),
            &ui,
            &mut popper,
        );
        assert!(popper.calls.is_empty());

        // tick 2: agent is now idle (no transcript write in last 60s)
        process_tick(
            &mut state,
            &[mk_agent("s:0.0", Some(Duration::from_secs(60)))],
            Duration::from_secs(30),
            Duration::from_secs(300),
            now0 + Duration::from_secs(60),
            &SnoozeState::default(),
            std::time::SystemTime::now(),
            &ui,
            &mut popper,
        );
        assert_eq!(popper.calls, vec!["s:0.0".to_string()]);
    }

    /// Cooldown: a second idle-tick within the cooldown window shouldn't
    /// re-pop the same agent.
    #[test]
    fn cooldown_prevents_repeated_pop() {
        let mut state = HashMap::new();
        let mut popper = RecordingPopper::default();
        let ui = UiConfig::default();
        let cooldown = Duration::from_secs(300);
        let now0 = Instant::now();

        // tick 1: active
        process_tick(
            &mut state,
            &[mk_agent("s:0.0", Some(Duration::from_secs(1)))],
            Duration::from_secs(30),
            cooldown,
            now0,
            &SnoozeState::default(),
            std::time::SystemTime::now(),
            &ui,
            &mut popper,
        );
        // tick 2: idle → pop
        process_tick(
            &mut state,
            &[mk_agent("s:0.0", Some(Duration::from_secs(60)))],
            Duration::from_secs(30),
            cooldown,
            now0 + Duration::from_secs(60),
            &SnoozeState::default(),
            std::time::SystemTime::now(),
            &ui,
            &mut popper,
        );
        // tick 3: still idle, well within cooldown → no second pop
        process_tick(
            &mut state,
            &[mk_agent("s:0.0", Some(Duration::from_secs(120)))],
            Duration::from_secs(30),
            cooldown,
            now0 + Duration::from_secs(120),
            &SnoozeState::default(),
            std::time::SystemTime::now(),
            &ui,
            &mut popper,
        );
        assert_eq!(popper.calls, vec!["s:0.0".to_string()]);
    }

    /// Re-arm: after a pop, if the agent goes active again (user replied,
    /// claude is working) and then idle again, we should pop again
    /// without waiting out the original cooldown.
    #[test]
    fn activity_rearms_cooldown() {
        let mut state = HashMap::new();
        let mut popper = RecordingPopper::default();
        let ui = UiConfig::default();
        let now0 = Instant::now();

        // active → idle → pop
        process_tick(
            &mut state,
            &[mk_agent("s:0.0", Some(Duration::from_secs(1)))],
            Duration::from_secs(30),
            Duration::from_secs(300),
            now0,
            &SnoozeState::default(),
            std::time::SystemTime::now(),
            &ui,
            &mut popper,
        );
        process_tick(
            &mut state,
            &[mk_agent("s:0.0", Some(Duration::from_secs(60)))],
            Duration::from_secs(30),
            Duration::from_secs(300),
            now0 + Duration::from_secs(60),
            &SnoozeState::default(),
            std::time::SystemTime::now(),
            &ui,
            &mut popper,
        );
        assert_eq!(popper.calls.len(), 1);

        // active again (user replied)
        process_tick(
            &mut state,
            &[mk_agent("s:0.0", Some(Duration::from_secs(1)))],
            Duration::from_secs(30),
            Duration::from_secs(300),
            now0 + Duration::from_secs(70),
            &SnoozeState::default(),
            std::time::SystemTime::now(),
            &ui,
            &mut popper,
        );
        // idle again — should pop despite cooldown not having elapsed,
        // because activity reset it
        process_tick(
            &mut state,
            &[mk_agent("s:0.0", Some(Duration::from_secs(60)))],
            Duration::from_secs(30),
            Duration::from_secs(300),
            now0 + Duration::from_secs(130),
            &SnoozeState::default(),
            std::time::SystemTime::now(),
            &ui,
            &mut popper,
        );
        assert_eq!(popper.calls.len(), 2);
    }

    /// Snooze: an agent whose target is in the snooze map shouldn't pop
    /// on Active→Idle until the snooze deadline passes.
    #[test]
    fn snooze_suppresses_pop_until_deadline() {
        let mut state = HashMap::new();
        let mut popper = RecordingPopper::default();
        let ui = UiConfig::default();
        let now0 = Instant::now();
        let wall0 = std::time::SystemTime::now();

        // tick 1: active (seed state, no pop)
        process_tick(
            &mut state,
            &[mk_agent("s:0.0", Some(Duration::from_secs(1)))],
            Duration::from_secs(30),
            Duration::from_secs(300),
            now0,
            &SnoozeState::default(),
            wall0,
            &ui,
            &mut popper,
        );

        // Snooze s:0.0 for 1 hour starting now.
        let mut snoozes = SnoozeState::default();
        snoozes.snoozes.insert(
            "s:0.0".into(),
            wall0
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
                + 3600,
        );

        // tick 2: idle → would normally pop, but snoozed → suppressed.
        process_tick(
            &mut state,
            &[mk_agent("s:0.0", Some(Duration::from_secs(60)))],
            Duration::from_secs(30),
            Duration::from_secs(300),
            now0 + Duration::from_secs(60),
            &snoozes,
            wall0 + Duration::from_secs(60),
            &ui,
            &mut popper,
        );
        assert!(
            popper.calls.is_empty(),
            "snooze should suppress the Active→Idle pop"
        );

        // tick 3: snooze has expired, agent is still idle → "remind me"
        // fires: a snooze-expiry-while-idle pops once.
        process_tick(
            &mut state,
            &[mk_agent("s:0.0", Some(Duration::from_secs(7200)))],
            Duration::from_secs(30),
            Duration::from_secs(300),
            now0 + Duration::from_secs(7200),
            &SnoozeState::default(),
            wall0 + Duration::from_secs(7200),
            &ui,
            &mut popper,
        );
        assert_eq!(
            popper.calls,
            vec!["s:0.0".to_string()],
            "snooze expiry while still idle should fire a reminder pop"
        );
    }

    /// Snooze + agent goes Active during the snooze + still idle when
    /// snooze expires. The activity-during-snooze isn't a pop trigger
    /// (snooze suppresses transitions), but when the snooze expires
    /// we still want the remind-me pop.
    #[test]
    fn snooze_expiry_while_idle_pops_even_after_activity() {
        let mut state = HashMap::new();
        let mut popper = RecordingPopper::default();
        let ui = UiConfig::default();
        let now0 = Instant::now();
        let wall0 = std::time::SystemTime::now();
        let snooze_until = wall0
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
            + 60;
        let mut snoozes = SnoozeState::default();
        snoozes.snoozes.insert("s:0.0".into(), snooze_until);

        // tick 1: seed as snoozed + active
        process_tick(
            &mut state,
            &[mk_agent("s:0.0", Some(Duration::from_secs(1)))],
            Duration::from_secs(30),
            Duration::from_secs(300),
            now0,
            &snoozes,
            wall0,
            &ui,
            &mut popper,
        );
        // tick 2: still snoozed, agent now idle → suppressed
        process_tick(
            &mut state,
            &[mk_agent("s:0.0", Some(Duration::from_secs(60)))],
            Duration::from_secs(30),
            Duration::from_secs(300),
            now0 + Duration::from_secs(30),
            &snoozes,
            wall0 + Duration::from_secs(30),
            &ui,
            &mut popper,
        );
        assert!(popper.calls.is_empty());

        // tick 3: snooze expired, agent still idle → reminder pop
        process_tick(
            &mut state,
            &[mk_agent("s:0.0", Some(Duration::from_secs(120)))],
            Duration::from_secs(30),
            Duration::from_secs(300),
            now0 + Duration::from_secs(90),
            &SnoozeState::default(),
            wall0 + Duration::from_secs(90),
            &ui,
            &mut popper,
        );
        assert_eq!(popper.calls, vec!["s:0.0".to_string()]);
    }

    /// The precise transcript signal: agent was Working (transcript
    /// classifier said so), now AwaitingInput → pop. This fires even
    /// if the mtime is still fresh, because the assistant's last
    /// stop_reason is end_turn.
    #[test]
    fn precise_signal_pops_on_busy_to_awaiting_input() {
        let mut state = HashMap::new();
        let mut popper = RecordingPopper::default();
        let ui = UiConfig::default();
        let now0 = Instant::now();

        // tick 1: classifier says Working — seed state, no pop
        process_tick(
            &mut state,
            &[mk_agent_with_attention(
                "s:0.0",
                Some(Duration::from_secs(1)),
                Attention::Working,
            )],
            Duration::from_secs(30),
            Duration::from_secs(300),
            now0,
            &SnoozeState::default(),
            std::time::SystemTime::now(),
            &ui,
            &mut popper,
        );
        assert!(popper.calls.is_empty());

        // tick 2: classifier flips to AwaitingInput (claude finished a
        // turn) — pop *immediately*, no idle-threshold wait required.
        process_tick(
            &mut state,
            &[mk_agent_with_attention(
                "s:0.0",
                Some(Duration::from_secs(2)),
                Attention::AwaitingInput,
            )],
            Duration::from_secs(30),
            Duration::from_secs(300),
            now0 + Duration::from_secs(5),
            &SnoozeState::default(),
            std::time::SystemTime::now(),
            &ui,
            &mut popper,
        );
        assert_eq!(popper.calls, vec!["s:0.0".to_string()]);
    }

    /// Mixed: precise signal Unknown on tick 1, becomes Working on tick
    /// 2 (parseable now), AwaitingInput on tick 3 → pop.
    /// Sanity-checks that Unknown→Working isn't mistaken for a transition.
    #[test]
    fn unknown_to_working_does_not_pop() {
        let mut state = HashMap::new();
        let mut popper = RecordingPopper::default();
        let ui = UiConfig::default();
        let now0 = Instant::now();

        process_tick(
            &mut state,
            &[mk_agent_with_attention(
                "s:0.0",
                Some(Duration::from_secs(1)),
                Attention::Unknown,
            )],
            Duration::from_secs(30),
            Duration::from_secs(300),
            now0,
            &SnoozeState::default(),
            std::time::SystemTime::now(),
            &ui,
            &mut popper,
        );
        process_tick(
            &mut state,
            &[mk_agent_with_attention(
                "s:0.0",
                Some(Duration::from_secs(2)),
                Attention::Working,
            )],
            Duration::from_secs(30),
            Duration::from_secs(300),
            now0 + Duration::from_secs(5),
            &SnoozeState::default(),
            std::time::SystemTime::now(),
            &ui,
            &mut popper,
        );
        assert!(popper.calls.is_empty(), "Unknown→Busy should not pop");
    }

    /// Agents that vanish from the scan should be evicted from state, so
    /// when they reappear we treat them as fresh (no false pop).
    #[test]
    fn vanished_agents_are_forgotten() {
        let mut state = HashMap::new();
        let mut popper = RecordingPopper::default();
        let ui = UiConfig::default();
        let now0 = Instant::now();

        process_tick(
            &mut state,
            &[mk_agent("s:0.0", Some(Duration::from_secs(1)))],
            Duration::from_secs(30),
            Duration::from_secs(300),
            now0,
            &SnoozeState::default(),
            std::time::SystemTime::now(),
            &ui,
            &mut popper,
        );
        assert_eq!(state.len(), 1);

        // scan now returns nothing — that agent went away
        process_tick(
            &mut state,
            &[],
            Duration::from_secs(30),
            Duration::from_secs(300),
            now0 + Duration::from_secs(10),
            &SnoozeState::default(),
            std::time::SystemTime::now(),
            &ui,
            &mut popper,
        );
        assert!(state.is_empty());
    }
}
