#!/bin/sh
# SessionStart hook: recall remembered context for the session that is about
# to start. Claude Code parses our stdout as the hook envelope, so stdout
# carries exactly one JSON object (or plain text on the REST fallback) and
# nothing else. Every failure path exits 0 — a memory outage must never cost
# the user their session — but it does print one stderr warning, so an outage
# is visible without MEMVAULT_HOOK_DEBUG=1.
#
# Environment:
#   MEMVAULT_AGENT_ID   agent identity for scoped injection (default: host-aware
#                       — `codex` under Codex, else `claude-code`)
#   MEMVAULT_BIN        explicit path to the MemVault CLI (optional)
#   MEMVAULT_HTTP_URL   REST backend, default http://127.0.0.1:3777 (optional)
#   MEMVAULT_HOOK_DEBUG write diagnostics to ${TMPDIR:-/tmp}/memvault-hook.log

. "$(dirname "$0")/lib.sh"

payload=$(mktemp "${TMPDIR:-/tmp}/memvault-hook-XXXXXX") 2>/dev/null || exit 0
errlog=$(mktemp "${TMPDIR:-/tmp}/memvault-hook-err-XXXXXX") 2>/dev/null || exit 0
trap 'rm -f "$payload" "$errlog"' EXIT
# The host hands over the SessionStart payload on stdin; forward it verbatim —
# the CLI parses it (cwd → project scope, source), not this script.
cat >"$payload" 2>/dev/null || exit 0

agent_id=$(mv_agent_id)

bin=$(mv_find_cli 2>/dev/null) && [ -n "$bin" ] || bin=""
if [ -n "$bin" ]; then
    # hook-json: stdout = one envelope with additionalContext. The CLI's own
    # stderr is captured so a non-zero exit can surface its last line (still
    # one stderr line for us) instead of vanishing into MEMVAULT_HOOK_DEBUG.
    # `rc=$?` must be read in the else branch: after `fi` the if-compound's
    # own status is 0 whenever the branch simply did not match, which reported
    # a bogus "rc=0" for a CLI that exited non-zero.
    if "$bin" session-start \
        --agent-id "$agent_id" \
        --format hook-json \
        --hook-input "$payload" 2>"$errlog"; then
        exit 0
    else
        rc=$?
        detail=$(mv_last_line "$errlog")
        if [ -n "$detail" ]; then
            mv_warn "session-start failed (rc=$rc, bin=$bin): $detail"
        else
            mv_warn "session-start failed (rc=$rc, bin=$bin); no memories injected (set MEMVAULT_HOOK_DEBUG=1 for details)"
        fi
        mv_debug "cli session-start failed (rc=$rc), trying REST fallback"
    fi
fi

if mv_rest_available; then
    # Plain-mode session output: Claude Code adds SessionStart stdout to the
    # context, so the bare formatted text injected by ?output=plain works even
    # without the CLI. No context_hint here — sh must not hand-craft JSON.
    curl -fsS --max-time 5 \
        -H 'content-type: application/json' \
        -d "{\"agent_id\":\"$agent_id\"}" \
        "$(mv_rest_base)/api/session?output=plain" 2>/dev/null ||
        mv_warn "REST fallback failed ($(mv_rest_base)); no memories injected"
    exit 0
fi

mv_warn "no MemVault CLI or HTTP backend found; memories not injected (install the CLI, or set MEMVAULT_BIN / MEMVAULT_HTTP_URL)"
exit 0
