# Idle suspend in game mode — state & readiness

## How it works now

`-steamos3` makes Steam run console-style idle power management (dim after
15 min, **system suspend after 1 h** — `IdleSuspendACSeconds` etc. in Steam's
`config.vdf` System block). On 2026-06-12 this wedged a session black for ~10h:
`suspend.target` was **masked** (temporary measure: this box currently provides
internet to another server, and hosts NFS/libvirt/the access-gate verifier), so
logind refused Steam's suspend call and Steam parked mid-suspend — black Big
Picture, input pump dead, unrecoverable in place.

Defense layers (installed by `install.sh`, invoked by the session wrapper):

1. **`game-mode-steam-config apply`** (pre-Steam-launch, every session entry):
   - suspend **available** → `IdleSuspendACSeconds=3600` (normal console sleep)
   - suspend **masked or sleep-block-inhibited** → `IdleSuspendACSeconds=0`
     (Steam never attempts → no wedge)
   - dim stays at 900s either way; also appends
     `--disable-gpu-process-crash-limit` to `steamwebhelper_sniper_wrap.sh`
     (CEF context-loss respawns with GPU instead of black).
   - **No manual step when the mask comes off** — the next game-mode entry
     detects suspend is available and restores normal idle suspend.
2. **`game-mode-watchdog`** (in-session):
   - CEF crash burst (≥3 in 45s) → restart `steamwebhelper` + bounce controller
     Bluetooth + `steam://forceinputappid/769` (the input re-sync ladder,
     empirically required) — games keep running.
   - Suspend-wedge signature with no real suspend completing → clean session
     end to the greeter (in-place recovery proven impossible).
   - Re-asserts the webhelper flag every 120s (Steam updates revert it).
   - Log: `/tmp/game-mode-watchdog.log`.

## When the internet-sharing measure ends (the real-suspend checklist)

1. Unmask sleep: `sudo systemctl unmask suspend.target sleep.target hibernate.target`
2. Wake sources: enable USB wakeup for the Bluetooth adapter so the controller
   can wake the box (`echo enabled | sudo tee /sys/bus/usb/devices/<bt-dev>/power/wakeup`,
   find it via `grep . /sys/bus/usb/devices/*/product`); verify
   `cat /proc/acpi/wakeup` lists the relevant hubs as enabled.
3. Enter game mode → `grep IdleSuspendACSeconds ~/.local/share/Steam/config/config.vdf`
   should now read `3600` (config tool detected availability).
4. Validation pass (do this at the TV):
   - idle BP past 1h (or temporarily set the key lower) → box should *actually*
     suspend;
   - wake via controller (or power button if BT wake fails — then revisit
     step 2);
   - confirm gamescope + Steam + a running game survive resume, and the
     watchdog logged "real suspend+resume detected — no action".
5. Optional hygiene: while sharing measures are needed in the future, prefer a
   named inhibitor over masking, e.g. a unit running
   `systemd-inhibit --what=sleep --who=net-share --why="tethering serverX" sleep infinity`
   — self-documenting, and `game-mode-steam-config` detects it the same way.

## Caveats

- Suspending the box takes down NFS exports, libvirt guests, tailscale/SSH and
  the access-gate verifier while asleep — by design once the box stops being
  an always-on host. The verifier only matters at game-mode *entry*, which
  implies the box is awake anyway.
- A *deliberate* future inhibitor (e.g. long downloads) is handled: the config
  tool sees the block and zeroes Steam's suspend for that session; the watchdog
  covers the case where an inhibitor appears mid-session.
