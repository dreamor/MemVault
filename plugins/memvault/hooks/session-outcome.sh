#!/bin/sh
# Stop hook: hand the session transcript to outcome inference so real task
# outcomes (not just manual `memvault-cli outcome` calls) feed `bench` and
# the lesson-reflection pipeline. Independent of session-extract.sh — this
# hook only records episodic outcomes, never extracted preferences/facts.
#
# OFF BY DEFAULT: automatic outcome inference is opt-in. The host (or user)
# enables it with MEMVAULT_HOOK_OUTCOME=1. Output of this hook is ignored by
# the host (diagnostics therefore go to stderr for visibility, details to
# the debug log under MEMVAULT_HOOK_DEBUG=1) and every path exits 0.
#
# Inferred outcomes land in the review inbox exactly like any other
# auto-generated memory (human_reviewed=false) — this hook never bypasses
# review, it only decides WHETHER to record an episode at all.
#
# Environment:
#   MEMVAULT_HOOK_OUTCOME                1 = infer an outcome at session end (default 0)
#   MEMVAULT_HOOK_OUTCOME_MIN_FRICTION    friction floor for a Partial verdict (default 1)
#   MEMVAULT_HOOK_OUTCOME_MIN_CONFIDENCE  LLM judge confidence floor (default 0.6)
#   MEMVAULT_AGENT_ID                     agent identity stamped on the episode (default:
#                                         host-aware — `codex` under Codex, else `claude-code`)
#   MEMVAULT_BIN                          explicit path to the MemVault CLI (optional)
#   MEMVAULT_HOOK_DEBUG                   write diagnostics to ${TMPDIR:-/tmp}/memvault-hook.log

. "$(dirname "$0")/lib.sh"

[ "${MEMVAULT_HOOK_OUTCOME:-0}" = "1" ] || exit 0

payload=$(mktemp "${TMPDIR:-/tmp}/memvault-hook-XXXXXX") 2>/dev/null || exit 0
errlog=$(mktemp "${TMPDIR:-/tmp}/memvault-hook-err-XXXXXX") 2>/dev/null || exit 0
trap 'rm -f "$payload" "$errlog"' EXIT
# Stop payload (transcript_path, session_id, cwd) is parsed by the CLI.
cat >"$payload" 2>/dev/null || exit 0

agent_id=$(mv_agent_id)

bin=$(mv_find_cli 2>/dev/null) && [ -n "$bin" ] || {
    mv_warn "MEMVAULT_HOOK_OUTCOME=1 but no MemVault CLI found; outcome not inferred (set MEMVAULT_BIN or put memvault-cli on PATH)"
    mv_debug "memvault CLI not found; outcome not inferred"
    exit 0
}

if "$bin" outcome \
    --infer \
    --hook-input "$payload" \
    --agent-id "$agent_id" >/dev/null 2>"$errlog"; then
    mv_debug "outcome inference completed"
else
    rc=$?
    detail=$(mv_last_line "$errlog")
    if [ -n "$detail" ]; then
        mv_warn "outcome inference failed (rc=$rc, bin=$bin): $detail"
    else
        mv_warn "outcome inference failed (rc=$rc, bin=$bin); outcome not recorded (set MEMVAULT_HOOK_DEBUG=1 for details)"
    fi
fi
exit 0
