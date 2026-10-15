#!/bin/sh
# Shared helpers for MemVault host hooks. POSIX sh only: Claude Code runs hooks
# through the user's shell on unix and through Git Bash on Windows, so a shell
# script works wherever hooks work — curl is the one soft dependency (REST
# fallback path only).
#
# Hook contract:
# - Hooks NEVER block the host session: every failure exits 0 quietly.
# - stdout is reserved for hook output the host reads (the SessionStart
#   envelope, plain REST text); it must never carry diagnostics.
# - One-line diagnostics go to stderr (mv_warn), so an ordinary user sees that
#   memory is not being injected without setting MEMVAULT_HOOK_DEBUG=1.
# - Detailed diagnostics are appended to ${TMPDIR}/memvault-hook.log, only
#   with MEMVAULT_HOOK_DEBUG=1.

# One diagnostic line on stderr. Always emitted — a memory outage that only
# shows up under MEMVAULT_HOOK_DEBUG=1 is a black box for everyone else.
mv_warn() {
    printf '[memvault] %s\n' "$1" >&2
}

# One diagnostic line, timestamped, gated behind MEMVAULT_HOOK_DEBUG=1.
mv_debug() {
    [ "${MEMVAULT_HOOK_DEBUG:-0}" = "1" ] || return 0
    stamp=$(date '+%Y-%m-%dT%H:%M:%S' 2>/dev/null || echo '?')
    printf '[memvault-hook %s] %s\n' "$stamp" "$1" \
        >>"${TMPDIR:-/tmp}/memvault-hook.log" 2>/dev/null || true
}

# Last (usually most specific) line of a captured stderr file, collapsed to a
# single line so a warning stays one line. Empty when there is nothing useful.
mv_last_line() {
    [ -f "$1" ] || return 0
    awk 'NF { last = $0 } END { if (last != "") print last }' "$1" 2>/dev/null
}

# Host-appropriate default agent identity. Claude Code and Codex load the same
# hooks.json and Codex offers no per-hook env we could set from that shared
# manifest, so the script names the host itself: Codex exports CODEX_* into the
# hook process environment, Claude Code does not. Override with
# MEMVAULT_AGENT_ID when that is wrong (e.g. an exotic launcher) — and make
# sure the id has an entry in ~/.memvault/agents.yaml, otherwise it falls back
# to `default`, whose namespace_filter is global-only and drops project memory.
mv_default_agent_id() {
    if [ -n "${CODEX_THREAD_ID:-}${CODEX_SESSION_ID:-}${CODEX_HOME:-}${CODEX_VERSION:-}" ]; then
        printf 'codex\n'
    else
        printf 'claude-code\n'
    fi
}

# Effective agent identity for this hook run.
mv_agent_id() {
    printf '%s\n' "${MEMVAULT_AGENT_ID:-$(mv_default_agent_id)}"
}

# Locate the MemVault CLI. Lookup order: explicit $MEMVAULT_BIN, then $PATH,
# then the install.sh prefix (which ships the binary as `memvault-cli` and is
# frequently missing from non-interactive PATH). Prints the path on success,
# exits non-zero when nothing is found.
mv_find_cli() {
    if [ -n "${MEMVAULT_BIN:-}" ] && [ -x "${MEMVAULT_BIN}" ]; then
        printf '%s\n' "$MEMVAULT_BIN"
        return 0
    fi
    if command -v memvault >/dev/null 2>&1; then
        command -v memvault
        return 0
    fi
    if command -v memvault-cli >/dev/null 2>&1; then
        command -v memvault-cli
        return 0
    fi
    for candidate in "${HOME:-/root}/.memvault/bin/memvault-cli" \
        "${HOME:-/root}/.memvault/bin/memvault"; do
        if [ -x "$candidate" ]; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done
    return 1
}

# Base URL of the optional HTTP backend (memvault-mcp --transport http).
mv_rest_base() {
    printf '%s\n' "${MEMVAULT_HTTP_URL:-http://127.0.0.1:3777}"
}

# True when a MemVault HTTP backend answers /health within 3s. Acting only on
# a live backend keeps hooks from opening the SQLite database alongside a
# running memvault-mcp process.
mv_rest_available() {
    command -v curl >/dev/null 2>&1 || return 1
    curl -fsS --max-time 3 "$(mv_rest_base)/health" >/dev/null 2>&1
}
