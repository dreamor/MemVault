#!/usr/bin/env bash
# Sync the built Web Dashboard (dashboard/dist) into the crate folder that
# rust-embed bakes into the memvault-mcp binary (crates/memvault-mcp/assets/web).
#
# Run this after changing dashboard/ sources and commit the result; CI's
# dashboard job fails when the checked-in assets drift from a fresh build.
set -euo pipefail
cd "$(dirname "$0")/.."

(cd dashboard && npm ci && npm run build)

rm -rf crates/memvault-mcp/assets/web
cp -R dashboard/dist crates/memvault-mcp/assets/web

echo "Synced $(find crates/memvault-mcp/assets/web -type f | wc -l | tr -d ' ') files into crates/memvault-mcp/assets/web"
