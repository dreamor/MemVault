#!/bin/sh
# Self-tests for the MemVault hook scripts (plugins/memvault/hooks/*.sh).
#
# A stub CLI records invocations and replays canned output, so the hook's
# control flow is exercised without a database. python3 (preinstalled on CI
# runners) validates JSON shapes. Exits non-zero when any check fails.
#
# Usage: sh plugins/memvault/tests/run-tests.sh
set -u

HERE=$(cd "$(dirname "$0")" && pwd)
HOOKS="$HERE/../hooks"
FIXTURES="$HERE/fixtures"

TMP=$(mktemp -d "${TMPDIR:-/tmp}/memvault-hook-tests-XXXXXX") || exit 1
trap 'rm -rf "$TMP"' EXIT
export TMPDIR="$TMP"

PASS=0
FAIL=0
SKIP=0
ok() { PASS=$((PASS + 1)); printf 'ok - %s\n' "$1"; }
fail() { FAIL=$((FAIL + 1)); printf 'not ok - %s\n' "$1"; }
skip() { SKIP=$((SKIP + 1)); printf 'ok - %s # SKIP: %s\n' "$1" "$2"; }

check_valid_json() { # $1 file, $2 description
    python3 - "$1" "$2" <<'PYEOF'
import json, sys
path, what = sys.argv[1], sys.argv[2]
with open(path, encoding="utf-8") as fh:
    data = json.load(fh)
assert data["hookSpecificOutput"]["hookEventName"] == "SessionStart", what
print("ok - %s" % what)
PYEOF
}

# Stub MemVault CLI: logs argv, copies the forwarded payload, and either
# emits a canned envelope (STUB_MODE=ok) or fails (STUB_MODE=fail).
export STUB_CALLS="$TMP/stub-calls.log"
export STUB_LAST_PAYLOAD="$TMP/stub-last-payload.json"
: >"$STUB_CALLS"
cat >"$TMP/stub-cli" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >>"$STUB_CALLS"
prev=""
for arg in "$@"; do
    [ "$prev" = "--hook-input" ] && cat "$arg" >"$STUB_LAST_PAYLOAD" 2>/dev/null
    prev="$arg"
done
[ "${STUB_MODE:-ok}" = "ok" ] || exit 3
printf '%s\n' '{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"MUST: stub memory"}}'
EOF
chmod +x "$TMP/stub-cli"

export MEMVAULT_BIN="$TMP/stub-cli"

# Isolated environment for "nothing available" scenarios: a minimal PATH with
# the shell utilities the hooks need and a curl stub that always fails, so the
# REST backend counts as unreachable no matter what runs on the test machine.
mkdir -p "$TMP/bin"
for tool in sh cat mktemp rm date dirname awk; do
    tool_path=$(command -v "$tool" 2>/dev/null)
    if [ -n "$tool_path" ]; then
        ln -s "$tool_path" "$TMP/bin/$tool"
    fi
done
printf '#!/bin/sh\nexit 1\n' >"$TMP/bin/curl"
chmod +x "$TMP/bin/curl"

# 1. SessionStart success path: exactly one valid envelope on stdout, exit 0.
if sh "$HOOKS/session-start.sh" <"$FIXTURES/session-start.json" >"$TMP/out" 2>/dev/null &&
    [ "$(grep -c . "$TMP/out")" -eq 1 ]; then
    if check_valid_json "$TMP/out" "session-start emits one valid envelope"; then :; else
        fail "session-start envelope invalid"
    fi
else
    fail "session-start success path"
fi

# 2. SessionStart CLI failure: falls back to REST (stubbed dead), exits 0 with
#    clean stdout, and leaves exactly one diagnosable warning on stderr — an
#    outage must be visible without MEMVAULT_HOOK_DEBUG=1. Env vars are passed
#    explicitly so inherited test state cannot leak into the scenario.
if env PATH="$TMP/bin" MEMVAULT_BIN="$TMP/stub-cli" STUB_MODE=fail \
    sh "$HOOKS/session-start.sh" <"$FIXTURES/session-start.json" \
    >"$TMP/out" 2>"$TMP/err" && [ ! -s "$TMP/out" ] && [ -s "$TMP/err" ] \
    && grep -q 'rc=3' "$TMP/err"; then
    ok "session-start CLI failure warns with its exit code, stdout stays clean"
else
    fail "session-start CLI failure warns with its exit code, stdout stays clean"
fi

# 3. No CLI, no REST: exit 0, empty stdout, stderr warning (isolated PATH).
if env PATH="$TMP/bin" HOME="$TMP" MEMVAULT_BIN= \
    sh "$HOOKS/session-start.sh" <"$FIXTURES/session-start.json" \
    >"$TMP/out" 2>"$TMP/err" && [ ! -s "$TMP/out" ] && [ -s "$TMP/err" ]; then
    ok "missing runtime warns on stderr, stdout stays clean"
