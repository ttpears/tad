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

A tmux session and group manager. Bare `tad` opens a native TUI dashboard
that cycles between live sessions, named groups, and the hosts inside those
groups, with live updates every ~1.5s. `tad <name>` attaches or creates a
session — and if no session exists yet but a group by that name does, it
offers to open the whole group instead. `tad -g <group>` opens a multi-host
session whose layout you control per group; for multi-pane layouts you're
prompted whether to enable tmux `synchronize-panes` (default: yes).

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

## Setup wizard / `tad config`

The setup wizard is **opt-in**. Bare `tad` goes straight to the
dashboard, which works fine with no groups (your tmux sessions and
Claude agents still show). When you want to define groups, run
`tad config`: it imports SSH hosts from sources you already have on
disk and shapes them into groups. When a config already exists, it
opens an edit view with the option to re-run imports. The empty Groups
view in the dashboard points you to `tad config` as a reminder.

All scanning is **local**: the wizard reads files on this machine. It
does not contact hosts, perform DNS lookups, or call any resolver.

Sources it can pull from (each toggleable):

- **Shell history** — `$HISTFILE`, `~/.bash_history`, `~/.zsh_history`,
  fish history. Extracts hosts from `ssh user@host -p 22` style
  invocations, strips `user@` and flag values, ignores `sshfs` /
  `ssh-add` / `ssh-keygen` / `ssh-copy-id`.
- **`~/.ssh/config`** — concrete `Host` entries, including
  one-level-deep `Include`. Wildcard patterns are skipped.
- **`~/.ssh/known_hosts`** — non-hashed entries. `@cert-authority` and
  `|1|...` hashed entries are skipped.
- **Tmux sessions** — existing sessions become pre-formed group
  candidates (window names → hosts, layout defaults to `windows`).
  Sessions whose windows are all generic shell names are pre-marked
  unusable, but you can still force-import them.

Flow: pick sources → review/toggle imported tmux sessions → review/toggle
discovered hosts (with `/` filter, `a` select-all, `n` clear) → build
groups one at a time (name, layout, member hosts) → confirm and write.
On a re-run via `tad config`, new groups are merged into the existing
config; name collisions get `-2`, `-3`, ... suffixes.

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
4. **Define your groups** (optional). Either run the `tad config`
   wizard to mine them from your shell history and `~/.ssh/config`,
   or open `tad groups edit` and hand-edit, or run `tad groups add`
   for a single-group interactive prompt. The config lives at
   `~/.config/tad/config.yaml` under the `groups:` key. The old function knew nothing about
   groups; everything else is a strict superset of the old behavior so
   existing muscle memory still works:
   - `tad <name>` — attach or create (same as old)
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
tad <name>                   attach or create a session; if <name> matches a
                             group and no session exists, offers to open it
tad -g <group>               open the group per its layout
tad -g <group> <host>        drill into one host from the group

tad config                   setup wizard / groups editor (TUI)

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

**The Agents tab** (the 4th dashboard tab, jump with `4`) lists every tmux
pane on this server whose process tree contains a `claude` process,
showing `<session>:<window>.<pane>  <cwd>  <status>` per row. Status comes
from the mtime of the most recent transcript jsonl under
`~/.claude/projects/<encoded-cwd>/`:

```
●  active · 2s        currently working
○  idle 4m            transcript hasn't been written in a while
?  no transcript      can't find a transcript on disk
```

`Enter` runs `tmux switch-client` to jump straight to that pane. If
you've installed the popup keybind, the loop is:

```
prefix + D  →  4  (Agents tab)  →  ↑↓ to the blocked one  →  ↵  →  reply
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

Bare `tad` opens a TUI with five views — **Projects** (the lead view),
Sessions, Groups, Hosts, Agents — that you cycle through with `Tab`
(or jump to with `1` / `2` / `3` / `4` / `5`):

| Projects | Sessions | Groups | Hosts | Agents |
| --- | --- | --- | --- | --- |
| (cockpit per repo) | (raw tmux sessions) | (multi-host configs) | (one row per host) | (one row per claude pane) |

### Projects (1)

The dashboard's lead view. A **project** = a git repo root (or any
directory with a `.tad/` marker) that tad has seen via a tmux pane cwd
or a Claude transcript. Each row aggregates: number of tmux sessions
in this project, number of claude agents running in it, how many of
those are awaiting input, and the most recent transcript activity.

```
tad-github                3 sess   2 agt   1 waiting   · 4s
salt-masters              1 sess   1 agt                · 2h
gitlab-mcp                1 sess   0 agt                · 1d
```

Enter on a project attaches to its most-recently-active session — or
jumps to its most-recently-active agent pane if there are no sessions.
`n` on a project spawns a fresh `claude` agent in that project's root
(an optional initial prompt is sent to claude as its first message);
it adds a window to the project's existing session or starts a new
one named after the project. The preview pane shows root path, git
branch + dirty count, and nested lists of sessions and agents (with
the same `! awaiting · 2s` markers from the Agents view).

Launching `tad` from inside a project directory preselects that
project's row automatically, so the dashboard opens with the right
context whether you wanted to browse or just resume where you are.

### Other views

Same shapes as before, with the keystroke shifted:

Keys (any view):
- `↑/↓` or `j/k`         move selection
- `Tab` / `Shift-Tab`    cycle views forward/back
- `1` / `2` / `3` / `4` / `5`   jump to Projects / Sessions / Groups / Hosts / Agents
- `g` / `G`              first / last item
- `Enter`                open the highlighted item
- `n`                    new — context-sensitive:
                         * Projects view → spawn a fresh `claude` agent
                           in the selected project's root (optional
                           initial prompt); adds a window to the project's
                           existing session or starts a new one named
                           after the project
                         * Hosts view → new tmux session with the host
                           prefilled as the SSH target
                         * other views → blank new tmux session prompt
- `d`                    kill (sessions view only)
- `s` / `S`              snooze / clear snooze (agents view only)
- `/`                    enter filter mode (live — ↑↓ navigates, Enter
                         opens, Tab cycles views with filter applied,
                         Esc exits and clears)
- `r`                    manual refresh
- `q` or `Esc`           quit

All views auto-refresh every ~1.5 seconds. The last view you were on
is remembered across launches in `$XDG_STATE_HOME/tad/dashboard.state`
(typically `~/.local/state/tad/dashboard.state`); first launch defaults
to Projects.

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
