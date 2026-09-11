#!/bin/sh
# Sync the Obsidian plugin sources into the standalone release repo and cut a
# release there when the version has never been released.
#
# Direction of truth:
#   dreamor/MemVault :obsidian-plugin/   — source of truth for CODE
#   dreamor/memvault-obsidian            — repo of record for releases
#     (community directory; tag push -> build + provenance + release)
#
# What gets synced (owned by this repo's copy):
#   src/ (exact mirror, deletions propagate), styles.css, manifest.json,
#   tsconfig.json, vitest.config.ts
#
# What stays plugin-repo-owned (never touched):
#   package.json, package-lock.json, README.md, LICENSE, eslint.config.mjs,
#   .gitignore, .github/ — and versions.json, except the single release entry
#   this script appends when publishing a new version.
#
# Release rule: a release happens iff manifest.json's version is NOT present in
# the target's versions.json. Guard: refuses to run when the target has released
# a version NEWER than manifest.json (edits made directly there would otherwise
# be silently overwritten — port them into obsidian-plugin/ first).
#
# Usage: scripts/sync-obsidian-plugin.sh <memvault-obsidian-checkout> [--dry-run]
# The checkout must be push-authenticated: in CI the workflow clones it with a
# PAT (Actions' default GITHUB_TOKEN pushes do NOT trigger the target's
# on:push/tag workflows), locally your own credentials apply.
#
# Exit codes: 0 = in sync or synced (nothing pushed on dry-run).
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
SRC="$ROOT/obsidian-plugin"
TARGET_DIR=${1:?usage: sync-obsidian-plugin.sh <memvault-obsidian-checkout> [--dry-run]}
DRY_RUN=0
[ "${2:-}" != "--dry-run" ] || DRY_RUN=1

# Files owned by the local copy; src/ is mirrored (rsync --delete), the rest copied.
MIRROR_DIR=src
SYNCED_FILES="styles.css manifest.json tsconfig.json vitest.config.ts"

for f in $MIRROR_DIR $SYNCED_FILES; do
    [ -e "$SRC/$f" ] || { echo "missing $SRC/$f" >&2; exit 1; }
done
[ -d "$TARGET_DIR/.git" ] || { echo "$TARGET_DIR is not a git checkout" >&2; exit 1; }
[ -f "$TARGET_DIR/manifest.json" ] || { echo "$TARGET_DIR has no manifest.json — is it the memvault-obsidian repo?" >&2; exit 1; }

json_field() { # json_field <file> <key>  (node -e puts args at argv[1..])
    node -e 'const j=JSON.parse(require("fs").readFileSync(process.argv[1],"utf8"));process.stdout.write(String(j[process.argv[2]]))' -- "$1" "$2"
}

if [ "$DRY_RUN" -eq 1 ]; then
    # Exercise the real sync against a throwaway clone so the report is honest.
    CLEANUP_TARGET=$(mktemp -d)
    git clone --quiet "$TARGET_DIR" "$CLEANUP_TARGET"
    TARGET_DIR="$CLEANUP_TARGET"
    trap 'rm -rf "$CLEANUP_TARGET"' EXIT
fi
TGT=$TARGET_DIR

SOURCE_SHA=${SOURCE_SHA:-$(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)}

# --- copy files -------------------------------------------------------------
rsync -a --delete --exclude .DS_Store "$SRC/$MIRROR_DIR/" "$TGT/$MIRROR_DIR/"
for f in $SYNCED_FILES; do
    cp "$SRC/$f" "$TGT/$f"
done

# --- decide changed / release ------------------------------------------------
changed=$(cd "$TGT" && git status --porcelain -- $MIRROR_DIR $SYNCED_FILES)
ver=$(json_field "$TGT/manifest.json" version)

released=0
if grep -q "\"$ver\"[[:space:]]*:" "$TGT/versions.json"; then
    released=1
fi

latest=$(node -e 'const j=JSON.parse(require("fs").readFileSync(process.argv[1],"utf8"));process.stdout.write(Object.keys(j).join("\n"))' -- "$TGT/versions.json" | sort -t. -k1,1n -k2,2n -k3,3n | tail -1)

if [ "$released" -eq 0 ] && [ -n "$latest" ]; then
    newer=$(printf '%s\n%s\n' "$latest" "$ver" | sort -t. -k1,1n -k2,2n -k3,3n | tail -1)
    if [ "$newer" = "$latest" ] && [ "$latest" != "$ver" ]; then
        echo "ERROR: memvault-obsidian has released v$latest but manifest.json says $ver." >&2
        echo "The release repo is ahead — port its changes into obsidian-plugin/ first." >&2
        exit 1
    fi
fi

if [ -z "$changed" ] && [ "$released" -eq 1 ]; then
    echo "ok - obsidian-plugin in sync with memvault-obsidian (v$ver released)"
    exit 0
fi

# --- append the release entry ------------------------------------------------
if [ "$released" -eq 0 ]; then
    node -e '
const fs = require("fs");
const [path, version, minApp] = process.argv.slice(1);
const j = JSON.parse(fs.readFileSync(path, "utf8"));
j[version] = minApp;
fs.writeFileSync(path, JSON.stringify(j, null, 2) + "\n");' -- "$TGT/versions.json" "$ver" "$(json_field "$TGT/manifest.json" minAppVersion)"
fi

# --- commit & push -------------------------------------------------------------
if [ -z "$(git -C "$TGT" config user.email 2>/dev/null || true)" ]; then
    git -C "$TGT" config user.name "memvault-sync-bot"
    git -C "$TGT" config user.email "actions@users.noreply.github.com"
fi
git -C "$TGT" add $MIRROR_DIR $SYNCED_FILES
[ "$released" -eq 1 ] || git -C "$TGT" add versions.json

if [ "$released" -eq 0 ]; then
    message="feat: release v$ver

Synced from dreamor/MemVault@${SOURCE_SHA}"
    tag="v$ver"
else
    message="chore: sync plugin sources from dreamor/MemVault@${SOURCE_SHA}"
    tag=""
fi
git -C "$TGT" commit --quiet -m "$message"

if [ "$DRY_RUN" -eq 1 ]; then
    echo "dry-run — would commit to memvault-obsidian master:"
    git -C "$TGT" show --stat --oneline HEAD | sed 's/^/  /'
    if [ -n "$tag" ]; then
        echo "  and push tag $tag (triggers the release workflow there)"
    else
        echo "  no release: v$ver is already in versions.json"
    fi
    exit 0
fi

git -C "$TGT" push origin HEAD
echo "pushed sync commit to memvault-obsidian master"
if [ -n "$tag" ]; then
    git -C "$TGT" tag "$tag"
    git -C "$TGT" push origin "$tag"
    echo "pushed $tag — release workflow will build and publish it"
fi
