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
use std::fs;
use std::path::Path;

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
    /// Couldn't decide — empty transcript, unparseable lines, unfamiliar
    /// event shapes, etc. Caller should fall back to the mtime heuristic.
    Unknown,
}

/// Read the last ~`tail_bytes` of the transcript and classify the agent's
/// current state. None on read error. A safe default tail size (256KB) is
/// large enough to span dozens of events on real transcripts.
pub fn classify_file(path: &Path) -> Attention {
    classify_file_with(path, 256 * 1024)
}

fn classify_file_with(path: &Path, tail_bytes: u64) -> Attention {
    let Ok(meta) = fs::metadata(path) else {
        return Attention::Unknown;
    };
    let len = meta.len();
    let start = len.saturating_sub(tail_bytes);
    let Ok(bytes) = read_tail(path, start) else {
        return Attention::Unknown;
    };
    classify(&bytes)
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
    for raw in text.lines().rev() {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
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
            Some(state) => return state,
            None => continue,
        }
    }
    Attention::Unknown
}

/// Turn one event into an attention verdict, or None if this event
/// alone isn't decisive (e.g. it's an assistant event with no
/// stop_reason we recognise).
fn decide(ev: &Event) -> Option<Attention> {
    match ev.r#type.as_deref()? {
        "assistant" => {
            let stop = ev.message.as_ref()?.stop_reason.as_deref();
            match stop {
                Some("end_turn") | Some("stop_sequence") | Some("max_tokens") => {
                    Some(Attention::AwaitingInput)
                }
                Some("tool_use") => Some(Attention::Working),
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
}
