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

## Contents

- [Install](#install)
- [Usage](#usage)
- [Dashboard](#dashboard)
- [Jumping back to the dashboard from inside tmux](#jumping-back-to-the-dashboard-from-inside-tmux)
- [Claude Code cockpit](#claude-code-cockpit)
- [Host discovery / `tad config`](#host-discovery--tad-config)
- [Groups config](#groups-config)
- [Configuration](#configuration)
- [Theme](#theme)
- [Files](#files)
- [Contributing](#contributing)

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
# check https://github.com/ttpears/tad/releases/latest for the current version
TAD_VERSION=v0.17.0
curl -fLO "https://github.com/ttpears/tad/releases/download/${TAD_VERSION}/tad-${TAD_VERSION}-x86_64.deb"
sudo apt install "./tad-${TAD_VERSION}-x86_64.deb"   # pulls in tmux if needed
```

(`apt install ./<file.deb>` is preferred over raw `dpkg -i` because apt
resolves the `tmux` dependency.)

### Fedora / RHEL (.rpm)

```sh
# check https://github.com/ttpears/tad/releases/latest for the current version
TAD_VERSION=v0.17.0
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
# check https://github.com/ttpears/tad/releases/latest for the current version
TAD_VERSION=v0.17.0
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

### Migrating from a shell-function `tad`

If you've been carrying a small `tad()` function in your `.bashrc` or
`.zshrc`, the binary replaces it entirely. See
[docs/MIGRATING.md](docs/MIGRATING.md) for the step-by-step swap.

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

## Claude Code cockpit

If you run multiple Claude Code agents across multiple tmux panes — one per
repo, one per worktree, parallel investigations — tad has a cockpit for
that:

- **The Agents view** — every pane whose process tree contains a
  `claude` process, with live status dots; `Enter` jumps straight to
  the pane.
- **`tad status`** — a one-line `claude: 3/12` summary for your tmux
  status-line.
- **`tad watch`** — a passive poller that marks windows whose agent
  needs your input via the `@tad-attn` tmux user-variable.

See [docs/CLAUDE-CODE.md](docs/CLAUDE-CODE.md) for setup and the full
details.

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

## Configuration

Everything lives in one file, `~/.config/tad/config.yaml` (all keys
optional). Each key is documented in the section that uses it:

| Key | Documented in |
| --- | --- |
| `discovery:` | [Host discovery](#host-discovery--tad-config) |
| `groups:` | [Groups config](#groups-config) |
| `theme:` | [Theme](#theme) |
| `ui:` | [Dashboard](#dashboard) and [docs/CLAUDE-CODE.md](docs/CLAUDE-CODE.md) |

See `examples/config.yaml.example` for a complete annotated example.

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

## Contributing

Build/test instructions and maintainer docs (releasing, regenerating
the README screenshots) live in [CONTRIBUTING.md](CONTRIBUTING.md).
