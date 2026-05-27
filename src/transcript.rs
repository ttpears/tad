//! Best-effort "is this agent waiting for me?" detection by inspecting
//! the tail of a Claude Code session transcript (the `.jsonl` files
//! under `~/.claude/projects/<encoded-cwd>/`).
//!
//! The signal is the most recent assistant message's `stop_reason`:
//!   * `end_turn` → claude finished a response; the next event should
//!     come from the user, so we're awaiting input
//!   * `tool_use` → claude requested a tool, still mid-thinking (will
//!     continue when the tool_result event arrives)
//!   * anything else / unparseable → Unknown, and the watcher falls
//!     back to the mtime-only Active/Idle heuristic
//!
//! Critically, if a later `user` event exists (a fresh user prompt OR a
//! tool_result the assistant is about to process), classify as Working
//! rather than AwaitingInput — even if the *previous* assistant event
//! was `end_turn`, the user has already replied so we're not waiting on
//! them.
//!
//! The Anthropic-API-side fields (stop_reason, content[].type) are
//! stable; the tad/CC outer wrapper changes more often, so the parser
//! is deliberately defensive: missing fields, unrecognized types, weird
//! `null`s, and lines that aren't even valid JSON all degrade to
//! "Unknown" rather than panicking.

use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::SystemTime;

/// What we infer about an agent from its transcript tail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attention {
    /// Claude finished a turn and there's no user reply after it. The
    /// agent is sitting at the prompt expecting input from the human.
    AwaitingInput,
    /// Either the assistant is mid-tool-call, the tool result is being
    /// processed, or the user just sent something and claude is
    /// thinking. In any case the user shouldn't be summoned.
    Working,
    /// Claude finished a turn AND has since written an `away_summary`
    /// system event — its own "the user walked away from this session"
    /// signal. The session is technically still AwaitingInput but the
    /// user has already been recognized as away, so surfacing it as
    /// "waiting" is noise. Watcher / status segment treat this as
    /// quiet; the dashboard still shows the row (with an "away" label)
    /// so abandoned work isn't invisible.
    Away,
    /// Couldn't decide — empty transcript, unparseable lines, unfamiliar
    /// event shapes, etc. Caller should fall back to the mtime heuristic.
    Unknown,
}

/// Cache: each transcript file is read at most once per mtime.
///
/// Without this, every `agents::scan()` (every 1.5s in the dashboard,
/// every 5s in `tad watch`) re-reads up to 256KB per claude pane —
/// against my live setup that's ~2.5MB/s of disk traffic for content
/// that didn't change. The cache reduces steady-state cost to one
/// `fs::metadata` call per transcript (just to see if mtime moved).
///
/// Unbounded: one entry per *ever-seen* transcript file, which is
/// small (one per claude session in your project history). If that
/// ever becomes large enough to matter (thousands of transcripts in a
/// single tad process) we'd need an LRU; for v1 the simple Mutex
/// over a HashMap is the right shape.
static CLASSIFY_CACHE: LazyLock<Mutex<HashMap<PathBuf, (SystemTime, Attention)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Read the last ~`tail_bytes` of the transcript and classify the agent's
/// current state. A safe default tail size (256KB) is large enough to
/// span dozens of events on real transcripts. Cached by (path, mtime):
/// a repeat call with no change-on-disk is one `stat` and a map lookup.
pub fn classify_file(path: &Path) -> Attention {
    classify_file_with(path, 256 * 1024)
}

fn classify_file_with(path: &Path, tail_bytes: u64) -> Attention {
    let Ok(meta) = fs::metadata(path) else {
        return Attention::Unknown;
    };
    let mtime = meta.modified().ok();

    // Fast path: same mtime as last classification → same verdict.
    if let Some(mt) = mtime {
        let cache = CLASSIFY_CACHE.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((cached_mt, cached_verdict)) = cache.get(path) {
            if *cached_mt == mt {
                return *cached_verdict;
            }
        }
    }

    let len = meta.len();
    let start = len.saturating_sub(tail_bytes);
    let Ok(bytes) = read_tail(path, start) else {
        return Attention::Unknown;
    };
    let verdict = classify(&bytes);

    if let Some(mt) = mtime {
        let mut cache = CLASSIFY_CACHE.lock().unwrap_or_else(|p| p.into_inner());
        cache.insert(path.to_path_buf(), (mt, verdict));
    }
    verdict
}

fn read_tail(path: &Path, start: u64) -> std::io::Result<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = fs::File::open(path)?;
    f.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    Ok(buf)
}

