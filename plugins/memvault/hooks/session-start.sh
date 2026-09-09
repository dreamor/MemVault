#!/bin/sh
# SessionStart hook: recall remembered context for the session that is about
# to start. Claude Code parses our stdout as the hook envelope, so stdout
# carries exactly one JSON object (or plain text on the REST fallback) and
# nothing else. Every failure path exits 0 — a memory outage must never cost
# the user their session.
#
# Environment:
#   MEMVAULT_AGENT_ID   agent identity for scoped injection (default claude-code)
#   MEMVAULT_BIN        explicit path to the MemVault CLI (optional)
#   MEMVAULT_HTTP_URL   REST backend, default http://127.0.0.1:3777 (optional)
#   MEMVAULT_HOOK_DEBUG write diagnostics to ${TMPDIR:-/tmp}/memvault-hook.log

. "$(dirname "$0")/lib.sh"

payload=$(mktemp "${TMPDIR:-/tmp}/memvault-hook-XXXXXX") 2>/dev/null || exit 0
trap 'rm -f "$payload"' EXIT
# The host hands over the SessionStart payload on stdin; forward it verbatim —
# the CLI parses it (cwd → project scope, source), not this script.
cat >"$payload" 2>/dev/null || exit 0

bin=$(mv_find_cli 2>/dev/null) && [ -n "$bin" ] || bin=""
if [ -n "$bin" ]; then
    # hook-json: stdout = one envelope with additionalContext; stderr is the
    # CLI's own diagnostics (debug-gated skip reasons).
    "$bin" session-start \
        --agent-id "${MEMVAULT_AGENT_ID:-claude-code}" \
        --format hook-json \
        --hook-input "$payload" 2>/dev/null && exit 0
    mv_debug "cli session-start failed (rc=$?), trying REST fallback"
fi

if mv_rest_available; then
    # Plain-mode session output: Claude Code adds SessionStart stdout to the
    # context, so the bare formatted text injected by ?output=plain works even
    # without the CLI. No context_hint here — sh must not hand-craft JSON.
    curl -fsS --max-time 5 \
        -H 'content-type: application/json' \
        -d "{\"agent_id\":\"${MEMVAULT_AGENT_ID:-claude-code}\"}" \
        "$(mv_rest_base)/api/session?output=plain" 2>/dev/null || mv_debug "rest fallback failed"
    exit 0
fi

mv_debug "no memvault CLI and no HTTP backend; skipping injection"
exit 0
