#!/usr/bin/env bash
# End-to-end release check: drives the REAL built binaries and a REAL REST server
# across every user-visible behaviour this release added or changed.
#
# Why this exists alongside `cargo test`. The unit and integration suites are the
# primary gate and they are thorough. But several defects in this release were
# found only by running the actual binaries end to end, and none of them were
# visible to the suites:
#
#   - `directive_still_says_old`: an edit to `content` approved cleanly, superseded
#     its target, and changed no behaviour, because injection renders
#     `instruction`. Every unit test passed.
#   - `sync` wrote unreviewed and expired memories into AGENTS.md, bypassing the
#     review gate entirely. The store-level filters were all correct and tested;
#     the second injection channel simply did not use them.
#   - `init`'s skip message said "already set in your environment" for keys the
#     env *file* had set, because the CLI loads the file at startup. Correct
#     outcome, wrong guidance, and only visible on a second real run.
#   - The reranker dropped `as_of_source`, making every point-in-time result
#     indistinguishable from a current one.
#
# What they have in common is that the bug lived in the wiring between layers, not
# inside any layer. So this script asserts on observable behaviour of the shipped
# artifacts — CLI stdout, REST JSON, and the bytes of generated files — and never
# on internal state.
#
# No LLM key and no embedding provider are required: everything here runs on the
# deterministic paths, which is also what makes it safe to gate a release on.
#
# Usage:
#   cargo build -p memvault-cli -p memvault-mcp
#   bash scripts/e2e-release-check.sh
#
# Exits non-zero on the first count of failures, naming each one.

set -uo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
CLI_BIN="$ROOT/target/debug/memvault-cli"
MCP_BIN="$ROOT/target/debug/memvault-mcp"

for b in "$CLI_BIN" "$MCP_BIN"; do
    [ -x "$b" ] || { echo "missing binary: $b (run: cargo build -p memvault-cli -p memvault-mcp)" >&2; exit 2; }
done

PASS=0
FAIL=0
FAILED_NAMES=()

# ─── assertion helpers ────────────────────────────────────────────────────
# Every check names what it is proving, so a failure says which *behaviour*
# broke rather than which line number.

ok()   { PASS=$((PASS+1)); printf '  \033[32mok\033[0m   %s\n' "$1"; }
bad()  { FAIL=$((FAIL+1)); FAILED_NAMES+=("$1"); printf '  \033[31mFAIL\033[0m %s\n' "$1"; [ -n "${2:-}" ] && printf '       %s\n' "$2"; }

# check <name> <actual> <expected>
check() {
    if [ "$2" = "$3" ]; then ok "$1"; else bad "$1" "expected [$3], got [$2]"; fi
}
# check_contains <name> <haystack> <needle>
check_contains() {
    case "$2" in
        *"$3"*) ok "$1" ;;
        *) bad "$1" "expected to contain [$3], got: $(printf '%s' "$2" | head -c 300)" ;;
    esac
}
# check_not_contains <name> <haystack> <needle>
check_not_contains() {
    case "$2" in
        *"$3"*) bad "$1" "must NOT contain [$3]" ;;
        *) ok "$1" ;;
    esac
}
section() { printf '\n\033[1m%s\033[0m\n' "$1"; }

# RFC3339 helper: now + N seconds, microsecond precision, UTC.
stamp_offset() {
    python3 -c "
import datetime,sys
print((datetime.datetime.now(datetime.timezone.utc)+datetime.timedelta(seconds=int(sys.argv[1]))).strftime('%Y-%m-%dT%H:%M:%S.%fZ'))" "$1"
}

# ─── scratch space ────────────────────────────────────────────────────────

WORK=$(mktemp -d)
cleanup() {
    [ -n "${SRV_PID:-}" ] && kill "$SRV_PID" 2>/dev/null
    rm -rf "$WORK"
}
trap cleanup EXIT

export MEMVAULT_HOME="$WORK/home"
mkdir -p "$MEMVAULT_HOME"
DB="$MEMVAULT_HOME/data.db"
# A bash ARRAY, not a string: `CLI="$BIN --db $DB"` invoked as `"$CLI"` treats
# the whole thing as one command name and fails with "No such file or directory".
CLI=("$CLI_BIN" --db "$DB")

# Embeddings off for the whole run: MUST injection is deterministic (§6.1.2), so
# every assertion here holds without a provider, and the run stays fast and
# offline.
export MEMVAULT_EMBEDDING_PROVIDER=off

