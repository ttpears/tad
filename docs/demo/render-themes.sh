#!/usr/bin/env bash
# Render one PNG per built-in theme by invoking vhs once per theme. We do
# this because vhs gets confused when an alt-screen TUI is restarted multiple
# times inside a single tape — running it fresh each time avoids the issue.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
OUT_DIR="$REPO_ROOT/docs/screenshots"
TAPE_DIR="$(mktemp -d)"
trap 'rm -rf "$TAPE_DIR"' EXIT

THEMES=(
    tokyonight
    tokyonight-storm
    dracula
    nord
    gruvbox
    catppuccin
    solarized-dark
    onedark
)

for theme in "${THEMES[@]}"; do
    tape="$TAPE_DIR/$theme.tape"
    out="$OUT_DIR/theme-$theme.png"
    gif="$OUT_DIR/theme-$theme.gif"
    cat > "$tape" <<EOF
Output "$gif"

Set Shell "bash"
Set FontSize 14
Set Width 1180
Set Height 640
Set Theme "TokyoNight"
Set Padding 16
Set TypingSpeed 0ms

Hide
Type "source $SCRIPT_DIR/setup.sh && printf 'theme: $theme\n' > \$XDG_CONFIG_HOME/tad/config.yaml && clear"
Enter
Sleep 800ms
Show

Type "tad"
Enter
Sleep 3500ms
Type "q"
Sleep 400ms
EOF
    printf '  >> %s\n' "$theme"
    vhs "$tape" >/dev/null
    # Pull the last in-tad frame as the still. The gif's final frame might
    # be a post-quit empty shell, so trim the last second.
    ffmpeg -loglevel error -y -sseof -1.2 -i "$gif" -vframes 1 "$out"
    rm -f "$gif"
done

printf 'done. %d PNGs in %s\n' "${#THEMES[@]}" "$OUT_DIR"
