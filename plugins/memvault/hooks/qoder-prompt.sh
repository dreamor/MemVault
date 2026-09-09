#!/bin/sh
# Qoder UserPromptSubmit hook: the first prompt of a session injects recalled
# memories as plain text; later prompts in the same session stay silent
# (dedup state keyed on the payload's session_id). REST-only, like the
# OpenCode adapter — no CLI parsing or envelope concerns.
#
# The output-shape contract of Qoder's UserPromptSubmit (whether bare stdout
# becomes context) is verify-on-install; see integrations/README.md. Every
# failure exits 0 quietly, so an unreachable backend never blocks a prompt.

. "$(dirname "$0")/lib.sh"

payload=$(mktemp "${TMPDIR:-/tmp}/memvault-hook-XXXXXX") 2>/dev/null || exit 0
trap 'rm -f "$payload"' EXIT
cat >"$payload" 2>/dev/null || exit 0

# Best-effort session key: single jq-free string extraction, empty on any
# mismatch. Without a key, every prompt would inject — so require it.
session=$(sed -n 's/.*"session_id"[[:space:]]*:[[:space:]]*"\([A-Za-z0-9._-]*\)".*/\1/p' "$payload" | head -n 1)
[ -n "$session" ] || exit 0

state="${TMPDIR:-/tmp}/memvault-qoder-$session"
[ -f "$state" ] && exit 0

command -v curl >/dev/null 2>&1 || exit 0
curl -fsS --max-time 5 \
    -H 'content-type: application/json' \
    -d "{\"agent_id\":\"${MEMVAULT_AGENT_ID:-qoder}\"}" \
    "$(mv_rest_base)/api/session?output=plain" 2>/dev/null || exit 0
: >"$state" 2>/dev/null || true
exit 0
