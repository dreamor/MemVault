import z from '@deepseek-ai/schemastery'

/**
 * Plugin config. Mirrors the pattern used by `@deepseek-ai/dsh-mcp-client`
 * (packages/mcp/mcp-client/src/index.ts in the dsh repo): a Schemastery
 * schema cast to the hand-written interface, because Schemastery's own type
 * inference for object/union schemas isn't relied on elsewhere in the dsh
 * codebase either — every real plugin we read casts explicitly too.
 */
export interface Config {
  /**
   * `spawn`: this plugin starts and owns a `memvault-proxy` child process.
   * `attach`: connect to an already-running instance at `url`.
   */
  mode: 'spawn' | 'attach'
  /** Used when `mode: spawn` — path to the MemVault SQLite database. */
  db: string
  /** Used when `mode: spawn` — port `memvault-proxy --transport sse` listens on. */
  port: number
  /** Used when `mode: attach` — MCP endpoint, e.g. `http://127.0.0.1:3778/mcp`. */
  url: string
  /** Used when `mode: spawn` — path to the `memvault-proxy` executable; resolved from PATH if empty. */
  binaryPath: string
  /**
   * Used when `mode: spawn` — forced as `MEMVAULT_EMBEDDING_PROVIDER` in the
   * child's environment. Defaults to `native` (offline, no API key, uses the
   * bundled fastembed model) rather than leaving it unset: an unset value
   * lets `memvault-proxy` inherit `OPENAI_API_KEY`/`OPENAI_API_BASE` from
   * dsh's own process env (e.g. dsh's own model credentials) and silently
   * try to use them as MemVault's embedding key, which is a different
   * credential entirely and fails with 401. `none` disables embeddings
   * (keyword-only search); `ollama`/`openai`/`openai-compatible` opt back
   * into a remote provider if you actually want one.
   */
  embeddingProvider: string
  /** Agent id reported to MemVault for `session_start` / `notify_response` calls. */
  agentId: string
  /** Auto-inject memories into the system prompt via `ctx.systemPrompt.section()`. */
  injectOnAssemble: boolean
  /** Auto-call `notify_response` when a turn ends with `reason.kind === 'completed'`. */
  extractOnTurnEnd: boolean
}

export const Config = z.object({
  mode: z.union([z.const('spawn'), z.const('attach')]).default('spawn'),
  db: z.string().default('~/.memvault/data.db'),
  port: z.number().default(3778),
  url: z.string().default(''),
  binaryPath: z.string().default(''),
  embeddingProvider: z.string().default('native'),
  agentId: z.string().default('dsh'),
  injectOnAssemble: z.boolean().default(true),
  extractOnTurnEnd: z.boolean().default(true),
}) as unknown as z<Config>
