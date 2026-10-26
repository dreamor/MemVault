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
# manifest, so the script names the host itself. Two signals, in order:
#
# 1. Codex exports CODEX_* into the hook process environment on the CLI.
# 2. The hook payload, passed as $1 by callers (`mv_agent_id "$payload"`).
#    Codex Desktop does NOT export CODEX_* into hook subprocesses, so the
#    environment alone labelled every Codex session `claude-code` — wrong
#    memory/episode attribution, an `extract-cli:claude-code` watermark shared
#    with real Claude Code sessions, and Codex-scoped injection rules never
#    applying to Codex's own sessions.
#
# Override with MEMVAULT_AGENT_ID when detection is wrong (e.g. an exotic
# launcher) — and make sure the id has an entry in ~/.memvault/agents.yaml,
# otherwise it falls back to `default`, whose namespace_filter is global-only
# and drops project memory.
mv_default_agent_id() {
    if [ -n "${CODEX_THREAD_ID:-}${CODEX_SESSION_ID:-}${CODEX_HOME:-}${CODEX_VERSION:-}" ]; then
        printf 'codex\n'
        return 0
    fi
    if [ -n "${1:-}" ] && mv_payload_is_codex "$1"; then
        printf 'codex\n'
        return 0
    fi
    printf 'claude-code\n'
}

# True when the hook payload captured in $1 was written by Codex.
#
# Claude Code and Codex send the same envelope (session_id, transcript_path,
# cwd, hook_event_name), so identification rests on Codex-only evidence:
#
# - `turn_id`: Codex's own hook schema documents it as "Codex extension:
#   expose the active turn id to internal turn-scoped hooks", and requires it
#   on every turn-scoped event (Stop, UserPromptSubmit, Pre/PostToolUse,
#   PermissionRequest, Pre/PostCompact, Subagent*, Interrupt).
# - A rollout transcript path: Codex writes
#   <CODEX_HOME>/sessions/<YYYY>/<MM>/<DD>/rollout-<stamp>-<uuid>.jsonl while
#   Claude Code writes <config>/projects/<slug>/<uuid>.jsonl.
# - `model` together with `permission_mode`: both are required on every Codex
#   hook payload and neither is in Claude Code's envelope. This is what still
#   identifies SessionStart, whose payload carries no turn_id.
#
# A Claude Code transcript path outranks all of it, so a Codex-shaped token
# quoted inside a prompt or an assistant message cannot flip the verdict. The
# key patterns are anchored on the delimiter that precedes a real JSON key
# (`{`, `,` or whitespace), so an escaped \"turn_id\" inside a string value
# does not match one either.
mv_payload_is_codex() {
    [ -f "$1" ] || return 1
    grep -qF '/.claude/projects/' "$1" 2>/dev/null && return 1
    grep -Eq '(^|[,{[:space:]])"turn_id"[[:space:]]*:' "$1" 2>/dev/null && return 0
    grep -Eq 'sessions/[0-9]{4}/[0-9]{2}/[0-9]{2}/rollout-' "$1" 2>/dev/null && return 0
    if grep -Eq '(^|[,{[:space:]])"model"[[:space:]]*:' "$1" 2>/dev/null &&
        grep -Eq '(^|[,{[:space:]])"permission_mode"[[:space:]]*:' "$1" 2>/dev/null; then
        return 0
    fi
    return 1
}

# Effective agent identity for this hook run. $1 is the captured hook payload
# when the caller has one (it hosts the host-detection signal on launchers
# that export nothing useful into the hook environment).
mv_agent_id() {
    printf '%s\n' "${MEMVAULT_AGENT_ID:-$(mv_default_agent_id "${1:-}")}"
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
