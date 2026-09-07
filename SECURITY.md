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

- **Local data**: `~/.memvault/data.db` stores every memory entry. The
  process does not force-tighten file permissions — the file lands with
  whatever your umask produces (typically `0644`). For stricter isolation,
  run `chmod 600 ~/.memvault/data.db` and `chmod 700` on `~/.memvault/`.
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
