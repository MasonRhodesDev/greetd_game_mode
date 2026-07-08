-- Minimal Hyprland session for the regreet greeter.
-- Launched by /etc/greetd/scripts/greeter-launcher.sh (which falls back to
-- cage if this config or Hyprland itself is broken).

-- Monitor layout hints mirrored from the last session's active hyprstate
-- profile (rotation/scale/positions), synced by /usr/local/bin/greeter-hint-sync
-- on profile switches. Missing/broken hint file = auto-configure everything.
pcall(dofile, "/etc/greetd/monitors-hint.lua")

hl.config({
  -- No animations at the greeter: regreet's close animation on successful
  -- login shrank the window and revealed the (formerly duplicated)
  -- background behind it — login should cut straight to the session.
  animations = { enabled = false },
  misc = {
    disable_hyprland_logo = true,
    disable_splash_rendering = true,
    -- Suppress the startup nag about hyprland-guiutils (renamed from qtutils
    -- in 0.55). guiutils IS installed; the greeter just doesn't need its
    -- dialogs.
    disable_hyprland_guiutils_check = true,
    force_default_wallpaper = -1,
  },
  render = {
    -- Force SDR at the greeter: gamescope leaves the display in HDR mode when
    -- the game session ends, and auto-HDR would keep it lit.
    cm_auto_hdr = 0,
  },
  input = {
    kb_layout = "us",
    -- The regreet window opens on (and focus follows) the monitor the mouse
    -- is on — multi-monitor login lands where you're looking.
    follow_mouse = 1,
    sensitivity = 0,
    touchpad = { natural_scroll = false },
  },
  general = {
    gaps_in = 0,
    gaps_out = 0,
    border_size = 0,
    layout = "dwindle",
    allow_tearing = false,
  },
})

-- regreet draws its own background (regreet.toml [background]); fullscreen it
-- so that is the ONLY background — no swaybg layered behind (the source of
-- the doubled-background look), nothing to reveal on exit.
hl.window_rule({ match = { class = [[.*]] }, fullscreen = true })

-- Environment: avoid portal-related delays in the greeter session.
hl.env("GTK_USE_PORTAL", "0")
hl.env("GDK_DEBUG", "no-portals")
hl.env("XCURSOR_SIZE", "24")

hl.on("hyprland.start", function()
  -- When regreet exits (successful login), exit the greeter compositor so
  -- greetd can hand over the VT.
  hl.exec_cmd("sh -c 'regreet; hyprctl dispatch exit'")
end)
