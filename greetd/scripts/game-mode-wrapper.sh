#!/bin/bash
#
# Game-mode session entrypoint. Executed as the child of gamescope, i.e.
#   /usr/bin/gamescope -e -- /etc/greetd/scripts/game-mode-wrapper.sh
#
# gamescope exposes an Xwayland X11 display to its children (DISPLAY) rather
# than a Wayland socket, so the Discord voice overlay must use the X11/GTK
# backend. Per `discover-overlay --help`:
#   "For gamescope compatibility ensure ENV has 'GDK_BACKEND=x11'".
#
# The overlay reads voice state from a running Discord client's local IPC
# socket ($XDG_RUNTIME_DIR/discord-ipc-0). Add Discord as a non-Steam shortcut
# and launch it from Big Picture when you want voice display — discover-overlay
# reconnects to the socket automatically. Run `discover-overlay --configure`
# once in desktop mode first to authorize the connection and set overlay
# options.

# Start the Discord overlay (best-effort; never block the Steam session).
# Backgrounded with a short delay so gamescope's Xwayland is ready first.
if command -v discover-overlay >/dev/null 2>&1; then
    ( sleep 2; env -u WAYLAND_DISPLAY GDK_BACKEND=x11 discover-overlay ) &
fi

# Hand off to Steam Big Picture as the session's main process.
exec steam -gamepadui
