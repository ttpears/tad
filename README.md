<p align="center">
  <img src="docs/logo.svg" alt="tad" width="128" height="128">
</p>

# tad

<p align="center">
  <a href="https://github.com/ttpears/tad/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/ttpears/tad/actions/workflows/ci.yml/badge.svg"></a>
  <a href="https://github.com/ttpears/tad/releases/latest"><img alt="Latest release" src="https://img.shields.io/github/v/release/ttpears/tad?display_name=tag&sort=semver"></a>
  <a href="https://aur.archlinux.org/packages/tmux-tad"><img alt="AUR version" src="https://img.shields.io/aur/version/tmux-tad"></a>
  <a href="https://github.com/ttpears/tad/releases"><img alt="Downloads" src="https://img.shields.io/github/downloads/ttpears/tad/total"></a>
  <a href="LICENSE"><img alt="License: MIT" src="https://img.shields.io/github/license/ttpears/tad"></a>
</p>

A tmux session and group manager. Bare `tad` opens a native TUI dashboard —
a collapsible sidebar cockpit of live sessions and running agents, with named
groups and their hosts a keystroke away (`g` / `h`) — with live updates every ~1.5s.
`tad <name>` resolves in order:
attach to an existing tmux session by that name → SSH into it as a discovered
host (in a new session) → create a new blank session by that name.
`tad -g <group>` opens a multi-host session whose layout you control per
group; for multi-pane layouts you're prompted whether to enable tmux
`synchronize-panes` (default: yes).

![tad dashboard demo](docs/screenshots/dashboard.gif)

## Install

Only requirement at runtime: `tmux`. The dashboard ships inside the
binary.

### Arch Linux (AUR)

Two variants per AUR convention:

```sh
yay -S tmux-tad           # builds from source (needs cargo/rust)
yay -S tmux-tad-bin       # prebuilt x86_64 binary from the GitHub release
```

Both install the same `/usr/bin/tad`, completions, and example configs;
they conflict with each other and `provides`-substitute. Pick whichever
you prefer — `tmux-tad-bin` is faster to install, `tmux-tad` is closer
to upstream. PKGBUILD sources live at `packaging/aur/tmux-tad/` and
`packaging/aur/tmux-tad-bin/` in this repo.

### Debian / Ubuntu (.deb)

```sh
TAD_VERSION=v0.7.0
curl -fLO "https://github.com/ttpears/tad/releases/download/${TAD_VERSION}/tad-${TAD_VERSION}-x86_64.deb"
sudo apt install "./tad-${TAD_VERSION}-x86_64.deb"   # pulls in tmux if needed
```

(`apt install ./<file.deb>` is preferred over raw `dpkg -i` because apt
resolves the `tmux` dependency.)

### Fedora / RHEL (.rpm)

```sh
TAD_VERSION=v0.7.0
sudo dnf install "https://github.com/ttpears/tad/releases/download/${TAD_VERSION}/tad-${TAD_VERSION}-x86_64.rpm"
```

The release is unsigned, so `dnf` will prompt to accept on first
install.

### macOS / Linux (Homebrew)

```sh
brew tap ttpears/tad
brew install tad
```

Builds from source via Homebrew's `rust` formula.

### From a release (any Linux x86_64)

