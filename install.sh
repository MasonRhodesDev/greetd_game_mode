#!/bin/bash
set -e
cd "$(dirname "$(realpath "$0")")"

if [ "$(id -u)" -eq 0 ]; then
    echo "Run as a regular user (the script escalates with sudo where needed);" >&2
    echo "cargo and yay refuse to run as root." >&2
    exit 1
fi

# ---------------------------------------------------------------------------
# Preflight: everything the finished setup needs. Compositor flavour, AUR
# helper and multilib Steam are setup choices, so missing pieces are reported
# with install hints rather than auto-installed.
# ---------------------------------------------------------------------------
echo "Checking prerequisites..."
MISSING=()
need() { command -v "$1" >/dev/null 2>&1 || MISSING+=("$1 — $2"); }
need greetd         "pacman -S greetd (or greetd-git)"
need regreet        "pacman -S greetd-regreet (or greetd-regreet-git)"
need hyprland       "hyprland >= 0.55 (pacman -S hyprland, or hyprland-git)"
need start-hyprland "ships with hyprland >= 0.55; greeter sessions launch via this watchdog"
need gamescope      "pacman -S gamescope"
need steam          "pacman -S steam (requires the [multilib] repo)"
need bwrap          "pacman -S bubblewrap (home mask for the game session)"
need swaybg         "pacman -S swaybg (greeter background)"
need qrencode       "pacman -S qrencode (one-time phone setup QRs)"
need python3        "pacman -S python"
need cargo          "pacman -S rust (or rustup)"
need curl           "pacman -S curl"
need tailscale      "pacman -S tailscale (HTTPS origin for the WebAuthn verifier)"
getent passwd greeter >/dev/null || MISSING+=("greeter user — created by the greetd package")
if [ ${#MISSING[@]} -gt 0 ]; then
    echo "Missing prerequisites:" >&2
    printf '  - %s\n' "${MISSING[@]}" >&2
    exit 1
fi

# The verifier's RP ID / origin come from the tailnet FQDN, so tailscale must
# be up and logged in before installing.
if ! tailscale status --json >/dev/null 2>&1; then
    echo "tailscale is not running or not logged in." >&2
    echo "  sudo systemctl enable --now tailscaled && sudo tailscale up" >&2
    echo "then re-run this installer." >&2
    exit 1
fi

# Optional cosmetics / extras
if ! pacman -Qi papirus-icon-theme &>/dev/null; then
    sudo pacman -S --noconfirm papirus-icon-theme
fi
if command -v yay >/dev/null 2>&1; then
    # greeter GTK theme + Discord voice overlay (the wrapper only launches the
    # overlay if present)
    pacman -Qi canta-gtk-theme &>/dev/null || yay -S --noconfirm canta-gtk-theme
    pacman -Qi discover-overlay &>/dev/null || yay -S --noconfirm discover-overlay
else
    echo "WARN: yay not found — skipping optional AUR packages (canta-gtk-theme, discover-overlay)"
fi

echo "Cleaning up old build..."
cargo clean

echo "Building game_mode..."
cargo build --release

# Source the constants from the Rust binary
echo "Loading configuration constants..."
eval "$(target/release/generate_constants)"

echo "Installing game_mode..."

# Set up games user and group
echo "Setting up games user and group..."
if ! getent group "$GAMES_GROUP" >/dev/null; then
    sudo groupadd "$GAMES_GROUP"
fi

if ! getent passwd "$GAMES_USER" >/dev/null; then
    sudo useradd -m -g "$GAMES_GROUP" -s /bin/bash "$GAMES_USER"
fi

# Set up games directory
echo "Setting up games directory..."
sudo mkdir -p "$GAMES_DIR"
sudo chgrp -R "$GAMES_GROUP" "$GAMES_DIR"
sudo chmod -R g+rwxs "$GAMES_DIR"

# Set group ownership and permissions for /etc/greetd
sudo mkdir -p "$GREETD_DIR/logs"
sudo mkdir -p /usr/local/bin
sudo chgrp -R "$GREETER_USER" "$GREETD_DIR"
sudo chmod g+rwxs "$GREETD_DIR"

# Create sudoers file for greetd service restart
echo "Configuring sudoers..."
SUDOERS_CONTENT="$GREETER_USER ALL=(ALL) NOPASSWD: /usr/bin/systemctl restart greetd.service, /usr/bin/fgconsole, /usr/bin/rm -f /run/greetd.run"
echo "$SUDOERS_CONTENT" | sudo tee /etc/sudoers.d/greeter-greetd > /dev/null
sudo chmod 440 /etc/sudoers.d/greeter-greetd
sudo visudo -c -f /etc/sudoers.d/greeter-greetd

# Copy all greetd files
sudo cp -r greetd/* "$GREETD_DIR"

# Ensure the game-mode session wrapper is executable
sudo chmod +x "$GREETD_DIR/scripts/game-mode-wrapper.sh"

# The greeter compositor config must parse on the installed Hyprland version
# (removed/renamed options otherwise surface as an error banner at the greeter).
echo "Verifying greeter Hyprland config..."
if ! hyprland --verify-config --config "$GREETD_DIR/hypr.conf" 2>&1 | grep -qx 'config ok'; then
    hyprland --verify-config --config "$GREETD_DIR/hypr.conf" 2>&1 | grep -i 'config error' >&2 || true
    echo "ERROR: $GREETD_DIR/hypr.conf failed Hyprland config verification" >&2
    exit 1
fi

# Steam gamepadui "Switch to Desktop" hook: Steam execs steamos-session-select
# from PATH; install our shim (ends the game session -> regreet greeter).
sudo install -m755 greetd/scripts/steamos-session-select /usr/local/bin/steamos-session-select

# Replace TTY placeholder in config files
echo "Configuring TTY settings..."
sudo sed -i "s/{{vt}}/$VT_NUMBER/g" "$GREETD_DIR/config_default.toml"
sudo sed -i "s/{{vt}}/$VT_NUMBER/g" "$GREETD_DIR/game_mode_login.toml"

# Set up config files
echo "Setting up configuration files..."
if [ -f "$GREETD_DIR/config.toml" ]; then
    sudo rm "$GREETD_DIR/config.toml"
fi
sudo -u "$GREETER_USER" ln -sf "$GREETD_DIR/config_default.toml" "$GREETD_DIR/config.toml"

# stop game-mode service if it is running
sudo systemctl stop game-mode.service || true

# Install binary
echo "Installing binary..."
sudo cp target/release/game_mode /usr/local/bin/game-mode
sudo chmod +x /usr/local/bin/game-mode

# Install greetd service
echo "Installing greetd service..."
sudo cp greetd/game-mode.service /etc/systemd/system/game-mode.service
sudo chmod 644 /etc/systemd/system/game-mode.service

# Reload systemd and enable service
echo "Enabling and starting service..."
sudo systemctl daemon-reload

echo "Enabling game-mode service..."
sudo systemctl enable game-mode.service

echo "Restarting greetd service..."
sudo systemctl restart greetd.service

# ---------------------------------------------------------------------------
# Passkey approval for game-mode entry (WebAuthn verifier + Web Push).
# The game-mode daemon gates switch_to_game_mode() on a phone passkey approval.
# ---------------------------------------------------------------------------
echo "Setting up game-mode passkey approval..."
for p in qrencode; do command -v "$p" >/dev/null || sudo pacman -S --noconfirm "$p"; done

# system user + data dir for the verifier (holds the enrolled passkey)
getent passwd access-gate >/dev/null || \
    sudo useradd --system --no-create-home --shell /usr/sbin/nologin access-gate
sudo install -d -o access-gate -g access-gate -m700 /var/lib/access-gate

# deploy verifier to /opt (system service must not depend on a user home)
sudo install -d -m755 /opt/game-mode/approval
sudo cp approval/app.py approval/requirements.txt /opt/game-mode/approval/
[ -d /opt/game-mode/approval/venv ] || sudo python3 -m venv /opt/game-mode/approval/venv
sudo /opt/game-mode/approval/venv/bin/pip -q install -r /opt/game-mode/approval/requirements.txt

# config (RP ID from tailscale) + helper + unit
FQDN=$(tailscale status --json | python3 -c 'import sys,json;print(json.load(sys.stdin)["Self"]["DNSName"].rstrip("."))')
if [ ! -f /etc/game-mode/approval.env ]; then
    sudo install -d -m755 /etc/game-mode
    printf 'AG_RP_ID=%s\nAG_ORIGIN=https://%s\nAG_DATA_DIR=/var/lib/access-gate\nAG_VERIFIER_CTRL=http://127.0.0.1:8731\nAG_APPROVE_BASE=https://%s/approve\nAG_TIMEOUT=90\nAG_VAPID_SUB=%s\n' \
        "$FQDN" "$FQDN" "$FQDN" "${AG_VAPID_SUB:-access-gate@$FQDN}" | sudo tee /etc/game-mode/approval.env >/dev/null
    sudo chmod 644 /etc/game-mode/approval.env
fi
sudo install -m755 bin/game-mode-approve /usr/local/bin/game-mode-approve
sudo install -m644 systemd/access-gate-verifier.service /etc/systemd/system/access-gate-verifier.service
sudo systemctl daemon-reload
sudo systemctl enable access-gate-verifier.service
sudo systemctl restart access-gate-verifier.service
# wait for the verifier to come up before querying enrollment state
for _ in $(seq 1 15); do
    curl -s --max-time 2 127.0.0.1:8730/ >/dev/null && break
    sleep 1
done

# HTTPS for the verifier on the tailnet (WebAuthn needs a real TLS origin)
sudo tailscale serve --bg --https=443 http://127.0.0.1:8730 || \
    echo "  WARN: run 'sudo tailscale serve --bg --https=443 http://127.0.0.1:8730' manually"

# one-time phone setup via QR: passkey enrollment, then Web Push subscription
# (both pages are flag/state-gated; skipped when already done)
if ! curl -s 127.0.0.1:8730/ | grep -q '"enrolled": *true'; then
    sudo -u access-gate touch /var/lib/access-gate/enroll-open
    echo; echo "== Scan with phone CAMERA to enroll the game-mode passkey =="
    qrencode -t ANSIUTF8 "https://$FQDN/enroll"
    echo "Waiting for passkey enrollment..."
    for _ in $(seq 1 60); do
        curl -s 127.0.0.1:8730/ | grep -q '"enrolled": *true' && { echo "Enrolled."; break; }
        sleep 3
    done
fi
if ! curl -s 127.0.0.1:8730/ | grep -q '"push_subscribed": *true'; then
    echo; echo "== Scan with phone CAMERA to enable approval notifications =="
    qrencode -t ANSIUTF8 "https://$FQDN/setup"
    echo "Waiting for push subscription..."
    for _ in $(seq 1 60); do
        curl -s 127.0.0.1:8730/ | grep -q '"push_subscribed": *true' && { echo "Subscribed."; break; }
        sleep 3
    done
fi

# Enable Decky Loader so its plugins (Discord status, etc.) inject into Big
# Picture. Decky itself must already be installed under the game user's
# ~/homebrew (https://decky.xyz); this only enables its service if present.
if [ -f /etc/systemd/system/plugin_loader.service ]; then
    echo "Enabling Decky Loader (plugin_loader.service)..."
    sudo systemctl enable plugin_loader.service
fi

echo "Installation complete!"