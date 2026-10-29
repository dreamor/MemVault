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
#
# Codex aborts Git marketplace clones after 30 seconds, and a full checkout of
# this repository can exceed that limit. Git registrations therefore use a
# sparse checkout containing only the plugin bundle and marketplace manifest.
set -eu

MARKET_NAME="memvault"
MARKET_SOURCE="${1:-dreamor/memvault}"
PLUGIN_ID="memvault@memvault"

add_marketplace() {
    if [ -d "$MARKET_SOURCE" ]; then
        codex plugin marketplace add "$MARKET_SOURCE"
    else
        codex plugin marketplace add "$MARKET_SOURCE" \
            --sparse plugins/memvault \
            --sparse .agents/plugins
    fi
}

# Refresh the marketplace snapshot (Git sources pull; local sources re-scan).
if codex plugin marketplace list | grep -q "^${MARKET_NAME} "; then
    if ! upgrade_output=$(codex plugin marketplace upgrade "$MARKET_NAME" 2>&1); then
        case "$upgrade_output" in
            *"timed out"*|*"early EOF"*)
                # Older registrations may lack sparse_paths, causing the
                # upgrade to fall back to a full clone. Recreate only those
                # timeout/truncation failures with the sparse layout.
                printf '%s\n' "$upgrade_output" >&2
                printf 'Recreating marketplace %s with sparse checkout paths.\n' "$MARKET_NAME" >&2
                codex plugin marketplace remove "$MARKET_NAME" >/dev/null 2>&1 || true
                add_marketplace
                ;;
            *)
                printf '%s\n' "$upgrade_output" >&2
                exit 1
                ;;
        esac
    fi
else
    add_marketplace
fi

# Reinstall the plugin so the cache picks up the new bundle.
codex plugin remove "$PLUGIN_ID" >/dev/null 2>&1 || true
codex plugin add "$PLUGIN_ID"

printf '\nRefreshed %s from %s\n' "$PLUGIN_ID" "$MARKET_SOURCE"
printf 'If the bundle hook scripts changed, re-approve them once in /hooks\n'
printf '(trust is pinned to the script hashes).\n'
