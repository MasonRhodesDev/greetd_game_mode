#!/bin/sh
# greetd session command: prefer a Hyprland-based greeter (mouse-follow,
# monitor rotation/scale hints from the last session's hyprstate profile),
# fall back to cage (stable, repo-packaged kiosk compositor) whenever
# Hyprland looks or proved broken — a hyprland-git upgrade must never be
# able to lock the seat.
#
# Fallback triggers:
#   - `Hyprland --verify-config` fails on the greeter config, or
#   - the previous $STREAK_MAX greeter attempts each died within
#     $FAST_DEATH_SECS (runtime crash the verifier can't see).
# The streak file lives in /tmp: it resets on reboot, and a successful
# greeter run (regreet up longer than FAST_DEATH_SECS) clears it.

GREETER_CFG="/etc/greetd/hypr.lua"
STREAK_FILE="/tmp/greeter-crash-streak"
FAST_DEATH_SECS=10
STREAK_MAX=2

fallback() {
    exec /usr/bin/cage -s -- /usr/bin/regreet
}

command -v Hyprland >/dev/null || fallback
[ -f "$GREETER_CFG" ] || fallback

STREAK=$(cat "$STREAK_FILE" 2>/dev/null || echo 0)
[ "$STREAK" -ge "$STREAK_MAX" ] 2>/dev/null && fallback

Hyprland --verify-config --config "$GREETER_CFG" >/dev/null 2>&1 || fallback

START=$(date +%s)
/usr/bin/start-hyprland -- --config "$GREETER_CFG"
RC=$?
ELAPSED=$(( $(date +%s) - START ))

if [ "$ELAPSED" -lt "$FAST_DEATH_SECS" ]; then
    echo $((STREAK + 1)) > "$STREAK_FILE"
else
    rm -f "$STREAK_FILE"
fi
exit $RC