else
    fail "missing runtime warns on stderr, stdout stays clean"
fi

# 4. Extract disabled by default: the stub is never called.
rm -f "$STUB_LAST_PAYLOAD" "$STUB_CALLS"
if sh "$HOOKS/session-extract.sh" </dev/null >/dev/null 2>&1 && [ ! -s "$STUB_CALLS" ]; then
    ok "extract off by default"
else
    fail "extract off by default"
fi

# 5. Extract enabled: payload forwarded intact for the CLI to parse, and an
#    explicit MEMVAULT_AGENT_ID is honored verbatim (deterministic regardless
#    of the host running the tests).
rm -f "$STUB_LAST_PAYLOAD"
sed "s|@TRANSCRIPT_PATH@|$FIXTURES/transcript.jsonl|" \
    "$FIXTURES/stop-payload.json" >"$TMP/stop.json"
if MEMVAULT_AGENT_ID=claude-code MEMVAULT_HOOK_EXTRACT=1 sh "$HOOKS/session-extract.sh" \
    <"$TMP/stop.json" >/dev/null 2>&1 &&
    grep -q -- '--save --agent-id claude-code' "$STUB_CALLS" &&
    grep -q '"transcript_path"' "$STUB_LAST_PAYLOAD" 2>/dev/null; then
    ok "extract forwards Stop payload, --save and explicit --agent-id"
else
    fail "extract forwards Stop payload, --save and explicit --agent-id"
fi

# 6. Host-aware default identity: with no override and Codex's env present the
#    hook must use `codex`, not `claude-code` (project-scoped rules apply only
#    to registered ids — see agents.example.yaml).
: >"$STUB_CALLS"
if env CODEX_SESSION_ID=test-session MEMVAULT_AGENT_ID= MEMVAULT_HOOK_EXTRACT=1 \
    sh "$HOOKS/session-extract.sh" <"$TMP/stop.json" >/dev/null 2>&1 &&
    grep -q -- '--agent-id codex' "$STUB_CALLS"; then
    ok "codex host env selects the codex agent id"
else
    fail "codex host env selects the codex agent id"
fi

# 7. Extract enabled with no CLI at all: exit 0, nothing recorded, warning out.
: >"$STUB_CALLS"
if env PATH="$TMP/bin" HOME="$TMP" MEMVAULT_BIN= MEMVAULT_HOOK_EXTRACT=1 \
    sh "$HOOKS/session-extract.sh" <"$TMP/stop.json" >"$TMP/out" 2>"$TMP/err" &&
    [ ! -s "$STUB_CALLS" ] && [ ! -s "$TMP/out" ] && [ -s "$TMP/err" ]; then
    ok "extract without CLI warns on stderr and records nothing"
else
    fail "extract without CLI warns on stderr and records nothing"
fi

# 8. Extract CLI failure: exit 0, warning on stderr.
: >"$STUB_CALLS"
if env STUB_MODE=fail MEMVAULT_HOOK_EXTRACT=1 \
    sh "$HOOKS/session-extract.sh" <"$TMP/stop.json" >"$TMP/out" 2>"$TMP/err" &&
    [ ! -s "$TMP/out" ] && [ -s "$TMP/err" ] && grep -q 'rc=3' "$TMP/err"; then
    ok "extract failure warns with its exit code"
else
    fail "extract failure warns with its exit code"
fi

# 9. Outcome inference disabled by default: the stub is never called.
: >"$STUB_CALLS"
if sh "$HOOKS/session-outcome.sh" </dev/null >/dev/null 2>&1 && [ ! -s "$STUB_CALLS" ]; then
    ok "outcome inference off by default"
else
    fail "outcome inference off by default"
fi

# 10. Outcome inference enabled: payload forwarded intact, --infer and an
#     explicit --agent-id are both present (no --save flag — unlike extract,
#     outcome has nothing to gate on review-approval at the hook layer).
rm -f "$STUB_LAST_PAYLOAD"
if MEMVAULT_AGENT_ID=claude-code MEMVAULT_HOOK_OUTCOME=1 sh "$HOOKS/session-outcome.sh" \
    <"$TMP/stop.json" >/dev/null 2>&1 &&
    grep -q -- '--infer --hook-input .* --agent-id claude-code' "$STUB_CALLS" &&
    grep -q '"transcript_path"' "$STUB_LAST_PAYLOAD" 2>/dev/null; then
    ok "outcome inference forwards Stop payload, --infer and explicit --agent-id"
else
    fail "outcome inference forwards Stop payload, --infer and explicit --agent-id"
fi

