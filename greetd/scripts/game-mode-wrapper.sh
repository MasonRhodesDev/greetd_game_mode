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

# ----------------------------------------------------------------------------
# Filesystem mask: run Steam (and every game under it) inside a bubblewrap
# sandbox that presents a curated view of $HOME. Secrets and personal data
# (.ssh, .gnupg, keyrings, repos, browser profiles, ...) are simply ABSENT
# from the sandbox; $HOME is read-only except for the explicit game binds, so
# stray writes fail loudly (EROFS) instead of vanishing.
#
# Landlock was evaluated first but pressure-vessel must enumerate / (it
# opendir()s /proc/self/root to mirror the host into its container), and
# Landlock grants are whole-hierarchy — allowing readdir on / would re-expose
# everything. The outer bwrap solves that naturally: pv enumerates the curated
# root. Verified: SLR/pressure-vessel (nested bwrap) runs fine inside this.
#
# Extend-on-breakage: if a game needs another dir, add a --bind line.
# Escape hatch while debugging: `touch {{games_dir}}/.game-mode-no-mask` and relaunch.
# ----------------------------------------------------------------------------
if [ -e {{games_dir}}/.game-mode-no-mask ] || ! command -v bwrap >/dev/null 2>&1; then
    echo "game-mode-wrapper: MASK DISABLED (flag file or bwrap missing)" >&2
    exec steam -gamepadui -steamos3
fi

# Only bind paths that exist (bwrap errors out on missing sources).
maybe_bind() { [ -e "$2" ] && MASK+=("$1" "$2" "$2"); }

MASK=(
    --die-with-parent
    # System: read-only, plus Arch merged-usr symlinks (absent from the
    # curated root otherwise — shebangs like #!/bin/bash need them).
    --ro-bind /usr /usr
    --symlink usr/bin /bin --symlink usr/sbin /sbin
    --symlink usr/lib /lib --symlink usr/lib /lib64
    --ro-bind /etc /etc --ro-bind /opt /opt --ro-bind /sys /sys --ro-bind /var /var
    --proc /proc --dev-bind /dev /dev
    # Sockets/IPC/state: X11+Wayland (/tmp), runtime dir + dbus + pipewire (/run)
    --bind /tmp /tmp --bind /run /run
    # Game library
    --bind {{games_dir}} {{games_dir}}
    # Curated home: starts empty, gains only the binds below, then goes RO
    --tmpfs "$HOME"
)
maybe_bind --bind "$HOME/.local/share/Steam"
maybe_bind --bind "$HOME/.steam"
maybe_bind --bind "$HOME/.cache"                       # mesa/radv shader caches
maybe_bind --ro-bind "$HOME/.local/bin"                # launch-option wrappers (ror2-mods-update)
# Native-game / engine state
maybe_bind --bind "$HOME/.local/share/PrismLauncher"   # Minecraft instances + saves
maybe_bind --bind "$HOME/.local/share/SlayTheSpire2"
maybe_bind --bind "$HOME/.local/share/godot"
maybe_bind --bind "$HOME/.local/share/vulkan"          # Steam-installed implicit layers
maybe_bind --bind "$HOME/.config/unity3d"
maybe_bind --bind "$HOME/.config/godot"
maybe_bind --bind "$HOME/.config/Valve"
maybe_bind --bind "$HOME/.config/r2modman"
maybe_bind --bind "$HOME/.config/r2modmanPlus-local"
MASK+=(--remount-ro "$HOME")

echo "game-mode-wrapper: launching Steam inside bwrap home mask" >&2
exec bwrap "${MASK[@]}" -- steam -gamepadui -steamos3
