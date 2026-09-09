# Agent Portability

MemVault is one memory service with many adapters. The shared core is the
`memvault-mcp` server (16 tools + 2 resources), the `memvault` CLI and the
REST API on port 3777; everything else here is a thin per-host adapter, in
the spirit of [Ponytail's portability layout](https://github.com/DietrichGebert/ponytail).

Capability tiers used below:

- **T1 — full native plugin**: one-command install, MCP bundled, hook-injected
  recall, optional end-of-session extraction (drafts → review inbox).
- **T2 — MCP registration**: paste the config from
  `integrations/mcp-clients/`; recall = MCP `session_start` tool / resources
  (`memory://user-profile`, `memory://project-context`) + a rules file.
- **T3 — native plugin, reduced capability**: hosts with plugin systems but no
  instruction-injecting hooks (planned second batch).
- **T4 — instruction-only**: copy a rule file; the agent is taught to call
  `session_start` and save back on its own.

## T1 · Full native plugins (first batch)

| Host | Install | Recall | Extraction | Adapter |
|---|---|---|---|---|
| Claude Code | `/plugin marketplace add dreamor/memvault` → `/plugin install memvault@memvault` | ✅ SessionStart hook (startup/resume/clear/compact) | ✅ Stop hook, opt-in `MEMVAULT_HOOK_EXTRACT=1` | [plugins/memvault/](../plugins/memvault/) |
| OpenCode | point `plugin` at `integrations/opencode/plugins/memvault.mjs` | ✅ system transform each session | ✅ `session.idle`, opt-in; JS reads messages via client | [integrations/opencode/](../integrations/opencode/) |
| Codex CLI | `config.toml` MCP + `memvault sync` AGENTS.md | ⚠️ agent-invoked (no instruction hooks) | ❌ no transcript hook | [integrations/codex/](../integrations/codex/) |
| Gemini CLI / Antigravity | `gemini extensions install https://github.com/dreamor/memvault` | ⚠️ contextFileName rules + `session_start` tool; Gemini has no injecting hooks | ❌ | [gemini-extension.json](../gemini-extension.json), [commands/](../commands/) |

## T2 · MCP registration (first batch)

Snippets live in [integrations/mcp-clients/](../integrations/mcp-clients/):
`cursor.json`, `windsurf.json`, `cline.json`, `continue.json`, `zed.json`,
`jetbrains.json`, `vscode.json`, `claude-desktop.json`. All register the same
stdio server with a per-agent `MEMVAULT_AGENT_ID` so engines share one store
without double-injecting (see Feature `InjectChannel` in the design docs).

Recall is agent-invoked; pair the registration with a rule copy (T4) so new
sessions know to call `session_start`.

## T3 · Native manifests, reduced capability

Qoder ships real surfaces in-repo: `.qoder/rules/memvault.md` (canonical
copy) + `.qoder-plugin/plugin.json` + `UserPromptSubmit` hook template
(`plugins/memvault/hooks/qoder-hooks.json` + `qoder-prompt.sh`, first-prompt
recall with per-session dedup). Grok Build: root `plugin.json` +
`.grok-plugin/marketplace.json` (`grok plugin install dreamor/memvault --trust`).
Batch-3 additions: Hermes Python plugin
([integrations/hermes/](../integrations/hermes/), `pre_llm_call` injection +
extract helper) and pi extension ([pi-extension/](../pi-extension/),
`pi install git:github.com/dreamor/memvault`) — both verify-on-install.
OpenClaw and Swival consume generated copies: repo-root `skills/` (Swival
`skills add`, generic skill hosts) and `.openclaw/skills/`, byte-synced from
the plugin skills dir by `scripts/gen-rule-copies.sh` and parity-checked in
CI. Devin remains a manual recipe in
[integrations/README.md](../integrations/README.md). MCP registry submission material:
[integrations/mcp-registry/](../integrations/mcp-registry/).

## T4 · Instruction-only rule copies

Canonical text: [plugins/memvault/rules/memvault.md](../plugins/memvault/rules/memvault.md).
Regenerate copies with `scripts/gen-rule-copies.sh`; CI enforces parity with
`scripts/check-rule-parity.sh`.

| Host | File (copy into the user's project) |
|---|---|
| Cursor | `.cursor/rules/memvault.mdc` — [integrations/rules/cursor.mdc](../integrations/rules/cursor.mdc) |
| Cline / Roo | `.clinerules/memvault.md` — [clinerules](../integrations/rules/clinerules.md) |
| Kiro | `.kiro/steering/memvault.md` — [kiro-steering](../integrations/rules/kiro-steering.md) |
| AGENTS.md-family (Codex IDE, Amp, Jules, Zed, CodeWhale, Junie, Copilot CLI fallback) | paste [agents-snippet](../integrations/rules/agents-snippet.md), or run `memvault sync` |

## Hook contract (all hosts)

Hooks never block the host session: any failure exits 0 quietly; diagnostics
go to `${TMPDIR:-/tmp}/memvault-hook.log` with `MEMVAULT_HOOK_DEBUG=1`. All
JSON handling lives in Rust (`memvault-core/src/{hook_envelope,transcript}.rs`)
or JS — shells only forward payloads. Environment knobs: `MEMVAULT_AGENT_ID`,
`MEMVAULT_HTTP_URL`, `MEMVAULT_HOOK_EXTRACT`, `MEMVAULT_BIN`.

## Verification status

- Claude Code hook scripts: covered by `plugins/memvault/tests/run-tests.sh`
  (CI job `agent-plugins`). Real marketplace install: run the two `/plugin`
  commands locally once before tagging a release.
- OpenCode/Codex/Gemini adapters: authored against current plugin docs;
  schemas move — re-verify the flagged fields in each host's README when it
  releases breaking changes.
