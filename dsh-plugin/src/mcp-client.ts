import { Client } from '@modelcontextprotocol/sdk/client/index.js'
import { StreamableHTTPClientTransport } from '@modelcontextprotocol/sdk/client/streamableHttp.js'
import type { Context } from '@deepseek-ai/cordis'
import type { OutcomeEvidence } from './outcome-evidence.js'

/** Result of {@link MemVaultClient.sessionStart}. */
export interface SessionStartResult {
  /** Parsed from the `[inject_session_id: inj_xxx]` prefix; empty string if parsing failed. */
  injectSessionId: string
  /** Full formatted MUST/REF instructions text (including the prefix). */
  text: string
}

/**
 * Thin MCP client wrapper around a running `memvault-proxy` instance.
 * Talks standard MCP-over-HTTP (rmcp's streamable-http transport on the Rust
 * side) — no private protocol. See crates/memvault-proxy/src/handler.rs for
 * the tool/resource definitions this wraps.
 */
export interface MemVaultClient {
  sessionStart(agentId: string, contextHint?: string, project?: string): Promise<SessionStartResult>
  /**
   * @param userText - optional: the user's own turn text. Passing it
   * substantially improves extraction recall — see `NotifyResponseParams`'s
   * doc comment in crates/memvault-proxy/src/handler.rs.
   */
  notifyResponse(responseText: string, agentId: string, userText?: string): Promise<string>
  readSessionInject(): Promise<string>
  /**
   * Classify and record a task outcome from host-computed evidence (see
   * {@link OutcomeEvidence}) rather than a raw transcript — dsh has no
   * Claude-Code-shaped JSONL transcript to hand over. Wraps the proxy's
   * `infer_outcome` tool (crates/memvault-proxy/src/handler.rs); returns the
   * tool's plain-text summary, including a "No outcome inferred" message
   * when the evidence was too thin to record anything (not an error).
   */
  inferOutcome(evidence: OutcomeEvidence, judgeContext: string, agentId: string): Promise<string>
}

const INJECT_SESSION_ID_PREFIX = /^\[inject_session_id: (inj_[0-9a-f]+)\]/

/**
 * `callTool`'s return type is a union that also covers a `toolResult`-only
 * shape (structured-content-only responses); MemVault's tools always return
 * `content` text blocks, so treat anything else as empty rather than widen
 * the type further.
 */
function textOf(result: unknown): string {
  const content = (result as { content?: unknown } | undefined)?.content
  if (!Array.isArray(content)) return ''
  return content
    .filter((block): block is { type: string; text: string } =>
      typeof block === 'object' && block !== null
      && (block as { type?: unknown }).type === 'text'
      && typeof (block as { text?: unknown }).text === 'string')
    .map(block => block.text)
    .join('\n')
}

/**
 * Create a MemVault MCP client and wire its connection lifecycle to `ctx`'s
 * current fiber via `ctx.effect()` — disposed automatically when the plugin
 * unloads or hot-reloads.
 * @param ctx - the plugin context (used only for `ctx.effect()`).
 * @param url - the memvault-proxy MCP endpoint, e.g. `http://127.0.0.1:3778/mcp`.
 */
export function createMcpClient(ctx: Context, url: string): MemVaultClient {
  const client = new Client({ name: 'memvault-dsh-memvault', version: '0.1.0' })
  let connected: Promise<void> | undefined

  const ensureConnected = (): Promise<void> => {
    // `connected` memoizes an in-flight or successful connection, but must
    // NOT memoize a failed one — a rejected promise cached here would poison
    // every future call for the process's lifetime (confirmed against a real
    // dsh run: the very first call raced memvault-proxy's own startup and
    // lost, and every later call kept re-rejecting the same cached failure
    // even after the proxy came up).
    connected ??= (async () => {
      const transport = new StreamableHTTPClientTransport(new URL(url))
      await client.connect(transport)
    })().catch((error: unknown) => {
      connected = undefined
      throw error
    })
    return connected
  }

  ctx.effect(() => {
    return async () => {
      if (!connected) return
      await connected.catch(() => {})
      await client.close()
    }
  }, 'memvault.mcp-client')

  return {
    async sessionStart(agentId, contextHint, project) {
      await ensureConnected()
      const result = await client.callTool({
        name: 'session_start',
        arguments: { agent_id: agentId, context_hint: contextHint, project },
      })
      const text = textOf(result)
      const injectSessionId = INJECT_SESSION_ID_PREFIX.exec(text)?.[1] ?? ''
      return { injectSessionId, text }
    },

    async notifyResponse(responseText, agentId, userText) {
      await ensureConnected()
      const result = await client.callTool({
        name: 'notify_response',
        arguments: {
          response_text: responseText,
          agent_id: agentId,
          ...(userText ? { user_text: userText } : {}),
        },
      })
      return textOf(result)
    },

    async readSessionInject() {
      await ensureConnected()
      const result = await client.readResource({ uri: 'memory://session-inject' })
      const contents = result.contents as Array<{ text?: string }> | undefined
      return (contents ?? []).map(entry => entry.text ?? '').join('\n')
    },

    async inferOutcome(evidence, judgeContext, agentId) {
      await ensureConnected()
      const result = await client.callTool({
        name: 'infer_outcome',
        arguments: {
          evidence: {
            task: evidence.task,
            retry_hits: evidence.retryHits,
            rejection_hits: evidence.rejectionHits,
            correction_hits: evidence.correctionHits,
            terminal: evidence.terminal,
            terminal_detail: evidence.terminalDetail,
          },
          judge_context: judgeContext,
          agent_id: agentId,
        },
      })
      return textOf(result)
    },
  }
}
