#!/usr/bin/env bash
set -euo pipefail

REPO_DIR="$HOME/slug-mcp"
BIN_PATH="/usr/local/bin/slug-mcp"
BUILD_DIR="target/release-optimized"

cd "$REPO_DIR"

echo "Pulling latest changes..."
git pull origin main

echo "Building public-only binary (release-optimized + native CPU)..."
RUSTFLAGS="-C target-cpu=native" cargo build \
    --profile release-optimized \
    --no-default-features

# strip is part of the profile already; this is a defensive no-op
strip "${BUILD_DIR}/slug-mcp" || true

echo "Stopping slug-mcp..."
sudo systemctl stop slug-mcp

echo "Swapping binary (atomic mv via /tmp)..."
sudo install -m 755 "${BUILD_DIR}/slug-mcp" "${BIN_PATH}.new"
sudo mv "${BIN_PATH}.new" "$BIN_PATH"

echo "Starting slug-mcp..."
sudo systemctl start slug-mcp

echo "Reloading nginx (in case unit/cert changed)..."
sudo systemctl reload nginx || true

sleep 2

echo "Verifying..."
sudo systemctl is-active slug-mcp
sudo systemctl is-active nginx

echo "Deploy complete."
