#!/usr/bin/env bash
set -euo pipefail

REPO_DIR="${REPO_DIR:-$HOME/slug-mcp}"
DEPLOY_BRANCH="${DEPLOY_BRANCH:-main}"
DEPLOY_REMOTE="${DEPLOY_REMOTE:-origin}"
BIN_PATH="${BIN_PATH:-/usr/local/bin/slug-mcp}"
BUILD_DIR="${BUILD_DIR:-target/release-optimized}"
HEALTH_HOST="${HEALTH_HOST:-2262-cse115b-02.be.ucsc.edu}"

cd "$REPO_DIR"

echo "Fetching ${DEPLOY_REMOTE}/${DEPLOY_BRANCH}..."
git fetch "$DEPLOY_REMOTE" "$DEPLOY_BRANCH"
git merge --ff-only FETCH_HEAD

echo "Building public-only binary (release-optimized + native CPU)..."
RUSTFLAGS="-C target-cpu=native" cargo build \
    --profile release-optimized \
    --no-default-features

if command -v strip >/dev/null 2>&1; then
    strip "${BUILD_DIR}/slug-mcp" || true
fi

echo "Installing binary..."
sudo install -m 755 "${BUILD_DIR}/slug-mcp" "${BIN_PATH}.new"
sudo mv "${BIN_PATH}.new" "$BIN_PATH"

echo "Restarting slug-mcp..."
sudo systemctl restart slug-mcp

echo "Testing and reloading nginx..."
sudo nginx -t
sudo systemctl reload-or-restart nginx

sleep 2

echo "Verifying..."
sudo systemctl is-active --quiet slug-mcp
sudo systemctl is-active --quiet nginx
curl -fsSk --max-time 10 \
    -H "Host: ${HEALTH_HOST}" \
    "https://127.0.0.1/healthz" >/dev/null

echo "Deploy complete."
