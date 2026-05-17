# Feature tour of the tad dashboard against the demo state.
# Run with: vhs docs/demo/dashboard.tape
# Output:   docs/screenshots/dashboard.gif
#
# State lives under /tmp/tad-demo. The tmux shim at docs/demo/bin/tmux
# pins every call to the `tad-demo` socket, so this never touches the
# user's real tmux server.

Output docs/screenshots/dashboard.gif

Set Shell "bash"
Set FontSize 14
Set Width 1180
Set Height 640
Set Theme "TokyoNight"
Set Padding 16
Set TypingSpeed 55ms

# Bootstrap demo state silently.
Hide
Type "source docs/demo/setup.sh && clear"
Enter
Sleep 1s
Show

# Launch the dashboard on the Sessions view.
Type "tad"
Enter
Sleep 3s

# Move down through sessions; pause on each so the preview updates.
Down
Sleep 1500ms
Down
Sleep 1500ms
Down
Sleep 1500ms

# Filter: `/` opens filter, then narrow to `web` and back out.
Type "/web"
Sleep 2500ms
Backspace
Backspace
Backspace
Sleep 1200ms
Escape
Sleep 1200ms

# Cycle to Groups.
Tab
Sleep 2500ms
Down
Sleep 1500ms
Down
Sleep 1500ms
Down
Sleep 1500ms

# Cycle to Hosts.
Tab
Sleep 2500ms
Down 5
Sleep 1500ms
Down 3
Sleep 1500ms

# Back to Sessions for the new-session prompt.
Tab
Sleep 2000ms

# `n` opens the new-session modal — preseeded with the highlighted item's
# short name. Clear it, type a fresh name, Tab to the host field, type an
# SSH host, then Escape to dismiss.
Type "n"
Sleep 1500ms
Backspace 10
Sleep 500ms
Type "deploy-eu"
Sleep 1200ms
Tab
Sleep 800ms
Type "deploy-eu.example.com"
Sleep 2000ms
Escape
Sleep 1500ms

# Quit cleanly.
Type "q"
Sleep 600ms
