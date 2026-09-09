# MCP registry submissions (manual)

The `server.json` here is a starting draft — each channel has its own
requirements, so submissions are a human step. Checklist:

1. **Official MCP Registry** — PR against `modelcontextprotocol/registry`,
   adding the server entry under `servers/`. Align `server.json` with their
   current schema (`schema_version`, `name` reverse-DNS, package sources:
   crates.io for `memvault-mcp`, GitHub Releases for binaries). This draft
   marks the packages block as a placeholder on purpose.
2. **Smithery** — `smithery.yaml` config + docs submission; verify their
   current `startCommand` format for stdio servers (`memvault-mcp`).
3. **Aggregators with auto-crawl, no submission needed** — mcp.so, Glama,
   PulseMCP typically pick servers up from the registry/GitHub; claim the
   listing afterwards to attach install snippets.

Do not submit without `MEMVAULT_HTTP_URL`-less stdio default (that is the
correct public shape: `memvault-mcp --transport stdio`).
