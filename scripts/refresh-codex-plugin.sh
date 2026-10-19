#!/bin/sh
# Refresh the Codex-side MemVault plugin after the bundle changes in this repo.
#
# Codex snapshots a marketplace and caches the installed plugin under
# ~/.codex/plugins/cache/<marketplace>/<plugin>/<version>/, so repo changes are
# not live until the snapshot is refreshed and the plugin reinstalled. This
# script does both (idempotent), then reminds you to re-approve hooks if their
# scripts changed. Usage:
#
#   scripts/refresh-codex-plugin.sh                 # source: dreamor/memvault
#   scripts/refresh-codex-plugin.sh /path/to/repo   # use a local directory
set -eu

MARKET_SOURCE="${1:-dreamor/memvault}"
PLUGIN_ID="memvault@memvault"

# Refresh the marketplace snapshot (Git sources pull; local sources re-scan).
if codex plugin marketplace list | grep -q '^memvault '; then
    codex plugin marketplace upgrade memvault >/dev/null 2>&1 || true
else
    codex plugin marketplace add "$MARKET_SOURCE"
fi

# Reinstall the plugin so the cache picks up the new bundle.
codex plugin remove "$PLUGIN_ID" >/dev/null 2>&1 || true
codex plugin add "$PLUGIN_ID"

printf '\nRefreshed %s from %s\n' "$PLUGIN_ID" "$MARKET_SOURCE"
printf 'If the bundle hook scripts changed, re-approve them once in /hooks\n'
printf '(trust is pinned to the script hashes).\n'
