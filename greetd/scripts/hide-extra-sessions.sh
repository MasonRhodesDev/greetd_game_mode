#!/bin/sh
# Curate greeter session entries. Two jobs, re-applied by the pacman hook
# hide-extra-greeter-sessions.hook whenever hyprland-git / kodi upgrades
# touch them:
#
# 1. Hide every package-owned session entry we don't want in regreet's menu.
#    Since 0.55.4 hyprland-git ships its OWN hyprland-uwsm.desktop that
#    launches by desktop-entry ID ("uwsm start ... hyprland.desktop") — uwsm
#    refuses hidden entry IDs, so that combination broke login on
#    2026-07-07. Hide upstream's uwsm entry too and never depend on it.
#
# 2. Install OUR login entry (unowned by any package, immune to upgrades).
#    It launches uwsm in hardcode mode with a binary PATH — no desktop-entry
#    indirection, so hiding/renaming package entries can never break it.
#    start-hyprland is upstream's watchdog wrapper: crash loops land in
#    safe mode instead of a dead seat.

for f in /usr/share/wayland-sessions/hyprland.desktop \
         /usr/share/wayland-sessions/hyprland-uwsm.desktop \
         /usr/share/wayland-sessions/kodi-gbm.desktop \
         /usr/share/xsessions/kodi.desktop; do
    [ -f "$f" ] || continue
    grep -q '^Hidden=true' "$f" || sed -i '/^\[Desktop Entry\]/a Hidden=true' "$f"
done

cat > /usr/share/wayland-sessions/hyprland-mason.desktop << 'EOF'
[Desktop Entry]
Name=Hyprland (uwsm)
Comment=Hyprland via uwsm + start-hyprland watchdog (local entry, survives package upgrades)
Exec=/usr/local/bin/uwsm-start-hyprland
TryExec=uwsm
DesktopNames=Hyprland
Type=Application
EOF