/// Decide the attention state by walking events from the end. Public for
/// hermetic tests (pass in transcript bytes directly).
pub fn classify(bytes: &[u8]) -> Attention {
    let text = match std::str::from_utf8(bytes) {
        Ok(s) => s,
        Err(_) => return Attention::Unknown,
    };

    // Walk lines from the end. The first event we encounter that gives
    // us a decisive signal wins; everything earlier is noise.
    //
    // We may have read mid-line at the head of the tail window — that
    // partial line will fail to parse and we just skip it.
    //
    // Side-channel: if we encounter an `away_summary` system event
    // BEFORE the first decisive user/assistant event, the user has
    // walked away from this session (claude writes the summary on
    // user-inactivity). When that flag is set and the verdict would
    // be AwaitingInput, downgrade to Away so the status segment and
    // marker don't shout about a session the user has abandoned.
    let mut saw_away_since_last_message = false;
    for raw in text.lines().rev() {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        if is_away_summary(trimmed) {
            saw_away_since_last_message = true;
            continue;
        }
        // Cheap pre-filter so we don't pay serde overhead on the dozens
        // of `agent-name` / `tool_reference` / `worktree-state` etc.
        // events that share the file.
        if !trimmed.contains("\"type\":\"assistant\"") && !trimmed.contains("\"type\":\"user\"") {
            continue;
        }
        let Ok(ev) = serde_json::from_str::<Event>(trimmed) else {
            continue;
        };
        match decide(&ev) {
            Some(Attention::AwaitingInput) if saw_away_since_last_message => {
                return Attention::Away;
            }
            Some(state) => return state,
            None => continue,
        }
    }
    Attention::Unknown
}

/// Cheap substring check for the away-summary event marker. claude
/// writes these as `{"type":"system","subtype":"away_summary",...}`;
/// the two substrings together are enough to identify them without
/// parsing the JSON.
fn is_away_summary(line: &str) -> bool {
    line.contains("\"type\":\"system\"") && line.contains("\"subtype\":\"away_summary\"")
}

/// Turn one event into an attention verdict, or None if this event
/// alone isn't decisive (e.g. it's an assistant event with no
/// stop_reason we recognise).
fn decide(ev: &Event) -> Option<Attention> {
    match ev.r#type.as_deref()? {
        "assistant" => {
            let stop = ev.message.as_ref()?.stop_reason.as_deref();
            match stop {
                // The assistant intentionally finished its response.
                // The next event should come from the user, so we're
                // awaiting input.
                Some("end_turn") | Some("stop_sequence") => Some(Attention::AwaitingInput),
                // The assistant requested a tool — claude is still
                // thinking, will continue when the tool_result event
                // arrives.
                Some("tool_use") => Some(Attention::Working),
                // max_tokens means the response was truncated mid-stream
                // because it hit the output-token cap. Claude did *not*
                // finish its turn — popping the dashboard here would
                // summon the user for a response that's still being
                // produced. Treat as Working.
                Some("max_tokens") => Some(Attention::Working),
                _ => None,
            }
        }
        "user" => {
            // A `user` event is one of:
            //   - tool_result (the tool finished, claude will process it next) → Working
            //   - text from a human (the user just typed something) → Working
            // Either way: not awaiting input from the human right now.
            Some(Attention::Working)
        }
        _ => None,
    }
}

