<p align="center">
  <img src="docs/logo.svg" alt="tad" width="128" height="128">
</p>

# tad

<p align="center">
  <a href="https://github.com/ttpears/tad/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/ttpears/tad/actions/workflows/ci.yml/badge.svg"></a>
  <a href="https://github.com/ttpears/tad/releases/latest"><img alt="Latest release" src="https://img.shields.io/github/v/release/ttpears/tad?display_name=tag&sort=semver"></a>
  <a href="https://aur.archlinux.org/packages/tmux-tad-bin"><img alt="AUR version" src="https://img.shields.io/aur/version/tmux-tad-bin"></a>
  <a href="https://github.com/ttpears/tad/releases"><img alt="Downloads" src="https://img.shields.io/github/downloads/ttpears/tad/total"></a>
  <a href="LICENSE"><img alt="License: MIT" src="https://img.shields.io/github/license/ttpears/tad"></a>
</p>

A tmux session and group manager. Bare `tad` opens a native TUI dashboard
that cycles between live sessions, named groups, and the hosts inside those
groups, with live updates every ~1.5s. `tad <name>` attaches or creates a
session. `tad -g <group>` opens a multi-host session whose layout you
control per group.

![tad dashboard demo](docs/screenshots/dashboard.gif)

## Install

Only requirement at runtime: `tmux`. The dashboard ships inside the
binary.

### Arch Linux (AUR)

```sh
yay -S tmux-tad-bin           # or: paru -S tmux-tad-bin
```

The [`tmux-tad-bin`](https://aur.archlinux.org/packages/tmux-tad-bin) package
installs the prebuilt x86_64 binary from the GitHub release plus the
bash/zsh completions and example configs. PKGBUILD source lives at
`packaging/aur/tmux-tad-bin/PKGBUILD` in this repo.

### From a release (any Linux x86_64)

Each [release](https://github.com/ttpears/tad/releases) ships a static
Linux x86_64 binary, matching completion files, and a `SHA256SUMS`.

```sh
TAD_VERSION=v0.4.0
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

## First-launch wizard / `tad config`

The first time you run bare `tad` with no `~/.config/tad/groups.yaml`,
a TUI wizard offers to import SSH hosts from sources you already have
on disk and shape them into groups. You can also launch it any time
with `tad config` — when a config already exists, that opens an edit
view with the option to re-run imports.

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
4. **Define your groups**. Either let the first-launch wizard /
   `tad config` mine them from your shell history and `~/.ssh/config`,
   or open `tad groups-edit` and hand-edit, or run `tad groups-add`
   for a single-group interactive prompt. The config lives at
   `~/.config/tad/groups.yaml`. The old function knew nothing about
   groups; everything else is a strict superset of the old behavior so
   existing muscle memory still works:
   - `tad <name>` — attach or create (same as old)
   - `tad` — opens the dashboard (was: list sessions)
   - new: `tad -g <group>`, `tad groups`, `tad config`, `tad complete`, etc.
5. **Optional: pick a theme**. Drop `theme: tokyonight` into
   `~/.config/tad/config.yaml` (or any of the built-ins; see Theme
   section).

Your existing tmux sessions are untouched — `tad` only reads tmux state
and asks tmux to attach/create. Nothing on disk changes for tmux.

## Usage

```
tad                          TUI dashboard (sessions / groups / hosts)
tad <session>                attach or create a tmux session by name
tad -g <group>               open the group per its layout
tad -g <group> <host>        drill into one host from the group

tad config                   first-launch wizard / groups editor (TUI)
tad groups                   list known groups
tad group-hosts <group>      list hosts in a group
tad groups-add <name> <layout> <host>...
                             add a group (layout: panes|synced-panes|windows|browse)
tad groups-rm <name>         remove a group
tad groups-edit              open the groups file in $EDITOR

tad complete                 emit completion source (used by shell)
```

## Groups config

Lives at `~/.config/tad/groups.yaml`. See `examples/groups.yaml.example` for
the schema. Edit by hand, via `tad config`, or via the `groups-*`
subcommands.

Layouts:
- `panes`         — single window, one pane per host. **Default.**
- `synced-panes`  — like panes, with tmux `synchronize-panes on` so input
                    fans out to every pane.
- `windows`       — one window per host. Use `Ctrl-b n/p` to switch.
- `browse`        — don't auto-open anything. `tad -g <name> <TAB>` shows
                    hosts for individual drill-in.

## Dashboard

Bare `tad` opens a TUI with three views — Sessions, Groups, Hosts — that
you cycle through with `Tab` (or jump to with `1`/`2`/`3`):

| Sessions | Groups | Hosts |
| --- | --- | --- |
| ![sessions](docs/screenshots/dashboard-sessions.png) | ![groups](docs/screenshots/dashboard-groups.png) | ![hosts](docs/screenshots/dashboard-hosts.png) |

Keys:
- `↑/↓` or `j/k`         move selection
- `Tab` / `Shift-Tab`    cycle views forward/back
- `1`, `2`, `3`          jump to Sessions / Groups / Hosts
- `g` / `G`              first / last item
- `Enter`                open the highlighted item
- `n`                    new session — opens a name prompt (preseeded
                         with the highlighted item's short name; edit
                         and Enter to create)
- `d`                    kill (sessions view only)
- `/`                    enter filter mode (type to filter, Esc/Enter exits)
- `r`                    manual refresh
- `q` or `Esc`           quit

Sessions/groups/hosts auto-refresh every ~1.5 seconds.

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
~/.config/tad/groups.yaml      — your group definitions
~/.config/tad/config.yaml      — theme + UI preferences (optional)
/tmp/tad-dashboard-$USER.state — current dashboard view (transient)
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
3. Refresh the AUR PKGBUILD hashes:
   ```sh
   curl -sL https://github.com/ttpears/tad/releases/download/vX.Y.Z/SHA256SUMS
   ```
   Paste each hash (binary, `tad.bash`, `_tad`, `groups.yaml.example`,
   `config.yaml.example`, `LICENSE`) into the corresponding `SKIP` slot
   in `packaging/aur/tmux-tad-bin/PKGBUILD`, bump `pkgver`, then push to AUR:
   ```sh
   cd packaging/aur/tmux-tad-bin
   makepkg --printsrcinfo > .SRCINFO
   # commit + push to ssh://aur@aur.archlinux.org/tmux-tad-bin.git
   ```
