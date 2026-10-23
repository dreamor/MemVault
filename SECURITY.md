# Security Policy

MemVault takes the security of user data seriously. The local-first design
means sensitive information stays on your machine by default, but misuse or
toolchain vulnerabilities can still introduce risk.

## Supported Versions

| Version | Supported |
|---------|-----------|
| `master` branch | ✅ Patched |
| Latest 3 release tags | ✅ Patched |
| Older versions | ❌ No patches |

## Reporting a Vulnerability

**Do not** report security vulnerabilities through public issues,
discussions, or pull requests.

Please use one of these private channels instead:

- **GitHub Security Advisories** (recommended): go to the repository's
  Security tab and select "Report a vulnerability"

Please include in your report:

- A description of the vulnerability and its impact
- Reproduction steps / a proof of concept
- Affected versions
- Your name / contact info (optional, for credit)

We commit to acknowledging reports within **48 hours** and providing a fix
timeline within **7 days**.

## Known Security Considerations

- **Local data**: `~/.memvault/data.db` stores every memory entry. New data
  directories are created `0700` and the database (plus its `-wal`/`-shm`
  siblings and the markdown cache) is tightened to `0600` after opening, so a
  fresh install is owner-only. This is best-effort and unix-only, and a
  directory that already existed keeps its mode — on an older install you may
  still want to `chmod 700 ~/.memvault` and `chmod 600 ~/.memvault/data.db`.
- **REST/MCP authentication**: with no agent `api_key` configured anywhere,
  the local API is unauthenticated by design (loopback-bound). As soon as any
  agent key exists, admin routes (`/api/memories`, `/api/import`, `/api/forget`,
  `/api/backup`, …) require a registered, keyed agent — an unregistered
  `X-MemVault-Agent-Id` is rejected rather than falling through to
  "unauthenticated mode". Set `MEMVAULT_IMPORT_ROOTS` to an explicit path list
  if you want to bound where `/api/agents/import/*` may read from.
- **Embedding calls**: setting `OPENAI_API_KEY` triggers outbound requests
  for semantic search; unset it to fall back to keyword-only search.
- **MCP stdio**: traffic between the CLI and MCP server is plaintext,
  intended for local inter-process communication only — do not forward it
  over a public network.
- **Prompt injection**: memories are surfaced to agents as MUST/REFERENCE
  instructions; always audit the `source` field (user vs. other agents) for
  where a given memory originated. By default, a MUST memory is only
  injected as a binding instruction when human-reviewed or human-authored —
  an unreviewed, AI-extracted MUST is demoted to inert reference data. In a
  shared multi-agent setup, set `agents.yaml` `api_key` per agent so writes
  carry a verified identity (`Memory.identity_verified`), and opt into
  `MEMVAULT_CORROBORATION_GATE=on` to require a MUST be independently
  corroborated by multiple distinct verified agents before it's trusted
  without human review — this narrows (does not eliminate) the risk of a
  single compromised/spoofed agent unilaterally poisoning the shared MUST
  pool that every local agent reads from.

## Acknowledgments

Researchers who responsibly disclose vulnerabilities will be credited in
`CHANGELOG.md` and this file (with their consent).
