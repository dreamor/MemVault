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

# Version declared by the installed plugin bundle (not the marketplace or the
# CLI). Used to detect a Codex cache that is older than the Homebrew/CLI
# install and therefore cannot receive new hooks or skills until refreshed.
mv_plugin_version() {
    [ -f "${1:-}" ] || return 1
    awk -F'"' '/"version"[[:space:]]*:/ { print $4; exit }' "$1" 2>/dev/null
}

# Version reported by the MemVault CLI (`memvault 0.7.7` -> `0.7.7`).
mv_cli_version() {
    [ -n "${1:-}" ] || return 1
    "$1" --version 2>/dev/null | awk 'NF { print $NF; exit }'
}

# Locate the Codex CLI. Hooks launched by the desktop app do not always inherit
# the login shell's PATH, so an explicit override and the common install paths
# are checked after PATH.
mv_find_codex() {
    if [ -n "${MEMVAULT_CODEX_BIN:-}" ] && [ -x "${MEMVAULT_CODEX_BIN}" ]; then
        printf '%s\n' "${MEMVAULT_CODEX_BIN}"
        return 0
    fi
    if command -v codex >/dev/null 2>&1; then
        command -v codex
        return 0
    fi
    for candidate in /opt/homebrew/bin/codex /usr/local/bin/codex \
        "${HOME:-/root}/.local/bin/codex"; do
        if [ -x "$candidate" ]; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done
    return 1
}

# True when semver-like $1 is numerically newer than $2. Pre-release suffixes
# are ignored; the plugin only needs to decide whether the CLI has moved ahead
# of the cached bundle, not reproduce full semver ordering.
mv_version_gt() {
    awk -v a="${1:-}" -v b="${2:-}" 'BEGIN {
        sub(/[^0-9.].*$/, "", a)
        sub(/[^0-9.].*$/, "", b)
        na = split(a, x, ".")
        nb = split(b, y, ".")
        for (i = 1; i <= 3; i++) {
            xi = (i <= na && x[i] != "") ? x[i] + 0 : 0
            yi = (i <= nb && y[i] != "") ? y[i] + 0 : 0
            if (xi > yi) exit 0
            if (xi < yi) exit 1
        }
        exit 1
    }'
}

# Codex caches the installed plugin by version. Upgrading the CLI does not
# automatically refresh that cache, so a user can keep running last release's
# hooks/skills indefinitely. When a Codex session starts with a newer CLI than
# the bundle, queue an idempotent marketplace refresh in the background: hooks
# must never wait on a network fetch. The next Codex session loads the refreshed
# cache. Throttled to one attempt per six hours and guarded by a lock so a burst
# of sessions cannot launch concurrent upgrades.
mv_maybe_refresh_codex_plugin() { # <plugin-root> <agent-id> <cli-bin>
    [ "${MEMVAULT_HOOK_PLUGIN_UPGRADE:-1}" = "1" ] || return 0
    [ "${2:-}" = "codex" ] || return 0
    plugin_root=${1:-}
    cli_bin=${3:-}
    [ -n "$plugin_root" ] && [ -n "$cli_bin" ] || return 0

    plugin_version=$(mv_plugin_version "$plugin_root/.claude-plugin/plugin.json" 2>/dev/null) || return 0
    cli_version=$(mv_cli_version "$cli_bin" 2>/dev/null) || return 0
    [ -n "$plugin_version" ] && [ -n "$cli_version" ] || return 0
    [ "$plugin_version" = "$cli_version" ] && return 0
    mv_version_gt "$cli_version" "$plugin_version" || return 0

    codex_bin=$(mv_find_codex 2>/dev/null) || return 0
    state_dir=${MEMVAULT_PLUGIN_REFRESH_DIR:-${TMPDIR:-/tmp}/memvault-codex-plugin-refresh}
    mkdir -p "$state_dir" 2>/dev/null || return 0

    now=$(date +%s 2>/dev/null || printf '0')
    stamp="$state_dir/last-attempt"
    if [ -f "$stamp" ]; then
        last=$(cat "$stamp" 2>/dev/null || printf '0')
        case "$last" in ''|*[!0-9]*) last=0 ;; esac
        if [ "$now" -gt 0 ] && [ "$last" -gt 0 ] && [ $((now - last)) -lt 21600 ]; then
            return 0
        fi
    fi

    lock="$state_dir/lock"
    if ! mkdir "$lock" 2>/dev/null; then
        # A concurrent SessionStart owns the updater. Never break a lock we
        # cannot age: a missing stamp means the owner is in its tiny
        # mkdir→stamp window. A killed updater leaves a stamp, so an old stamp
        # makes the lock safely reclaimable without disabling self-healing.
        [ -f "$stamp" ] || return 0
        lock_last=$(cat "$stamp" 2>/dev/null || printf '0')
        case "$lock_last" in ''|*[!0-9]*) lock_last=0 ;; esac
        if [ "$now" -le 0 ] || [ "$lock_last" -le 0 ] || [ $((now - lock_last)) -lt 21600 ]; then
            return 0
        fi
        rmdir "$lock" 2>/dev/null || return 0
        mkdir "$lock" 2>/dev/null || return 0
    fi
    [ "$now" -gt 0 ] && printf '%s\n' "$now" >"$stamp" 2>/dev/null || true

    log="$state_dir/refresh.log"
    mv_debug "codex plugin $plugin_version is older than CLI $cli_version; scheduling marketplace refresh"
    (
        printf '[%s] plugin=%s cli=%s\n' \
            "$(date '+%Y-%m-%dT%H:%M:%S' 2>/dev/null || printf '?')" \
            "$plugin_version" "$cli_version" >>"$log" 2>/dev/null || true
        "$codex_bin" plugin marketplace upgrade memvault >>"$log" 2>&1
        rc=$?
        if [ "$rc" -eq 0 ]; then
            cache_root="${CODEX_HOME:-${HOME:-/root}/.codex}/plugins/cache/memvault/memvault/$cli_version"
            if [ -d "$cache_root" ]; then
                printf '[%s] refreshed; cache=%s\n' \
                    "$(date '+%Y-%m-%dT%H:%M:%S' 2>/dev/null || printf '?')" \
                    "$cache_root" >>"$log" 2>/dev/null || true
            else
                printf '[%s] marketplace upgraded but cache %s is missing; restart Codex and reinstall memvault@memvault if it stays stale\n' \
                    "$(date '+%Y-%m-%dT%H:%M:%S' 2>/dev/null || printf '?')" \
                    "$cache_root" >>"$log" 2>/dev/null || true
            fi
        else
            printf '[%s] refresh failed rc=%s (retry after throttle window)\n' \
                "$(date '+%Y-%m-%dT%H:%M:%S' 2>/dev/null || printf '?')" "$rc" \
                >>"$log" 2>/dev/null || true
        fi
        rmdir "$lock" 2>/dev/null || true
    ) </dev/null >/dev/null 2>&1 &
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
