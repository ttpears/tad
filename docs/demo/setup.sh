#!/usr/bin/env bash
# Seed a throwaway tmux server (socket name: tad-demo) and a sample tad
# config under /tmp/tad-demo/config. Designed to be safe to source: every
# tmux call goes through the shim at docs/demo/bin/tmux which always passes
# `-L tad-demo`, so the user's real tmux server cannot be reached.
#
# Sourcing it also exports XDG_CONFIG_HOME and prepends the shim + release
# binary to PATH so a subsequent `tad` invocation sees the demo state.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
DEMO_ROOT="${TAD_DEMO_ROOT:-/tmp/tad-demo}"

rm -rf "$DEMO_ROOT/config"
mkdir -p "$DEMO_ROOT/config/tad"
cp "$SCRIPT_DIR/groups.yaml" "$DEMO_ROOT/config/tad/groups.yaml"
cat > "$DEMO_ROOT/config/tad/config.yaml" <<'EOF'
theme: tokyonight
EOF

export XDG_CONFIG_HOME="$DEMO_ROOT/config"

# Shim must come first so bare `tmux` from anywhere (including tad) hits
# the tad-demo socket.
PATH="$SCRIPT_DIR/bin:$PATH"
if [ -x "$REPO_ROOT/target/release/tad" ]; then
    PATH="$REPO_ROOT/target/release:$PATH"
fi
export PATH

# Tear down any previous demo server (shim ensures this is the demo socket
# only) and wait for it to fully exit — kill-server is async and racing it
# loses the first session we try to create on the new server.
if tmux info >/dev/null 2>&1; then
    tmux kill-server 2>/dev/null || true
    for _ in $(seq 1 50); do
        tmux info >/dev/null 2>&1 || break
        sleep 0.05
    done
fi

tmux new-session -d -s web-prod -n nginx
tmux new-window  -t web-prod   -n logs
tmux new-window  -t web-prod   -n deploy

tmux new-session -d -s db-staging -n psql
tmux new-window  -t db-staging    -n migrations

tmux new-session -d -s worker-eu  -n redis-cli
tmux new-session -d -s edge-1     -n top
tmux new-session -d -s scratch    -n shell
tmux new-session -d -s docs       -n vim
