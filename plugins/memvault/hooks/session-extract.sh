#!/bin/sh
# Stop hook: hand the session transcript to the extractor so durable learnings
# (preferences, corrections, decisions) land in the review inbox as unreviewed
# drafts — same trust boundary as every other auto-extracted source.
#
# OFF BY DEFAULT: automatic extraction is opt-in. The host (or user) enables
# it with MEMVAULT_HOOK_EXTRACT=1. Output of this hook is ignored by the host,
# so diagnostics go to the debug log only and every path exits 0.
#
# REST-only machines deliberately skip extraction: embedding a potentially
# large transcript into a JSON body from POSIX sh is where hooks quietly
# corrupt data. Use the CLI path (default) or the dashboard import instead.
#
# Environment:
#   MEMVAULT_HOOK_EXTRACT  1 = extract at session end (default 0)
#   MEMVAULT_AGENT_ID      agent identity stamped on the drafts (default claude-code)
#   MEMVAULT_BIN           explicit path to the MemVault CLI (optional)
#   MEMVAULT_HOOK_DEBUG    write diagnostics to ${TMPDIR:-/tmp}/memvault-hook.log

. "$(dirname "$0")/lib.sh"

[ "${MEMVAULT_HOOK_EXTRACT:-0}" = "1" ] || exit 0

payload=$(mktemp "${TMPDIR:-/tmp}/memvault-hook-XXXXXX") 2>/dev/null || exit 0
trap 'rm -f "$payload"' EXIT
# Stop payload (transcript_path, session_id, cwd) is parsed by the CLI.
cat >"$payload" 2>/dev/null || exit 0

bin=$(mv_find_cli 2>/dev/null) && [ -n "$bin" ] || {
    mv_debug "memvault CLI not found; transcript not extracted"
    exit 0
}

if "$bin" extract \
    --hook-input "$payload" \
    --save \
    --agent-id "${MEMVAULT_AGENT_ID:-claude-code}" >/dev/null 2>&1; then
    mv_debug "extract completed"
else
    mv_debug "extract failed (see MEMVAULT_HOOK_DEBUG run for details)"
fi
exit 0
