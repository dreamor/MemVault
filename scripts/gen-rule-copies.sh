#!/bin/sh
# Regenerate the instruction-tier rule copies from the canonical text.
#
# The canonical body lives in plugins/memvault/rules/memvault.md between
# memvault:canonical-begin/end markers. Every host copy reuses that exact
# body (verbatim markers included) so scripts/check-rule-parity.sh can hold
# them together. Idempotent: re-running overwrites copies deterministically.
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
CANONICAL="$ROOT/plugins/memvault/rules/memvault.md"
OUT="$ROOT/integrations/rules"

[ -f "$CANONICAL" ] || { echo "missing canonical: $CANONICAL" >&2; exit 1; }

extract() { sed -n '/memvault:canonical-begin/,/memvault:canonical-end/p' "$CANONICAL"; }
BODY=$(extract)
[ -n "$BODY" ] || { echo "canonical markers not found in $CANONICAL" >&2; exit 1; }

# write_copy <file> <header-lines> <footer-lines>
write_copy() {
    {
        printf '%b' "$2"
        printf '%s\n' "$BODY"
        printf '%b' "$3"
    } >"$OUT/$1"
}

write_copy cursor.mdc \
    "---\ndescription: MemVault shared memory rules\ngroups: []\nalwaysApply: true\n---\n\n" \
    "\n"

write_copy clinerules.md "### MemVault (auto-generated block — edit memories, not this file)\n\n" ""
write_copy kiro-steering.md "" "\n"
write_copy agents-snippet.md \
    "<!-- Paste this block into AGENTS.md / CLAUDE.md / junie guidelines for\n     hosts with no hooks and no MCP registration. -->\n\n" "\n"

# Qoder reads .qoder/rules/ from the repo root (T3); ship a bare copy there.
mkdir -p "$ROOT/.qoder/rules"
{ printf '%s\n' "$BODY"; printf '\n'; } >"$ROOT/.qoder/rules/memvault.md"

# Batch-3 skill hosts read a repo-root skills/ (Swival `skills add`, generic
# skill-capable agents) or .openclaw/skills/ (OpenClaw); both are byte-copies
# of the plugin skills dir, kept in sync and parity-checked in CI.
mkdir -p "$ROOT/skills" "$ROOT/.openclaw/skills"
rm -rf "$ROOT"/skills/memvault-* "$ROOT"/.openclaw/skills/memvault-*
for skill_dir in "$ROOT"/plugins/memvault/skills/memvault-*; do
    cp -R "$skill_dir" "$ROOT/skills/"
    cp -R "$skill_dir" "$ROOT/.openclaw/skills/"
done

printf 'regenerated rule copies in %s (plus .qoder/rules/, skills/, .openclaw/skills/)\n' "$OUT"