Each [release](https://github.com/ttpears/tad/releases) ships a static
Linux x86_64 binary, matching completion files, and a `SHA256SUMS`.

```sh
TAD_VERSION=v0.7.0
BASE="https://github.com/ttpears/tad/releases/download/${TAD_VERSION}"

mkdir -p ~/.local/bin \
         ~/.local/share/bash-completion/completions \
         ~/.local/share/zsh/site-functions

curl -fL "${BASE}/tad-${TAD_VERSION}-x86_64-linux" -o ~/.local/bin/tad
chmod +x ~/.local/bin/tad

curl -fL "${BASE}/tad.bash" -o ~/.local/share/bash-completion/completions/tad
curl -fL "${BASE}/_tad"     -o ~/.local/share/zsh/site-functions/_tad
```

Verify against `SHA256SUMS` from the release if you care:

```sh
curl -fL "${BASE}/SHA256SUMS" | grep "tad-${TAD_VERSION}-x86_64-linux" \
    | sha256sum -c --ignore-missing -
```

Make sure `~/.local/bin` is in `PATH` and (for zsh) that
`~/.local/share/zsh/site-functions` is in `fpath`.

### From source

```sh
git clone https://github.com/ttpears/tad.git ~/git/tad
cd ~/git/tad
make install              # builds release binary + installs binary and
                          # completions under ~/.local
```

Or just:
```sh
cargo install --git https://github.com/ttpears/tad --locked
```

### Shell completions (manual)

`make install` and the AUR package both put completions in the standard
locations. If you installed the binary by other means, do this once:

bash:
```sh
ln -s ~/git/tad/completions/tad.bash ~/.local/share/bash-completion/completions/tad
```

zsh — add to your `.zshrc` if it's not already:
```sh
fpath=(~/.local/share/zsh/site-functions $fpath)
autoload -Uz compinit && compinit
```

Completion scripts live in `completions/`. `tad <TAB>` completes tmux
**sessions** and **discovered hosts**. Group names complete after
`tad -g <TAB>` — they are not offered at the bare top level.

## Host discovery / `tad config`

### Automatic live discovery

On every dashboard launch (and for CLI dispatch and shell completion),
`tad` automatically scans **local sources only** for host names — no
network, no DNS:

- **`~/.ssh/config`** — concrete `Host` entries, including one-level-deep
  `Include`. Wildcard patterns are skipped.
- **`~/.ssh/known_hosts`** — non-hashed entries. `@cert-authority` and
  `|1|...` hashed entries are skipped.
- **Shell history** — `$HISTFILE`, `~/.bash_history`, `~/.zsh_history`,
  fish history. Extracts hosts from `ssh user@host -p 22` style
  invocations, strips `user@` and flag values, ignores `sshfs` /
  `ssh-add` / `ssh-keygen` / `ssh-copy-id`.

Results are **ranked**: ssh-config and known_hosts entries come first,
then shell-history hosts by frequency. History-only hosts seen fewer than
`min_history_uses` times (default: 2) are hidden unless they also appear
in ssh-config or known_hosts. Discovered hosts appear live in the
dashboard's hosts picker (`h`) and in shell completion. Discovery is never
written to config — it is always fresh.

Tune via `~/.config/tad/config.yaml` (all keys optional):

```yaml
discovery:
  min_history_uses: 2      # history-only hosts below this are hidden
  shell_history: true      # toggle sources independently
  ssh_config: true
  known_hosts: true
```

### `tad config` — groups editor

`tad config` opens a slim TUI groups editor. On an empty config it goes
straight to add-a-group; otherwise it lists your existing groups. From
there you can:

- **Add a group** — enter a name, pick a layout, then select member hosts
  from the live discovered host list (with `/` filter) or type them in.
- **Delete a group.**
- **Pick a theme.**

Groups are **optional** — an organizing convenience on top of the
automatic host discovery. Open a group with `tad -g <group>`. The empty
groups picker (`g`) in the dashboard points you to `tad config` as a reminder.

## Migrating from a shell-function `tad`

`tad` started as a small bash function — usually a few lines wrapping
`tmux attach/new-session` with a confirmation prompt. If you've been
carrying something like this around in your `.bashrc`, `.zshrc`, or a
dotfiles repo:

```bash
function tad() {
   local s=$1
   if [ -z "$s" ]; then tmux ls; return; fi
   tmux has-session -t "$s" 2>/dev/null && tmux attach -d -t "$s" \
      || tmux new-session -s "$s"
}
```

…the binary replaces it entirely. To migrate:

1. **Install the binary** (see above). Make sure `~/.local/bin` comes
   before any directory holding the old function on `PATH`. Verify:
   ```sh
   command -v tad     # should print ~/.local/bin/tad
   tad --version      # should print a version number
   ```
2. **Remove the function definition** from your shell rc files. Grep
   for it:
   ```sh
   grep -nE 'function tad|^\s*tad\s*\(\)' ~/.bash* ~/.zsh* 2>/dev/null
   ```
   Then delete those blocks. Functions in your current shell session
   live in memory — `unfunction tad` (zsh) or `unset -f tad` (bash) to
   evict the old one without restarting.
3. **Remove any `complete -F` line** that paired with the old function:
   ```sh
   grep -n 'complete .* tad' ~/.bash*
   ```
   The Rust binary ships its own bash + zsh completions; the old one
   would conflict.
4. **Define your groups** (optional). Run `tad config` to open the
   groups editor — it shows discovered hosts (from your ssh-config,
   known_hosts, and shell history automatically) so you can pick
   members without typing them manually. Or open `tad groups edit`
   and hand-edit, or run `tad groups add` for a single-group
   interactive prompt. The config lives at
   `~/.config/tad/config.yaml` under the `groups:` key. The old
   function knew nothing about groups; everything else is a strict
   superset of the old behavior so existing muscle memory still works:
   - `tad <name>` — attach existing session, or SSH into a discovered
     host, or create a new session (replaces the old simple attach-or-create)
   - `tad` — opens the dashboard (was: list sessions)
   - new: `tad -g <group>`, `tad groups`, `tad config`, etc.
5. **Optional: pick a theme**. Drop `theme: tokyonight` into
   `~/.config/tad/config.yaml` (or any of the built-ins; see Theme
   section).

Your existing tmux sessions are untouched — `tad` only reads tmux state
and asks tmux to attach/create. Nothing on disk changes for tmux.

## Usage

```
tad                          TUI dashboard (sessions / groups / hosts)
tad <name>                   attach existing session → SSH into discovered host
                             (in a new session) → create a new session by that
                             name; groups are NOT in bare dispatch (use -g)
tad -g <group>               open the group per its layout
tad -g <group> <host>        drill into one host from the group

tad config                   groups editor (TUI)

tad groups [list]            list known groups (default subcommand)
tad groups hosts <group>     list hosts in a group
tad groups add <name> <layout> <host>...
                             add a group (layout: panes|synced-panes|windows|browse)
                             — with no args, launches an interactive prompt
tad groups rm <name>         remove a group
tad groups edit              open the groups file in $EDITOR

tad tmux-keybind             print a tmux popup binding for the dashboard
tad tmux-keybind --install   write it into ~/.tmux.conf (idempotent)

tad status                   one-line summary of running Claude Code agents
                             across all tmux panes — for `#(tad status)` in
                             your status-line
tad watch                    long-running poller that auto-pops the dashboard
                             when an agent goes idle (run from your shell rc
                             / tmux startup hook / systemd-user service)

tad --select-agent <target>  open the dashboard on the Agents view with the
                             given pane preselected (used by `tad watch`)
```

The `tad groups …` family used to live as flat subcommands
(`tad groups-add`, `tad group-hosts`, etc.). Those still parse but error
with a one-line hint pointing to the new form — update any scripts.

## Jumping back to the dashboard from inside tmux

Once you've attached into a tmux session, you usually want to flip back
to the dashboard without leaving — open another group, switch sessions,
kill a stale one. `tad tmux-keybind --install` writes a `display-popup`
binding so you can:

```
prefix + D     # opens tad in a tmux popup; quit tad and you're back
```

The binding lives in a marker-delimited managed block, so re-running the
command updates in place and any other config in the file is preserved.
`tad tmux-keybind` (no flags) prints the snippet to stdout if you'd
rather paste it yourself; `tad tmux-keybind --uninstall` removes it.

**Where it writes.** To avoid clobbering Oh-My-Tmux / framework-managed
`~/.tmux.conf` files, the target is auto-detected in this order:

1. `--conf-path PATH` if given
2. `$TAD_TMUX_CONF` env var
3. `~/.tmux.conf.local`  (Oh-My-Tmux / gpakosz convention)
4. `~/.tmux.local.conf`  (alternate spelling)
5. `$XDG_CONFIG_HOME/tmux/tmux.conf` if present
6. `~/.tmux.conf` (created if needed)

Running `tad tmux-keybind` without `--install` always prints the
resolved target path, so you can see where it would write before doing
anything. Override key/dimensions with `--key`, `--width`, `--height`.

Requires tmux 3.2+ for `display-popup`.

## Claude Code cockpit: Agents view + status-line segment

If you run multiple Claude Code agents across multiple tmux panes — one per
repo, one per worktree, parallel investigations — tad has a view for that.

**The Agents section** (jump with `2`) lists every tmux pane on this
server whose process tree contains a `claude` process, showing
`<session>:<window>.<pane>  <cwd>  <status>` per row with a semantic
status dot (see the states legend in the Dashboard section above:
`●` blocked, `◐◓◑◒` working, `○` idle, `◌` away).

`Enter` runs `tmux switch-client` to jump straight to that pane. If
you've installed the popup keybind, the loop is:

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

### `tad watch`: passive `@tad-attn` attention marker

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

## Groups config

Lives in `~/.config/tad/config.yaml` under the `groups:` key (alongside
`theme:` and `ui:`). See `examples/groups.yaml.example` for the group
schema. Edit by hand, via `tad config`, or via `tad groups <add|rm|edit>`.

Pre-v0.10 layouts used a separate `~/.config/tad/groups.yaml`. On first
launch tad auto-migrates it: groups move into `config.yaml` and the old
file is renamed to `groups.yaml.migrated` so the migration is one-shot
and reversible.

Layouts:
- `panes`         — single window, one pane per host. **Default.**
- `synced-panes`  — like panes; `synchronize-panes on` is the default when
                    scripted/non-interactive.
- `windows`       — one window per host. Use `Ctrl-b n/p` to switch.
- `browse`        — don't auto-open anything. `tad -g <name> <TAB>` shows
                    hosts for individual drill-in.

For both `panes` and `synced-panes`, opening 2+ panes interactively prompts
`Enable text-sync across panes? [Y/n]` (default yes). The layout choice now
controls only the non-interactive fallback: `synced-panes` keeps sync on
in scripts, `panes` keeps it off.

## Dashboard

Bare `tad` opens a persistent sidebar cockpit, not a tab-cycling
screen: **Sessions** and **Agents** are the two sections stacked in
one scrollable sidebar, each collapsible, with a live preview of the
selected row's pane alongside it. **Groups** and **Hosts** are
reference material rather than day-to-day views, so they live behind
on-demand pickers — press `g` for groups, `h` for hosts — instead of
taking up permanent sidebar space. Rows carry a semantic status dot
instead of plain text:

```
●  blocked   — needs your input
◐◓◑◒  working    — animated, actively producing output
○  idle      — quiet for a while
◌  away      — no agent / not currently running
```

The Agents section groups its rows under per-session headers
(`session · N agents · M awaiting`), most-recently-active session
first, so the busiest work reads from the top.

Everything is clickable as well as keyboard-driven: click a row to
select it, click it again (or double-click) to open it, scroll the
sidebar or preview with the wheel, and drag the divider between them
to resize. The footer is a row of clickable chips (`open`, `pin`,
`new`, `kill`, `groups`, `hosts`, `theme`, `filter`, `refresh`,
`quit`, …) that mirror
whatever keys apply to the current row, and every modal (theme
picker, snooze picker, rename, kill confirmation, group/host picker)
is clickable too. Mouse support needs tmux's own mouse mode; see
`tad doctor` and the note below.

Keys (any section):
- `↑/↓` or `j/k`         move selection
- `Tab` / `Shift-Tab`    jump to next/previous section
- `1` / `2`              jump to Sessions / Agents
                         (the sidebar's visual top-to-bottom order)
- `Space`                collapse/expand the section under the cursor
- `` ` ``                toggle the sidebar as an overlay — only
                         matters in narrow terminals, where the
                         sidebar auto-hides behind a `☰` chip
- `g` / `h`              open the groups / hosts picker (a filterable
                         overlay; type to narrow, `↑↓` to pick, `Enter`
                         opens the group / SSHes the host, `Esc` closes)
- `Home` / `G`           first / last item
- `Enter`                open the highlighted item
- `t`                    open the theme picker (live preview as you
                         move; `Enter` confirms, `Esc` cancels)
- `n`                    new blank tmux session prompt (to SSH into a
                         discovered host, use the `h` picker instead)
- `o`                    pin the selected pane into a tmux split
                         beside tad (sessions: the session's active
                         pane; agents: the agent's pane) and focus
                         it — work in the real pane with the
                         dashboard still open. Up to 4 panes can be
                         pinned at once, tiled in a grid; `o` again
                         (or clicking its dot) unpins that one.
                         Quitting tad returns any pinned panes to
                         their original windows first. Needs tad in
                         a regular tmux pane (not the popup).
- `d`                    kill, with a y/N confirmation (sessions:
                         tmux kill-session; agents: SIGINT to the
                         agent). Only `y`/`Enter` confirm — Esc, `n`,
                         or any other key cancels.
- `R`                    rename the selected agent's display label
- `s` / `S`              snooze / clear snooze (agents section only)
- `/`                    enter filter mode (live — ↑↓ navigates, Enter
                         opens, Tab jumps sections with filter applied,
                         Esc exits and clears)
- `r`                    manual refresh
- `q` or `Esc`           quit

The sidebar auto-refreshes every ~1.5 seconds. Selection, collapsed
sections, and sidebar width are all remembered across launches in
`$XDG_STATE_HOME/tad/dashboard.state` (typically
`~/.local/state/tad/dashboard.state`); first launch defaults to
Sessions, nothing collapsed.

By default tad sends a desktop notification the moment an agent turns
blocked (needs your input). Disable it with `ui.notify_on_blocked:
false` in `~/.config/tad/config.yaml`.

### Mouse support

The sidebar, preview, footer chips, and modals all respond to clicks
and scroll natively. Clicking to focus a *pinned* pane, though, is
tmux's job, not tad's — it needs tmux's own mouse mode:

```tmux
set -g mouse on
```

`tad doctor` checks this (`tmux show -gv mouse`) and warns if it's
off.

## Theme

Set in `~/.config/tad/config.yaml`. Default is `tokyonight`. See
`examples/config.yaml.example`.

Built-in names: `tokyonight`, `tokyonight-storm`, `dracula`, `nord`,
`gruvbox`, `catppuccin`, `solarized-dark`, `onedark`, `terminal`.

| tokyonight | tokyonight-storm | dracula | nord |
| --- | --- | --- | --- |
| ![tokyonight](docs/screenshots/theme-tokyonight.png) | ![tokyonight-storm](docs/screenshots/theme-tokyonight-storm.png) | ![dracula](docs/screenshots/theme-dracula.png) | ![nord](docs/screenshots/theme-nord.png) |

| gruvbox | catppuccin | solarized-dark | onedark |
| --- | --- | --- | --- |
| ![gruvbox](docs/screenshots/theme-gruvbox.png) | ![catppuccin](docs/screenshots/theme-catppuccin.png) | ![solarized-dark](docs/screenshots/theme-solarized-dark.png) | ![onedark](docs/screenshots/theme-onedark.png) |

```yaml
theme: catppuccin
```

Or override individual colors inline (hex `#rrggbb`):

```yaml
theme:
  accent: "#ff79c6"
  selection_bg: "#222536"
```

## Files

```
~/.config/tad/config.yaml         — unified config: theme, ui, groups
~/.local/state/tad/dashboard.state — last dashboard view (persisted)
~/.config/tad/groups.yaml.migrated — pre-v0.10 leftover after auto-migration
```

## Regenerating the README screenshots

The dashboard demo gif and stills are produced from `vhs` tapes using
sample data under `docs/demo/`. A throwaway tmux server on socket
`tad-demo` is seeded with sample sessions, and a tmux shim
(`docs/demo/bin/tmux`) pins every call to that socket so your real tmux
is never touched.

```sh
cargo build --release             # tapes use target/release/tad if present
vhs docs/demo/dashboard.tape      # writes docs/screenshots/dashboard.gif
bash docs/demo/render-themes.sh   # writes docs/screenshots/theme-*.png
```

To refresh the per-view PNGs (`dashboard-sessions.png`, etc.) extract
frames from the new gif:

```sh
ffmpeg -y -i docs/screenshots/dashboard.gif -vf fps=2 /tmp/tad-f%03d.png
# then cp the frames you like into docs/screenshots/
```

## Cutting a release

1. Bump `version` in `Cargo.toml`, run `cargo build --release` so
   `Cargo.lock` updates, and commit.
2. Tag and push:
   ```sh
   git tag vX.Y.Z && git push origin vX.Y.Z
   ```
   `.github/workflows/release.yml` builds the binary, bundles
   completions + examples + `LICENSE`, computes `SHA256SUMS`, and
   publishes the GitHub release.
3. Refresh both AUR PKGBUILDs and push to AUR:

   **`tmux-tad`** (source build) — bump `pkgver` and refresh the source
   tarball hash:
   ```sh
   curl -sL https://github.com/ttpears/tad/archive/vX.Y.Z.tar.gz | sha256sum
   # paste into packaging/aur/tmux-tad/PKGBUILD, bump pkgver, then:
   cd packaging/aur/tmux-tad
   makepkg --printsrcinfo > .SRCINFO
   # commit + push to ssh://aur@aur.archlinux.org/tmux-tad.git
   ```

   **`tmux-tad-bin`** (prebuilt binary) — bump `pkgver` and refresh the
   release-artifact hashes:
   ```sh
   curl -sL https://github.com/ttpears/tad/releases/download/vX.Y.Z/SHA256SUMS
   # paste each hash (binary, tad.bash, _tad, groups.example,
   # config.example, LICENSE) into packaging/aur/tmux-tad-bin/PKGBUILD,
   # bump pkgver, then:
   cd packaging/aur/tmux-tad-bin
   makepkg --printsrcinfo > .SRCINFO
   # commit + push to ssh://aur@aur.archlinux.org/tmux-tad-bin.git
   ```