# 11. Outcome inference enabled with no CLI at all: exit 0, nothing recorded,
#     warning out.
: >"$STUB_CALLS"
if env PATH="$TMP/bin" HOME="$TMP" MEMVAULT_BIN= MEMVAULT_HOOK_OUTCOME=1 \
    sh "$HOOKS/session-outcome.sh" <"$TMP/stop.json" >"$TMP/out" 2>"$TMP/err" &&
    [ ! -s "$STUB_CALLS" ] && [ ! -s "$TMP/out" ] && [ -s "$TMP/err" ]; then
    ok "outcome inference without CLI warns on stderr and records nothing"
else
    fail "outcome inference without CLI warns on stderr and records nothing"
fi

# 12. Outcome inference CLI failure: exit 0, warning on stderr with the rc.
: >"$STUB_CALLS"
if env STUB_MODE=fail MEMVAULT_HOOK_OUTCOME=1 \
    sh "$HOOKS/session-outcome.sh" <"$TMP/stop.json" >"$TMP/out" 2>"$TMP/err" &&
    [ ! -s "$TMP/out" ] && [ -s "$TMP/err" ] && grep -q 'rc=3' "$TMP/err"; then
    ok "outcome inference failure warns with its exit code"
else
    fail "outcome inference failure warns with its exit code"
fi

# 13. Manifests parse and stay internally consistent.
# The marketplace index lives at the repository root, which the installed
# plugin cache does not ship. When it is absent, only the plugin-local
# manifests can be validated, so the repo-level checks are skipped (TAP
# `# SKIP`) instead of failing the run.
python3 - "$HERE/../../.." "$HERE/.." <<'PYEOF'
import json, os, sys
root = os.path.abspath(sys.argv[1])
plugin_dir = os.path.abspath(sys.argv[2])
def load(rel, base=root):
    with open(os.path.join(base, rel), encoding="utf-8") as fh:
        return json.load(fh)

repository = os.path.isfile(os.path.join(root, ".claude-plugin", "marketplace.json"))
claude = None
if repository:
    market = load(".claude-plugin/marketplace.json")
    entry = market["plugins"][0]
    plugin_dir = os.path.join(root, entry["source"].lstrip("./"))
    claude = load(os.path.join(plugin_dir, ".claude-plugin/plugin.json"))
    assert claude["name"] == entry["name"] == "memvault", "plugin/marketplace name mismatch"
else:
    claude_manifest = os.path.join(plugin_dir, ".claude-plugin", "plugin.json")
    if os.path.isfile(claude_manifest):
        claude = load(".claude-plugin/plugin.json", base=plugin_dir)
        assert claude["name"] == "memvault", "plugin name mismatch"

hooks = load(os.path.join(plugin_dir, "hooks", "hooks.json"))
for event in hooks["hooks"].values():
    for group in event:
        for hook in group["hooks"]:
            cmd = hook["command"]
            assert "${CLAUDE_PLUGIN_ROOT}" in cmd, "hooks must use $CLAUDE_PLUGIN_ROOT"
            script = cmd.split("/")[-1].rstrip('"')
            assert os.path.isfile(os.path.join(plugin_dir, "hooks", script)), script
# Codex/OpenAI reads the portable Agent Plugins manifest at the plugin root
# (plugins/memvault/plugin.json) and discovers hooks/hooks.json by default as
# long as the manifest does not itself define `hooks` -- an explicit hooks value
# (top-level or extensions.com.openai.hooks) replaces default-file discovery.
portable = load("plugin.json", base=plugin_dir)
assert portable.get("$schema") == "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json", \
    "portable plugin.json must declare the Agent Plugins schema"
assert portable.get("name") == "memvault", "portable plugin name mismatch"
if claude is not None:
    assert portable["name"] == claude["name"], "portable/Claude manifest name mismatch"
    assert portable.get("version") == claude["version"], "portable/Claude manifest version mismatch"
ext = portable.get("extensions", {}).get("com.openai", {}) or {}
assert "hooks" not in portable and "hooks" not in ext, \
    "defining hooks in the manifest would replace default hooks/hooks.json discovery"
assert os.path.isfile(os.path.join(plugin_dir, "hooks", "hooks.json")), \
    "hooks/hooks.json must exist for default hook discovery"
for rel, count in (("skills", 4), ("commands", 4)):
    actual = [n for n in os.listdir(os.path.join(plugin_dir, rel)) if not n.startswith(".")]
    assert len(actual) == count, f"expected {count} entries in {rel}, got {actual}"
print("ok - manifests parse and stay consistent")
PYEOF
rc=$?
if [ "$rc" -eq 0 ]; then
    if [ -f "$HERE/../../../.claude-plugin/marketplace.json" ]; then
        ok "manifest consistency checked"
    else
        skip "manifest consistency checked" "repo marketplace index absent (installed-plugin layout)"
    fi
else
    fail "manifest consistency checked"
fi

printf '\n%d passed, %d failed, %d skipped\n' "$PASS" "$FAIL" "$SKIP"
[ "$FAIL" -eq 0 ]
