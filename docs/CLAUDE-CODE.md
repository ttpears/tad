# Claude Code cockpit: Agents view + status-line segment

If you run multiple Claude Code agents across multiple tmux panes — one per
repo, one per worktree, parallel investigations — tad has a view for that.

**The Agents section** (jump with `2`) lists every tmux pane on this
server whose process tree contains a `claude` process, showing
`<session>:<window>.<pane>  <cwd>  <status>` per row with a semantic
status dot (see the states legend in the
[Dashboard section](../README.md#dashboard) of the README:
`●` blocked, `◐◓◑◒` working, `○` idle, `◌` away).

`Enter` runs `tmux switch-client` to jump straight to that pane. If
you've installed the
[popup keybind](../README.md#jumping-back-to-the-dashboard-from-inside-tmux),
the loop is:

```
prefix + D  →  2  (Agents section)  →  ↑↓ to the blocked one  →  ↵  →  reply
```

**The status-line segment** so you never have to look first. Add this to
your tmux config (auto-detected location — `~/.tmux.conf.local`,
`~/.tmux.local.conf`, `$XDG_CONFIG_HOME/tmux/tmux.conf`, or
`~/.tmux.conf`):

```
set -g status-interval 5
set -g status-right '#(tad status) | %H:%M '
```

You'll see `claude: 3/12` (3 currently active, 12 total) in your status
line. Format compacts to `claude: N` if all are active, or
`claude: N idle` if none are active. Prints nothing when no agents are
running, so when you're not using Claude Code your status line stays
clean.

The "active" threshold is the last 30s of transcript-write activity by
default. Tune with `tad status --active-secs N`. tad does no caching, so
if your status interval is low (1s) and you have hundreds of panes, the
per-tick scan cost adds up — keep it at 5–15s.

Detection is process-tree based (walks `/proc/<pane_pid>/task/*/children`
looking for a `claude` comm), so it catches any agent regardless of how
it was started.

## `tad watch`: passive `@tad-attn` attention marker

The status segment is for *checking* at a glance. `tad watch` is for
*surfacing* attention without grabbing your screen. It's a long-running
poller that watches every claude pane and, when one needs your input,
sets the per-window tmux user-variable `@tad-attn=1` on that window.
When you respond (or visit the pane), it unsets it. No popups, no
modal interruptions — just a quiet bit you can render however you like.

`tad install` writes a window-status block that appends a `!` to the
window name when `@tad-attn` is set, so your tmux window list looks
like:

```
[1] main   [2] salt !   [3] cops
```

…and the `!` disappears the moment you visit `salt` or the agent
starts working again. Heavy theme users can opt out with
`tad install --no-window-marker` and consume `@tad-attn` directly:

```tmux
set -g status-right '#(tad status) #{?@tad-attn,⚠ ,}%H:%M'
```

Start it once per user session — `tad install` writes a tmux
session-created hook that does this for you:

```tmux
set-hook -g session-created 'run-shell -b "pgrep -x tad >/dev/null || tad watch &"'
```

A pidfile (`$XDG_STATE_HOME/tad/watch.pid`) guards against
double-running. Snoozes from the dashboard `s` modal suppress the
marker for the snoozed agent until the deadline.

Tune via `~/.config/tad/config.yaml`:

```yaml
ui:
  attention_idle_secs: 30         # mtime-fallback threshold
  awaiting_freshness_secs: 600    # "user walked away" cutoff for status count
```

Pre-v0.11 configs with `ui.auto_popup*` keys still parse; the watcher
prints a one-line deprecation notice on startup. `tad doctor` flags
them too.
