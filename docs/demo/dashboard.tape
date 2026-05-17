# Captures the tad dashboard against the fictional demo state. Run with:
#   vhs docs/demo/dashboard.tape
#
# Output lands at docs/screenshots/dashboard.gif (and dashboard.png for the
# final frame). The demo data lives entirely under /tmp/tad-demo and is
# routed through a tmux shim (docs/demo/bin/tmux) that pins the socket to
# `tad-demo`, so your real tmux server is never touched.

Output docs/screenshots/dashboard.gif

Set Shell "bash"
Set FontSize 14
Set Width 1180
Set Height 640
Set Theme "TokyoNight"
Set Padding 16
Set TypingSpeed 40ms

# Bootstrap demo state silently.
Hide
Type "source docs/demo/setup.sh && clear"
Enter
Sleep 1s
Show

# Launch the dashboard.
Type "tad"
Enter
Sleep 2500ms

# Drift down a couple sessions to show the preview update.
Down
Sleep 900ms
Down
Sleep 1200ms

# Cycle to Groups view.
Tab
Sleep 1800ms
Down
Sleep 900ms
Down
Sleep 1200ms

# Cycle to Hosts view.
Tab
Sleep 2200ms

# Back to Sessions for the final frame (the PNG).
Tab
Sleep 1500ms

# Quit cleanly.
Type "q"
Sleep 500ms
