#!/bin/bash
set -e

# Check if packages are installed and install if needed
if ! pacman -Qi canta-gtk-theme &>/dev/null; then
    yay -S --noconfirm canta-gtk-theme
fi

if ! pacman -Qi papirus-icon-theme &>/dev/null; then
    sudo pacman -S --noconfirm papirus-icon-theme
fi

echo "Cleaning up old build..."
cargo clean

echo "Building game_mode..."
cargo build --release

# Source the constants from the Rust binary
echo "Loading configuration constants..."
eval "$(target/release/generate_constants)"

echo "Installing game_mode..."

# Set group ownership and permissions for /etc/greetd
sudo mkdir -p "$GREETD_DIR/logs"
sudo mkdir -p /usr/local/bin
sudo chgrp -R "$GREETER_USER" "$GREETD_DIR"
sudo chmod g+rwxs "$GREETD_DIR"

# Create sudoers file for greetd service restart
echo "Configuring sudoers..."
SUDOERS_CONTENT="$GREETER_USER ALL=(ALL) NOPASSWD: /usr/bin/systemctl restart greetd.service, /usr/bin/fgconsole"
echo "$SUDOERS_CONTENT" | sudo tee /etc/sudoers.d/greeter-greetd > /dev/null
sudo chmod 440 /etc/sudoers.d/greeter-greetd
sudo visudo -c -f /etc/sudoers.d/greeter-greetd

# Copy all greetd files
sudo cp -r greetd/* "$GREETD_DIR"

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

echo "Installation complete!" 