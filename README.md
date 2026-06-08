# Game Mode Service

A system service that provides a game-focused login experience by integrating with greetd display manager. This service allows users to switch between desktop and game mode login screens using a gamepad.

## Overview

The Game Mode Service is designed to enhance the gaming experience on Linux systems by:

1. Providing a dedicated game mode login screen
2. Allowing seamless switching between desktop and game mode using a gamepad
3. Integrating with greetd display manager for a smooth transition

## Features

- Automatic detection of connected gamepads
- Gamepad-based mode switching (using the Mode/Guide button)
- Seamless integration with greetd display manager
- Debug logging support
- Automatic fallback to desktop mode on service startup

## Requirements

- Arch Linux system with greetd display manager
- Gamepad with a Mode button

Unlisted Dependencies:
```
sudo pacman -S greetd-git greetd-regreet-git
yay -S steam gamescope-session-steam-git hyprland-meta-git
yay -S discover-overlay   # Discord voice overlay (optional)
```

These can all be swapped out. This is just specific to my setup, so I didn't add any config options to help.

## Discord Integration

Game mode runs Steam Big Picture (`steam -gamepadui`) inside a standalone
gamescope compositor, so the desktop Discord app and overlay aren't present by
default. Two complementary pieces cover it:

### Decky Loader (status / presence panels)

[Decky Loader](https://decky.xyz) patches the Big Picture UI and exposes plugins
(e.g. Discord status) in the Quick Access menu. It runs as the `plugin_loader`
system service and is independent of how the session is launched. Install Decky
once under the game user's `~/homebrew`; `install.sh` then enables the service.
Enable manually with:
```
sudo systemctl enable --now plugin_loader.service
```

### discover-overlay (in-game voice overlay)

`scripts/game-mode-wrapper.sh` launches `discover-overlay` before exec'ing Steam.
gamescope hands children an **Xwayland (X11)** display, not Wayland, so the
overlay runs with `GDK_BACKEND=x11` (and `WAYLAND_DISPLAY` unset) per
`discover-overlay --help`. Notes:

- Run `discover-overlay --configure` **once in desktop mode** (with Discord
  running) to authorize the connection and set overlay options.
- The overlay reads voice state from a running Discord client's IPC socket
  (`$XDG_RUNTIME_DIR/discord-ipc-0`). Add Discord as a non-Steam shortcut and
  launch it from Big Picture when you want voice display — the overlay
  reconnects automatically. Text chat / calls are best done from a phone;
  controller text entry in Big Picture is painful.

## Installation

Run the installer
```bash
sudo ./install.sh
```

## Configuration

The service uses several configuration files located in `/etc/greetd/`:

- `config_default.toml` - Default desktop mode configuration - This can be overwritten with your current `/etc/greetd/config.toml`
- `game_mode_login.toml` - Game mode configuration, launches steam big picture with autologin
- `src/config.rs` - The compiled config for both the installer and runtime. This is where you change options and defaults

## Usage

1. The service automatically starts with the system
2. By default, it starts in desktop mode config
3. To switch to game mode:
   - Press the Mode (Guide) button on your gamepad
   - Release to switch to game mode
4. The service will restart greetd and autologin as the `games` user with steam big picture mode

## Logging

Logs are written to `/etc/greetd/logs/game-mode.log`. Debug logging can be enabled by setting the `RUST_LOG` environment variable to `game_mode=debug`.

## Development

The project is written in Rust and uses the following key dependencies:
- `gilrs` for gamepad support
- `tracing` for logging
- `anyhow` for error handling
- `serde` for configuration management

## License

[Add your license information here]
