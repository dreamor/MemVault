#!/bin/sh
# SessionStart companion hook: best-effort self-heal for the local REST
# backend (the host of the embedded Web Dashboard).
#
# The backend is auto-started by the installers (`memvault-cli server start`
# is idempotent: it probes /api/health, and spawns memvault-mcp detached only
# when nothing answers). That process can die with the machine, so every
# session re-checks it — exactly how claude-mem keeps its worker/dashboard
# alive. This hook is purely additive: it prints nothing on stdout (the host
# treats stdout as the recall envelope) and always exits 0, so recall is never
# blocked by server flaps.
#
# Environment:
#   MEMVAULT_BIN   explicit path to the MemVault CLI (optional; never forwarded
#                  to the server-start child, where the CLI would mistake it
#                  for the MCP binary)
#   MEMVAULT_HOOK_DEBUG  write diagnostics to ${TMPDIR:-/tmp}/memvault-hook.log

. "$(dirname "$0")/lib.sh"

bin=$(mv_find_cli 2>/dev/null) && [ -n "$bin" ] || exit 0

# Time-box: a first-run model download is the only slow path, and a session
# must never wait on it. `server start` is left to finish in the background:
# the process exists (detached), so the health check it performs on the next
# hook invocation, or `memvault-cli dashboard`, picks it up.
#
# `env -u MEMVAULT_BIN`: that variable names the CLI *for hooks*, while the
# CLI's `server start` resolves it as the MCP binary. Forwarding it made
# `server start` spawn a CLI wrapper with `--transport http`, which fails with
# `unexpected argument '--transport'` and only shows up when the previous
# background server must be replaced. Strip it for this child only; the hook
# still uses `$bin` from the parent environment.
(
    if ! env -u MEMVAULT_BIN "$bin" server start --quiet >/dev/null 2>&1; then
        mv_debug "ensure-server: '$bin server start' failed; REST backend did not start"
        mv_warn "ensure-server: '$bin server start' failed; the REST backend did not start (see ${MEMVAULT_HOME:-$HOME/.memvault}/logs)"
    fi
) &
hook_pid=$!
mv_debug "ensure-server: launched '$bin server start' (pid $hook_pid)"
exit 0
