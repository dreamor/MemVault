#!/bin/sh
# SessionEnd hook: close the compliance rows opened by SessionStart.
#
# With an LLM extractor configured, each injected memory is judged against
# the transcript (followed / violated / unknown). Without one, rows are
# closed as explicitly Unknown instead of staying pending forever or being
# presented as a fabricated 100% compliance rate.
#
# Environment:
#   MEMVAULT_HOOK_COMPLIANCE  1 = finalize at SessionEnd (default 1); 0 disables
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
    mv_warn "MEMVAULT_HOOK_COMPLIANCE=1 but no MemVault CLI found; compliance not finalized (set MEMVAULT_BIN or put memvault-cli on PATH)"
    mv_debug "memvault CLI not found; compliance not finalized"
    exit 0
}

if "$bin" compliance-finalize     --hook-input "$payload"     --agent-id "$agent_id" >/dev/null 2>"$errlog"; then
    mv_debug "compliance finalization completed"
else
    rc=$?
    detail=$(mv_last_line "$errlog")
    if [ -n "$detail" ]; then
        mv_warn "compliance finalization failed (rc=$rc, bin=$bin): $detail"
    else
        mv_warn "compliance finalization failed (rc=$rc, bin=$bin); rows left pending (set MEMVAULT_HOOK_DEBUG=1 for details)"
    fi
fi
exit 0
