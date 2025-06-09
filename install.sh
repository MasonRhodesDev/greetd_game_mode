#!/bin/bash
set -e

echo "Building game_mode..."
cargo build --release

echo "Installing game_mode..."
sudo ./target/release/game_mode --install

echo "Restarting greetd service..."
sudo systemctl restart greetd

echo "Installation complete!" 