PORT=$(python3 -c "import socket;s=socket.socket();s.bind(('127.0.0.1',0));print(s.getsockname()[1]);s.close()")
"$MCP_BIN" --db "$DB" --transport http --port "$PORT" >/dev/null 2>&1 &
SRV_PID=$!
API="http://127.0.0.1:$PORT"
for _ in $(seq 1 60); do
    curl -sf "$API/health" >/dev/null 2>&1 && break
    sleep 0.5
done
curl -sf "$API/health" >/dev/null 2>&1 || { echo "REST server did not become healthy" >&2; exit 2; }

# json_get <url> <jq-ish python expr on the parsed envelope's data>
api() { curl -s -X "${1:-GET}" "$API$2" ${3:+-H 'content-type: application/json' -d "$3"}; }
data_field() { python3 -c "
import sys,json
d=json.load(sys.stdin)
d=d.get('data')
for k in sys.argv[1].split('.'):
    if d is None: break
    d = d[int(k)] if isinstance(d,list) else d.get(k)
print('' if d is None else (json.dumps(d) if isinstance(d,(dict,list)) else d))" "$1"; }

# ══════════════════════════════════════════════════════════════════════════
section "1. Scenario profiles"
# ══════════════════════════════════════════════════════════════════════════

ENVF="$WORK/profile.env"
OUT=$("$CLI_BIN" init --list 2>&1)
for p in coding-agent assistant team edge-lite; do
    check_contains "init --list names $p" "$OUT" "$p"
done
check_contains "init --list states the precedence guarantee" "$OUT" "lowest-precedence"

"$CLI_BIN" init --profile edge-lite --dry-run --env-file "$ENVF" >/dev/null 2>&1
[ -e "$ENVF" ] && bad "--dry-run must not write" || ok "--dry-run writes nothing"

"$CLI_BIN" init --profile coding-agent --env-file "$ENVF" >/dev/null 2>&1
check_contains "generated env selects the markdown backend" "$(cat "$ENVF")" "MEMVAULT_STORAGE_BACKEND=markdown"
# The markdown backend refuses to start without a directory, so a profile that
# picks it must supply one.
check_contains "generated env supplies the required markdown dir" "$(cat "$ENVF")" "MEMVAULT_MARKDOWN_DIR="
grep -q "API_KEY" "$ENVF" && bad "profile must never write a secret" || ok "profile writes no secrets"

FIRST=$(cat "$ENVF")
"$CLI_BIN" init --profile coding-agent --env-file "$ENVF" >/dev/null 2>&1
check "second init is idempotent" "$(cat "$ENVF")" "$FIRST"

OUT=$("$CLI_BIN" init --profile enterprize --env-file "$ENVF" 2>&1)
check_contains "unknown profile is refused, not defaulted" "$OUT" "not a known profile"
check_contains "unknown profile error lists the valid names" "$OUT" "edge-lite"

# A key held by the environment must not be written into the file: doing so would
# PROMOTE it above the environment on the next start, which is the one way a
# lowest-precedence layer could clobber an explicit override.
ENVF2="$WORK/profile2.env"
OUT=$(MEMVAULT_DB_POOL_SIZE=99 "$CLI_BIN" init --profile team --env-file "$ENVF2" 2>&1)
grep -q "MEMVAULT_DB_POOL_SIZE" "$ENVF2" \
    && bad "env-held key must not be written to the file" \
    || ok "env-held key is left to the environment"
check_contains "skip message says the key came from the environment" "$OUT" "already set in your environment"

# ══════════════════════════════════════════════════════════════════════════
section "2. expires_at — temporary facts retire themselves"
# ══════════════════════════════════════════════════════════════════════════

LAPSED=$("${CLI[@]}" save --content "the deploy freeze runs until friday" --expires-at "2026-01-01T00:00:00Z" 2>&1 | grep -oE 'mem_[a-f0-9]+')
FUTURE=$("${CLI[@]}" save --content "the review window is open this week" --ttl 7d 2>&1 | grep -oE 'mem_[a-f0-9]+')
PERMANENT=$("${CLI[@]}" save --content "always use pnpm not npm" 2>&1 | grep -oE 'mem_[a-f0-9]+')

check_contains "expired memory is excluded from keyword search" \
    "$("${CLI[@]}" search --query "deploy freeze" 2>&1)" "No memories found"
check_contains "a memory inside its window is still found" \
    "$("${CLI[@]}" search --query "review window" 2>&1)" "$FUTURE"
# `list` is the inspection path and deliberately filters nothing — but it must say
# which entries are excluded from retrieval, or an expired memory looks identical
# to a live one.
LIST=$("${CLI[@]}" list --limit 20 2>&1)
check_contains "list still shows the expired memory" "$LIST" "$LAPSED"
check_contains "list marks it expired and states the consequence" "$LIST" "not injected"
check_contains "list marks an upcoming expiry without alarming" "$LIST" "[expires "
check_not_contains "a permanent memory carries no time marker" \
    "$(printf '%s' "$LIST" | grep "$PERMANENT")" "[expires"

# The shared pool injects into EVERY session, so it is the most damaging place a
# lapsed fact could surface.
SHARED=$(api POST /api/memories '{"content":"team standup is at nine","priority":"MUST","visibility":"shared","expires_at":"2026-01-01T00:00:00Z"}' | data_field id)
POOL=$(api POST /api/session '{"agent_id":"claude-desktop","context_hint":"standup"}' | data_field formatted)
check_not_contains "an expired shared MUST does not reach a session" "$POOL" "team standup is at nine"

# ══════════════════════════════════════════════════════════════════════════
section "3. Three time axes stay separate"
# ══════════════════════════════════════════════════════════════════════════

DATED=$(api POST /api/memories '{"content":"the march incident review concluded","occurred_at":"2026-03-10T00:00:00Z"}' | data_field id)
MARCH=$(api POST /api/search '{"query":"incident review","mode":"keyword","occurred_from":"2026-03-01T00:00:00Z","occurred_to":"2026-03-31T00:00:00Z"}' | python3 -c "import sys,json;print(len(json.load(sys.stdin)['data']))")
check "event-time window selects the dated memory" "$MARCH" "1"
APRIL=$(api POST /api/search '{"query":"incident review","mode":"keyword","occurred_from":"2026-04-01T00:00:00Z","occurred_to":"2026-04-30T00:00:00Z"}' | python3 -c "import sys,json;print(len(json.load(sys.stdin)['data']))")
check "a different window excludes it" "$APRIL" "0"
# An inverted window is a 400, not an empty result: empty would be correct and
# completely unhelpful, sending the caller to debug their data instead of a typo.
INVERTED=$(curl -s -o /dev/null -w '%{http_code}' -X POST "$API/api/search" -H 'content-type: application/json' \
    -d '{"query":"x","mode":"keyword","occurred_from":"2026-06-01T00:00:00Z","occurred_to":"2026-01-01T00:00:00Z"}')
check "inverted event-time window is a 400" "$INVERTED" "400"
BADSTAMP=$(curl -s -o /dev/null -w '%{http_code}' -X POST "$API/api/search" -H 'content-type: application/json' \
    -d '{"query":"x","mode":"keyword","as_of":"last tuesday"}')
check "malformed as_of is a 400, not a silently ignored filter" "$BADSTAMP" "400"

# occurred_at must be settable on the direct-save path, not only via extraction.
ROUNDTRIP=$(api GET "/api/memories" | python3 -c "
import sys,json
for m in json.load(sys.stdin)['data']:
    if m['id']=='$DATED': print(m.get('occurred_at'))")
check "occurred_at survives the direct save path" "$ROUNDTRIP" "2026-03-10T00:00:00Z"

# ══════════════════════════════════════════════════════════════════════════
section "4. as_of — valid-time travel and historical recall"
# ══════════════════════════════════════════════════════════════════════════

TRAVEL=$(api POST /api/memories '{"content":"the oncall pager belongs to zephyr team","priority":"REFERENCE"}' | data_field id)
MID=$(stamp_offset 1); sleep 2
api PUT "/api/memories/$TRAVEL" '{"content":"the oncall pager belongs to atlas team"}' >/dev/null

check_contains "current search no longer finds the retired wording" \
    "$(api POST /api/search '{"query":"zephyr","mode":"keyword"}' | python3 -c 'import sys,json;print(len(json.load(sys.stdin)["data"]))')" "0"
HIST=$(api POST /api/search "{\"query\":\"zephyr\",\"mode\":\"keyword\",\"as_of\":\"$MID\"}")
check_contains "as_of search recalls it from the historical index" \
    "$(printf '%s' "$HIST" | python3 -c 'import sys,json;print(len(json.load(sys.stdin)["data"]))')" "1"
check_contains "historical recall is tagged distinctly" "$HIST" "hist-kw#"
check_contains "as_of search returns the text it had then" "$HIST" "zephyr team"
check_contains "result reports it was reconstructed from a snapshot" "$HIST" '"source":"history"'

check_contains "cli as-of reports the snapshot it came from" \
    "$("${CLI[@]}" as-of "$TRAVEL" --at "$MID" 2>&1)" "reconstructed from history snapshot"
check_contains "cli as-of now reports the live row" \
    "$("${CLI[@]}" as-of "$TRAVEL" 2>&1)" "live row"
BEFORE=$(stamp_offset -3600)
check_contains "as-of before creation returns no state, not the current row" \
    "$("${CLI[@]}" as-of "$TRAVEL" --at "$BEFORE" 2>&1)" "No state for"

# list --as-of must include a memory deleted AFTER that instant: at the instant it
# was still the current truth.
TODELETE=$(api POST /api/memories '{"content":"a fact that gets deleted later"}' | data_field id)
MID2=$(stamp_offset 1); sleep 2
api DELETE "/api/memories/$TODELETE" >/dev/null
check_contains "list --as-of includes a memory deleted afterwards" \
    "$("${CLI[@]}" list --as-of "$MID2" --limit 50 2>&1)" "$TODELETE"

# ══════════════════════════════════════════════════════════════════════════
section "5. Review-gated edit proposals"
# ══════════════════════════════════════════════════════════════════════════

# `--force` on purpose: delta-write could merge this into an earlier similar
# memory and quietly drop the instruction, which would make every assertion below
# about the directive meaningless. Assert the instruction landed before relying on
# it — a test that passes because the field it checks is absent is worse than one
# that fails.
TARGET=$("${CLI[@]}" save --force --content "always run migrations before deploy" --instruction "[MUST] run migrations before deploy" --priority MUST 2>&1 | grep -oE 'mem_[a-f0-9]+')
STORED_INSTRUCTION=$(api GET "/api/memories?limit=200" | python3 -c "
import sys,json
for m in json.load(sys.stdin)['data']:
    if m['id']=='$TARGET': print(m.get('instruction') or '')")
check "the target's directive was stored as given" "$STORED_INSTRUCTION" "[MUST] run migrations before deploy"
BASELINE=$(api POST /api/session '{"agent_id":"claude-desktop","context_hint":"deploy migrations"}' | data_field formatted)
check_contains "the MUST is injected as a directive before any proposal" "$BASELINE" "run migrations before deploy"

# First propose against `content` only. The target's instruction says the same
# thing, so this edit would change no behaviour — the harness must warn rather than
# let it look successful. (This e2e script originally asserted the opposite and
# failed, which is the warning earning its keep.)
BODY_ONLY=$("${CLI[@]}" propose-edit --id "$TARGET" --old "before deploy" --new "only after approval" --agent-id claude-code 2>&1)
check_contains "warns when a content-only edit would not change behaviour" "$BODY_ONLY" "will NOT change agent behaviour"
check_contains "and names the follow-up command" "$BODY_ONLY" "--field instruction"
BODY_ONLY_ID=$(printf '%s' "$BODY_ONLY" | grep -oE 'proposal: mem_[a-f0-9]+' | grep -oE 'mem_[a-f0-9]+')
"${CLI[@]}" review --reject "$BODY_ONLY_ID" >/dev/null 2>&1

# Now propose against the field injection actually renders.
PROP_OUT=$("${CLI[@]}" propose-edit --id "$TARGET" --old "run migrations before deploy" --new "run migrations only after approval" --field instruction --agent-id claude-code 2>&1)
PROP=$(printf '%s' "$PROP_OUT" | grep -oE 'proposal: mem_[a-f0-9]+' | grep -oE 'mem_[a-f0-9]+')
check_contains "propose-edit states the target is unchanged" "$PROP_OUT" "UNCHANGED and still injecting"
check_not_contains "an instruction-field edit is not falsely warned" "$PROP_OUT" "will NOT change"
# The critical safety property: while a proposal is pending the target must keep
# injecting. If it stopped, any agent could suppress any MUST merely by proposing
# an edit to it.
PENDING=$(api POST /api/session '{"agent_id":"claude-desktop","context_hint":"deploy migrations"}' | data_field formatted)
check_contains "while pending, injection still carries the ORIGINAL directive" "$PENDING" "run migrations before deploy"
check_not_contains "the proposal text has not taken effect" "$PENDING" "only after approval"

# The inbox must make a proposal distinguishable from a near-duplicate draft.
INBOX=$(api GET "/api/inbox?limit=50")
check_contains "inbox labels the proposal with its target" "$INBOX" '"proposes_supersede_of":"'"$TARGET"'"'
check_contains "inbox ships the target's current text for diffing" "$INBOX" "always run migrations before deploy"
check_contains "inbox states what approving would do" "$INBOX" "superseded"
check_contains "cli review flags the proposal distinctly" \
    "$("${CLI[@]}" review --limit 50 2>&1)" "[proposes edit to $TARGET]"

# A proposal must not reach retrieval as if it were current truth.
check_contains "the pending proposal is excluded from search" \
    "$(api POST /api/search '{"query":"only after approval","mode":"keyword"}' | python3 -c 'import sys,json;print(len(json.load(sys.stdin)["data"]))')" "0"

APPROVE=$(api POST "/api/inbox/$PROP/approve" '{}')
check_contains "approving reports which memory it superseded" "$APPROVE" "$TARGET"
AFTER=$(api POST /api/session '{"agent_id":"claude-desktop","context_hint":"deploy migrations"}' | data_field formatted)
check_contains "after approval the NEW directive is what gets injected" "$AFTER" "only after approval"
check_not_contains "and the old one no longer is" "$AFTER" "run migrations before deploy"

# Rejecting a second proposal must leave its target untouched.
T2=$("${CLI[@]}" save --content "never force-push to main" --priority MUST 2>&1 | grep -oE 'mem_[a-f0-9]+')
P2=$("${CLI[@]}" propose-edit --id "$T2" --old "never force-push" --new "sometimes force-push" 2>&1 | grep -oE 'proposal: mem_[a-f0-9]+' | grep -oE 'mem_[a-f0-9]+')
"${CLI[@]}" review --reject "$P2" >/dev/null 2>&1
check_contains "a rejected proposal leaves the target intact" \
    "$("${CLI[@]}" list --limit 50 2>&1 | grep "$T2")" "never force-push to main"

# Ambiguity must be refused rather than guessed.
AMBIG=$("${CLI[@]}" propose-edit --id "$T2" --old "o" --new "x" 2>&1)
check_contains "an ambiguous anchor is refused with the count" "$AMBIG" "times"
check_contains "and the error says how to fix it" "$AMBIG" "unambiguous"

# ══════════════════════════════════════════════════════════════════════════
section "6. Progressive disclosure under a tight budget"
# ══════════════════════════════════════════════════════════════════════════

# A second home with a 120-token budget, so the trim path is actually exercised.
TIGHT_HOME="$WORK/tight"
mkdir -p "$TIGHT_HOME"
cat > "$TIGHT_HOME/agents.yaml" <<'YAML'
agents:
  - id: tight-agent
    agent_type: coding-assistant
    inject_rules:
      max_memories: 8
      token_budget: 120
      priority_order: ["MUST", "REFERENCE"]
      namespace_filter: ["global", "project:*"]
      exclude_types: []
YAML
TIGHT_PORT=$(python3 -c "import socket;s=socket.socket();s.bind(('127.0.0.1',0));print(s.getsockname()[1]);s.close()")
MEMVAULT_HOME="$TIGHT_HOME" MEMVAULT_EMBEDDING_PROVIDER=off \
    "$MCP_BIN" --db "$TIGHT_HOME/data.db" --transport http --port "$TIGHT_PORT" >/dev/null 2>&1 &
TIGHT_PID=$!
for _ in $(seq 1 60); do curl -sf "http://127.0.0.1:$TIGHT_PORT/health" >/dev/null 2>&1 && break; sleep 0.5; done
TIGHT_CLI=("$CLI_BIN" --db "$TIGHT_HOME/data.db")
for topic in alpha bravo charlie delta; do
    BODY=$(python3 -c "print('Distinct policy for the $topic subsystem covering retries, backoff, circuit breaking and the escalation path that oncall must follow when it trips. ' * 5)")
    "${TIGHT_CLI[@]}" save --force --content "$BODY" >/dev/null 2>&1
done
TIGHT=$(curl -s -X POST "http://127.0.0.1:$TIGHT_PORT/api/session" -H 'content-type: application/json' \
    -d '{"agent_id":"tight-agent","context_hint":"policy"}')
INJECTED=$(printf '%s' "$TIGHT" | python3 -c 'import sys,json;print(len(json.load(sys.stdin)["data"]["results"]))')
DEGRADED=$(printf '%s' "$TIGHT" | python3 -c 'import sys,json;print(len(json.load(sys.stdin)["data"].get("degraded") or []))')
SKIPPED=$(printf '%s' "$TIGHT" | python3 -c 'import sys,json;print(len(json.load(sys.stdin)["data"].get("skipped") or []))')
# Under the previous behaviour all four were cut: the first did not fit and the
# loop broke. Now at least one is delivered in shortened form.
[ "$INJECTED" -ge 1 ] && ok "a tight budget still delivers at least one memory (was 0 before disclosure)" \
    || bad "tight budget delivered nothing" "injected=$INJECTED"
[ "$DEGRADED" -ge 1 ] && ok "the delivered memory is reported as degraded" \
    || bad "no degradation reported" "degraded=$DEGRADED"
check_contains "degraded text carries the abbreviation marker" \
    "$(printf '%s' "$TIGHT" | data_field formatted)" "[…]"
[ "$SKIPPED" -ge 1 ] && ok "memories that did not fit are still reported as skipped" \
    || bad "nothing reported skipped" "skipped=$SKIPPED"
# degraded and skipped must stay separate: a degraded memory WAS delivered, and
# folding it into "dropped" would tell an admin a rule never reached the agent.
check_contains "degraded and skipped are reported as distinct outcomes" "$TIGHT" '"degraded"'

# A MUST must never be shortened, whatever the budget.
"${TIGHT_CLI[@]}" save --content "$(python3 -c "print('A very long binding rule that would never fit the tiny budget. ' * 12)")" \
    --instruction "[MUST] the long binding rule" --priority MUST >/dev/null 2>&1
MUSTFITS=$(curl -s -X POST "http://127.0.0.1:$TIGHT_PORT/api/session" -H 'content-type: application/json' \
    -d '{"agent_id":"tight-agent","context_hint":"binding rule"}' | data_field formatted)
check_contains "a MUST is injected verbatim despite the budget" "$MUSTFITS" "[MUST] the long binding rule"
kill "$TIGHT_PID" 2>/dev/null

# ══════════════════════════════════════════════════════════════════════════
section "7. sync is an injection channel and filters like one"
# ══════════════════════════════════════════════════════════════════════════

PROJ="$WORK/project"
mkdir -p "$PROJ"
printf '[package]\nname = "demo"\n' > "$PROJ/Cargo.toml"
SYNC_OUT=$(cd "$PROJ" && "${CLI[@]}" sync 2>&1)
check_contains "sync reports what it held back" "$SYNC_OUT" "Held back"
check_contains "and names the expiry reason" "$SYNC_OUT" "expires_at bound"
AGENTS_MD=$(cat "$PROJ/AGENTS.md" 2>/dev/null || echo "")
check_not_contains "an expired directive never reaches AGENTS.md" "$AGENTS_MD" "freeze all deploys"
check_not_contains "nor the lapsed freeze fact" "$AGENTS_MD" "the deploy freeze runs until friday"
# Control: a live reviewed MUST must still be written, or the assertions above
# would pass merely because sync wrote nothing.
check_contains "a live reviewed MUST is still written" "$AGENTS_MD" "only after approval"
# Unreviewed drafts must not leak into the generated files either — that was the
# gate bypass this release closed.
"${CLI[@]}" save --content "an unreviewed sounding preference" >/dev/null 2>&1
api POST /api/memories '{"content":"an ai-extracted draft that nobody reviewed","priority":"MUST"}' >/dev/null
(cd "$PROJ" && "${CLI[@]}" sync >/dev/null 2>&1)
AGENTS_MD2=$(cat "$PROJ/AGENTS.md" 2>/dev/null || echo "")
check_not_contains "an unreviewed AI draft does not reach AGENTS.md" "$AGENTS_MD2" "nobody reviewed"

# ══════════════════════════════════════════════════════════════════════════
section "8. decay, doctor, update, and the export round trip"
# ══════════════════════════════════════════════════════════════════════════

DECAY=$(api POST /api/decay '{}')
check_contains "decay reports expired archives separately" "$DECAY" "expired_archived"
EXPIRED_COUNT=$(printf '%s' "$DECAY" | data_field expired_archived)
[ "${EXPIRED_COUNT:-0}" -ge 1 ] && ok "decay archived at least one expired memory" \
    || bad "decay archived no expired memory" "expired_archived=$EXPIRED_COUNT"

# Both checks report only when the condition exists, so create each one rather
# than asserting on a clean vault.
ORPHAN_TARGET=$(api POST /api/memories '{"content":"a fact that will be deleted"}' | data_field id)
ORPHAN_PROP=$("${CLI[@]}" propose-edit --id "$ORPHAN_TARGET" --old "will be deleted" --new "was deleted" 2>&1 | grep -oE 'proposal: mem_[a-f0-9]+' | grep -oE 'mem_[a-f0-9]+')
api DELETE "/api/memories/$ORPHAN_TARGET" >/dev/null
"${CLI[@]}" save --content "a fact that expires and is never archived" --expires-at "2026-01-01T00:00:00Z" >/dev/null 2>&1
DOC=$("${CLI[@]}" doctor 2>&1)
check_contains "doctor reports an orphaned proposal" "$DOC" "orphaned_proposal"
check_contains "and names the missing target" "$DOC" "$ORPHAN_TARGET"
check_contains "doctor reports an expired-but-unarchived memory" "$DOC" "expired_unarchived"
check_contains "and names the fix" "$DOC" "decay"

# update: set, then lift, a valid-time bound — the path that did not exist before.
UPD=$(api POST /api/memories '{"content":"the migration window is sunday","priority":"REFERENCE"}' | data_field id)
"${CLI[@]}" update "$UPD" --expires-at "2026-01-01T00:00:00Z" >/dev/null 2>&1
check_contains "update can set expires_at after save" \
    "$("${CLI[@]}" list --limit 100 2>&1 | grep "$UPD")" "expired"
"${CLI[@]}" update "$UPD" --clear-expires >/dev/null 2>&1
check_not_contains "update can lift it again" \
    "$("${CLI[@]}" list --limit 100 2>&1 | grep "$UPD")" "expired"
NOARGS=$("${CLI[@]}" update "$UPD" 2>&1)
check_contains "update with no fields is a usage error, not a silent no-op" "$NOARGS" "at least one field"
BADTS=$("${CLI[@]}" update "$UPD" --expires-at "next friday" 2>&1)
check_contains "update rejects a malformed timestamp" "$BADTS" "RFC3339"
PRIO=$("${CLI[@]}" update "$UPD" --priority MUST 2>&1)
check_contains "changing priority moves the layer with it" "$PRIO" "L3"

# Export/import must carry the new fields, and the directory form must round trip.
EXPORT_PROP_TARGET=$(api POST /api/memories '{"content":"a fact to propose against"}' | data_field id)
"${CLI[@]}" propose-edit --id "$EXPORT_PROP_TARGET" --old "to propose against" --new "already proposed" >/dev/null 2>&1
EXPORT_DIR="$WORK/export"
mkdir -p "$EXPORT_DIR"
"${CLI[@]}" export --format json --output "$EXPORT_DIR" >/dev/null 2>&1
check_contains "export carries expires_at" "$(cat "$EXPORT_DIR/export.json")" "expires_at"
check_contains "export carries proposes_supersede_of" "$(cat "$EXPORT_DIR/export.json")" "propose"
IMPORT_DB="$WORK/import.db"
IMPORT_OUT=$("$CLI_BIN" --db "$IMPORT_DB" import --format json --input "$EXPORT_DIR" 2>&1)
check_contains "import accepts the directory export wrote to" "$IMPORT_OUT" "Imported"
IMPORTED=$("$CLI_BIN" --db "$IMPORT_DB" list --limit 100 2>&1)
check_contains "an imported expiry survives the round trip" "$IMPORTED" "[expires"

# ══════════════════════════════════════════════════════════════════════════
section "Summary"
# ══════════════════════════════════════════════════════════════════════════

printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"
if [ "$FAIL" -gt 0 ]; then
    printf '\n  \033[31mfailed checks:\033[0m\n'
    for n in "${FAILED_NAMES[@]}"; do printf '    - %s\n' "$n"; done
    exit 1
fi
printf '  \033[32mall end-to-end release checks passed\033[0m\n'
