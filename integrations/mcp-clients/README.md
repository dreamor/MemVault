# MCP client snippets

Each `*.json` here is a minimal, strictly-JSON registration for `memvault-mcp`
(one per host — agents distinguish themselves via `MEMVAULT_AGENT_ID`, sharing
one memory store). Merge the object at the path below:

| Host | Snippet | Merge into | Rules file (optional) |
|---|---|---|---|
| Cursor | `cursor.json` | `.cursor/mcp.json` (project) or global MCP config | `.cursor/rules/memvault.mdc` ← `integrations/rules/cursor.mdc` |
| Windsurf | `windsurf.json` | `~/.codeium/windsurf/mcp_config.json` | `.windsurf/rules/memvault.md` ← snippet |
| Cline / Roo Code | `cline.json` | `cline_mcp_settings.json` (VS Code) | `.clinerules/memvault.md` ← snippet |
| Continue | `continue.json` | `~/.continue/config.yaml` (as `mcpServers:`) | — |
| Zed | `zed.json` | `settings.json` (`context_servers`) | reads `AGENTS.md` natively |
| JetBrains AI Assistant | `jetbrains.json` | Settings ▸ Tools ▸ AI Assistant ▸ MCP | Junie: `.junie/guidelines.md` ← snippet |
| VS Code (Copilot Chat) | `vscode.json` | `.vscode/mcp.json` | `.github/copilot-instructions.md` ← snippet |
| Claude Desktop | `claude-desktop.json` | `claude_desktop_config.json` (absolute path recommended) | — |

Snippets are strict JSON (no comments) so CI lints them as-is; the target
paths live in this table. Full capability matrix: `docs/AGENT-PORTABILITY.md`.
