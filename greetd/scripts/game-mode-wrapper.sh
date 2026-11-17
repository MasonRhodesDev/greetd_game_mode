#!/bin/bash

GAMES_DIR=/games
if [ -r /etc/game-mode/config.toml ]; then
    _dir=$(sed -n 's/^[[:space:]]*dir[[:space:]]*=[[:space:]]*"\(.*\)".*/\1/p' /etc/game-mode/config.toml | tail -1)
    [ -n "$_dir" ] && GAMES_DIR="$_dir"
fi

start_session_apps() {
    if command -v game-mode-steam-config >/dev/null 2>&1; then
        game-mode-steam-config apply >> /tmp/game-mode-watchdog.log 2>&1 || true
    fi
    if command -v game-mode-watchdog >/dev/null 2>&1; then
        game-mode-watchdog &
    fi
    if command -v discover-overlay >/dev/null 2>&1; then
        (
            for attempt in 1 2 3 4 5; do
                sleep 3
                echo "=== discover-overlay attempt $attempt $(date -Is)" >> /tmp/discover-overlay.log
                env -u WAYLAND_DISPLAY GDK_BACKEND=x11 /usr/bin/game-mode-overlay >> /tmp/discover-overlay.log 2>&1
            done
            echo "=== discover-overlay gave up $(date -Is)" >> /tmp/discover-overlay.log
        ) &
    fi
}

if [ "${1:-}" = "--inner" ]; then
    start_session_apps
    exec steam -gamepadui -steamos3
fi

if ! command -v bwrap >/dev/null 2>&1; then
    echo "game-mode-wrapper: refusing unsandboxed session; bwrap is missing" >&2
    exit 1
fi

maybe_bind() { [ -e "$2" ] && MASK+=("$1" "$2" "$2"); }

MASK=(
    --die-with-parent
    --ro-bind /usr /usr
    --symlink usr/bin /bin --symlink usr/sbin /sbin
    --symlink usr/lib /lib --symlink usr/lib /lib64
    --ro-bind /etc /etc --ro-bind /opt /opt --ro-bind /sys /sys --ro-bind /var /var
    --proc /proc --dev-bind /dev /dev
    --bind /tmp /tmp --bind /run /run
    --bind "$GAMES_DIR" "$GAMES_DIR"
    --tmpfs "$HOME"
)
maybe_bind --bind "$HOME/.local/share/Steam"
maybe_bind --bind "$HOME/.steam"
maybe_bind --bind "$HOME/.cache"
maybe_bind --bind "$HOME/.config/discord"
maybe_bind --bind "$HOME/.config/discover_overlay"
maybe_bind --bind "$HOME/.pki"
maybe_bind --bind "$HOME/.local/share/godot"
maybe_bind --bind "$HOME/.local/share/vulkan"
maybe_bind --bind "$HOME/.config/unity3d"
maybe_bind --bind "$HOME/.config/godot"
maybe_bind --bind "$HOME/.config/Valve"

BINDS_FILE=/etc/game-mode/binds
if [ -r "$BINDS_FILE" ]; then
    while read -r mode path; do
        case "$path" in /*) ;; *) path="$HOME/$path" ;; esac
        case "$mode" in
            ''|\#*) ;;
            rw) maybe_bind --bind "$path" ;;
            ro) maybe_bind --ro-bind "$path" ;;
            *) echo "game-mode-wrapper: ignoring bad line in $BINDS_FILE: $mode $path" >&2 ;;
        esac
    done < "$BINDS_FILE"
fi
MASK+=(--remount-ro "$HOME")

echo "game-mode-wrapper: launching session inside bwrap home mask" >&2
exec bwrap "${MASK[@]}" -- /usr/bin/game-mode-wrapper --inner