// ---- JSON event shape (only the fields we care about) ----

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct Event {
    r#type: Option<String>,
    message: Option<Message>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct Message {
    stop_reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Last event is an assistant message that ended its turn → AwaitingInput.
    #[test]
    fn assistant_end_turn_is_awaiting_input() {
        let lines = "\
{\"type\":\"system\",\"message\":\"hi\"}
{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"do the thing\"}]}}
{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"stop_reason\":\"end_turn\",\"content\":[{\"type\":\"text\",\"text\":\"done\"}]}}
";
        assert_eq!(classify(lines.as_bytes()), Attention::AwaitingInput);
    }

    /// Last event is an assistant `tool_use` (a tool was kicked off, tool_result
    /// hasn't landed yet) → Working.
    #[test]
    fn assistant_tool_use_is_working() {
        let lines = "\
{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"stop_reason\":\"end_turn\",\"content\":[]}}
{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"stop_reason\":\"tool_use\",\"content\":[{\"type\":\"tool_use\",\"id\":\"x\",\"name\":\"Bash\"}]}}
";
        assert_eq!(classify(lines.as_bytes()), Attention::Working);
    }

    /// Assistant ended its turn but the user replied after → Working.
    /// (Classic case: claude finished, the human typed something, the
    /// next assistant turn hasn't begun yet.)
    #[test]
    fn user_reply_after_end_turn_means_working() {
        let lines = "\
{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"stop_reason\":\"end_turn\"}}
{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"and another thing\"}]}}
";
        assert_eq!(classify(lines.as_bytes()), Attention::Working);
    }

    /// A user event carrying a tool_result is still "working" — claude
    /// is about to consume it.
    #[test]
    fn user_tool_result_is_working() {
        let lines = "\
{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"stop_reason\":\"tool_use\"}}
{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"tool_use_id\":\"x\",\"type\":\"tool_result\",\"content\":\"42\"}]}}
";
        assert_eq!(classify(lines.as_bytes()), Attention::Working);
    }

    /// Garbage / no assistant or user events → Unknown.
    #[test]
    fn no_relevant_events_is_unknown() {
        let lines = "\
{\"type\":\"agent-name\",\"name\":\"alpha\"}
{\"type\":\"permission-mode\",\"permissionMode\":\"default\"}
{\"type\":\"tool_reference\"}
";
        assert_eq!(classify(lines.as_bytes()), Attention::Unknown);
    }

    /// Malformed JSON line in the middle shouldn't poison classification —
    /// we just skip and keep walking.
    #[test]
    fn malformed_line_is_skipped() {
        let lines = "\
{\"type\":\"assistant\",\"message\":{\"stop_reason\":\"end_turn\"}}
this is not json at all
";
        assert_eq!(classify(lines.as_bytes()), Attention::AwaitingInput);
    }

    /// Empty input → Unknown (not a panic).
    #[test]
    fn empty_input_is_unknown() {
        assert_eq!(classify(b""), Attention::Unknown);
    }

    /// `max_tokens` means the response was *truncated*, not finished.
    /// Classifying it as AwaitingInput would summon the user during a
    /// still-being-produced long response.
    #[test]
    fn max_tokens_is_working_not_awaiting() {
        let lines = "\
{\"type\":\"assistant\",\"message\":{\"stop_reason\":\"max_tokens\"}}
";
        assert_eq!(classify(lines.as_bytes()), Attention::Working);
    }

    /// stop_reason we don't recognise on the most recent assistant event →
    /// fall back to whatever event comes before, or Unknown.
    #[test]
    fn unrecognized_stop_reason_falls_back() {
        let lines = "\
{\"type\":\"assistant\",\"message\":{\"stop_reason\":\"end_turn\"}}
{\"type\":\"assistant\",\"message\":{\"stop_reason\":\"some_new_reason_we_dont_know_about\"}}
";
        // The newer event isn't decisive, so we walk back and find the
        // older end_turn → AwaitingInput.
        assert_eq!(classify(lines.as_bytes()), Attention::AwaitingInput);
    }

    /// claude wrote an away_summary after the last end_turn → the user
    /// has been recognized as away. Downgrade AwaitingInput to Away.
    #[test]
    fn away_summary_after_end_turn_yields_away() {
        let lines = "\
{\"type\":\"assistant\",\"message\":{\"stop_reason\":\"end_turn\"}}
{\"type\":\"system\",\"subtype\":\"away_summary\",\"content\":\"…\"}
";
        assert_eq!(classify(lines.as_bytes()), Attention::Away);
    }

    /// A user reply after the away_summary means the user came back —
    /// classify should reflect the new exchange (Working in this case
    /// because the trailing event is a user message), not Away.
    #[test]
    fn user_reply_after_away_summary_overrides_it() {
        let lines = "\
{\"type\":\"assistant\",\"message\":{\"stop_reason\":\"end_turn\"}}
{\"type\":\"system\",\"subtype\":\"away_summary\",\"content\":\"…\"}
{\"type\":\"user\",\"message\":{\"role\":\"user\"}}
";
        assert_eq!(classify(lines.as_bytes()), Attention::Working);
    }

    /// An assistant turn that completes AFTER an earlier away_summary
    /// (i.e. user came back, replied, claude finished again) is genuine
    /// AwaitingInput — the away signal was older than the new exchange.
    #[test]
    fn new_exchange_after_old_away_summary_is_awaiting_input() {
        let lines = "\
{\"type\":\"assistant\",\"message\":{\"stop_reason\":\"end_turn\"}}
{\"type\":\"system\",\"subtype\":\"away_summary\",\"content\":\"…\"}
{\"type\":\"user\",\"message\":{\"role\":\"user\"}}
{\"type\":\"assistant\",\"message\":{\"stop_reason\":\"end_turn\"}}
";
        assert_eq!(classify(lines.as_bytes()), Attention::AwaitingInput);
    }

    /// away_summary alone with no decisive assistant event in range →
    /// fall back to Unknown (the away flag without an AwaitingInput
    /// verdict to downgrade isn't a verdict by itself).
    #[test]
    fn away_summary_alone_is_unknown() {
        let lines = "{\"type\":\"system\",\"subtype\":\"away_summary\",\"content\":\"…\"}\n";
        assert_eq!(classify(lines.as_bytes()), Attention::Unknown);
    }
}
