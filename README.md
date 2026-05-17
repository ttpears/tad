# tad

A tmux session and group manager. Bare `tad` opens a native TUI dashboard
that cycles between live sessions, named groups, and the hosts inside those
groups, with live updates every ~1.5s. `tad <name>` attaches or creates a
session. `tad -g <group>` opens a multi-host session whose layout you
control per group.

## Install

Requires:
- `tmux`
- Rust toolchain to build

No fzf needed — the dashboard is built into the binary via ratatui.

```sh
git clone <repo-url> ~/git/tad
cd ~/git/tad
make install              # builds and installs to ~/.local/bin
```

Or manually:
```sh
cargo build --release
install -Dm755 target/release/tad ~/.local/bin/tad
```

### Shell completions

bash:
```sh
ln -s ~/git/tad/completions/tad.bash ~/.bash_completion.d/tad
# or source it from your rc
echo '. ~/git/tad/completions/tad.bash' >> ~/.bashrc
```

zsh:
```sh
# Add the dir to fpath in your .zshrc, then compinit
fpath=(~/git/tad/completions $fpath)
autoload -Uz compinit && compinit
```

## Usage

```
tad                          fzf dashboard (sessions / groups / hosts)
tad <session>                attach or create a tmux session by name
tad -g <group>               open the group per its layout
tad -g <group> <host>        drill into one host from the group

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
the schema. Edit by hand or via the `groups-*` subcommands.

Layouts:
- `panes`         — single window, one pane per host. **Default.**
- `synced-panes`  — like panes, with tmux `synchronize-panes on` so input
                    fans out to every pane.
- `windows`       — one window per host. Use `Ctrl-b n/p` to switch.
- `browse`        — don't auto-open anything. `tad -g <name> <TAB>` shows
                    hosts for individual drill-in.

## Dashboard

Bare `tad` opens a TUI:

```
┌─ tad ────────────────────────────────────────────────────┐
│  Sessions  Groups  Hosts                                  │
├──────────────────────────┬───────────────────────────────┤
│ Sessions (12)            │ preview                        │
│ ▶ ● web-prod  1w     │ session: web-prod          │
│   ● docker        2w     │                                │
│     web-...   1w     │   1: claude (1 panes)          │
│     ...                  │                                │
│                          │ created: ...                   │
│                          │ activity: ...                  │
├──────────────────────────┴───────────────────────────────┤
│ ↑↓/jk nav  ⇥ view  1/2/3 jump  ↵ open  d kill  / filter  │
└──────────────────────────────────────────────────────────┘
```

Keys:
- `↑/↓` or `j/k`         move selection
- `Tab` / `Shift-Tab`    cycle views forward/back
- `1`, `2`, `3`          jump to Sessions / Groups / Hosts
- `g` / `G`              first / last item
- `Enter`                open the highlighted item
- `d`                    kill (sessions view only)
- `/`                    enter filter mode (type to filter, Esc/Enter exits)
- `r`                    manual refresh
- `q` or `Esc`           quit

Sessions/groups/hosts auto-refresh every ~1.5 seconds.

## Files

```
~/.config/tad/groups.yaml      — your group definitions
/tmp/tad-dashboard-$USER.state — current dashboard view (transient)
```
