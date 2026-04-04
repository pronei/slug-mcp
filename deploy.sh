#!/usr/bin/env bash
set -euo pipefail

REPO_DIR="$HOME/slug-mcp"
BIN_PATH="/usr/local/bin/slug-mcp"

cd "$REPO_DIR"

echo "Pulling latest changes..."
git pull origin main

echo "Building public-only release binary..."
cargo build --release --no-default-features
strip target/release/slug-mcp

echo "Stopping services..."
sudo systemctl stop slug-mcp

echo "Swapping binary..."
sudo cp target/release/slug-mcp "$BIN_PATH"
sudo chmod +x "$BIN_PATH"

echo "Starting services..."
sudo systemctl start slug-mcp
sudo systemctl restart ngrok

sleep 2

echo "Verifying..."
sudo systemctl is-active slug-mcp
sudo systemctl is-active ngrok

echo "Deploy complete."
