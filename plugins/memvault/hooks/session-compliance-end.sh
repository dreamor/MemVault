#!/bin/sh
# SessionEnd hook: enqueue the compliance rows opened by SessionStart.
#
# Codex hard-caps SessionEnd commands at three seconds. Doing LLM calls here
# (the old behavior) meant a configured provider could be interrupted before
# it wrote any verdict. The hook now only writes a durable job; the long-lived
# HTTP server worker reads the transcript and judges outside this timeout.
#
# The worker records an explicit Unknown verdict on missing transcripts, no
# LLM provider, judge errors, and deadlines instead of leaving rows pending or
# presenting a fabricated 100% compliance rate.
#
# Environment:
#   MEMVAULT_HOOK_COMPLIANCE  1 = enqueue at SessionEnd (default 1); 0 disables
#   MEMVAULT_AGENT_ID         agent identity override (default: host-aware)
#   MEMVAULT_BIN              explicit path to the MemVault CLI (optional)
#   MEMVAULT_HOOK_DEBUG       write diagnostics to ${TMPDIR:-/tmp}/memvault-hook.log
#
# Host output is ignored for SessionEnd. Diagnostics go to stderr and every
# path exits 0 so a memory outage never breaks the host session.

. "$(dirname "$0")/lib.sh"

[ "${MEMVAULT_HOOK_COMPLIANCE:-1}" = "1" ] || exit 0

payload=$(mktemp "${TMPDIR:-/tmp}/memvault-hook-XXXXXX") 2>/dev/null || exit 0
errlog=$(mktemp "${TMPDIR:-/tmp}/memvault-hook-err-XXXXXX") 2>/dev/null || exit 0
trap 'rm -f "$payload" "$errlog"' EXIT
cat >"$payload" 2>/dev/null || exit 0

agent_id=$(mv_agent_id "$payload")

bin=$(mv_find_cli 2>/dev/null) && [ -n "$bin" ] || {
    mv_warn "MEMVAULT_HOOK_COMPLIANCE=1 but no MemVault CLI found; compliance not queued (set MEMVAULT_BIN or put memvault-cli on PATH)"
    mv_debug "memvault CLI not found; compliance not queued"
    exit 0
}

if "$bin" compliance-enqueue --hook-input "$payload" --agent-id "$agent_id" >/dev/null 2>"$errlog"; then
    mv_debug "compliance job enqueued"
else
    rc=$?
    detail=$(mv_last_line "$errlog")
    if [ -n "$detail" ]; then
        mv_warn "compliance enqueue failed (rc=$rc, bin=$bin): $detail"
    else
        mv_warn "compliance enqueue failed (rc=$rc, bin=$bin); rows left pending (set MEMVAULT_HOOK_DEBUG=1 for details)"
    fi
fi
exit 0
