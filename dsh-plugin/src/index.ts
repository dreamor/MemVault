import type { Context } from '@deepseek-ai/cordis'
// Side-effect type imports: declaration-merge `ctx.systemPrompt` and the
// `session/event`/`turn/end`/etc. Events onto Context (same pattern as the
// real `@deepseek-ai/dsh-mcp-client` importing `@deepseek-ai/dsh-tools`).
import type {} from '@deepseek-ai/dsh-session'
import type {} from '@deepseek-ai/dsh-system-prompt'
import { Config } from './config.js'
import { extractMessageText } from './extract-text.js'
import { createMcpClient } from './mcp-client.js'
import { startProxy } from './process-manager.js'

export { Config }

/** Cordis plugin display name (loader diagnostics). */
export const name = 'memvault'

/** This plugin only needs the system-prompt registry to inject via `section()`. */
export const inject = ['systemPrompt']

/**
 * Order for the injected memory section: after the deployment persona
 * (order 0) so MUST rules read like part of the persona, before tool
 * guidance (order 100+). See @deepseek-ai/dsh-system-prompt's `PromptSection`
 * doc comment for the ordering convention.
 */
const INJECTION_SECTION_ORDER = 1

/**
 * DeepSeek Harness (dsh) Cordis plugin bridging MemVault into the harness.
 *
 * Two independent features, each individually toggleable via config:
 * - `injectOnAssemble`: keeps a locally cached copy of `memory://session-inject`
 *   fresh and registers it as a system prompt section via
 *   `ctx.systemPrompt.section()`. The section's `text` callback must be
 *   synchronous (Cordis constraint), hence the cache — this mirrors
 *   `memvault-proxy`'s own `InjectionEngine` cache-refresh pattern on the
 *   Rust side. The cache is refreshed eagerly at plugin load (so turn 1
 *   already has content) and again on every `turn/end` (preparing the cache
 *   for the *next* turn, with the whole inter-turn gap to complete —
 *   refreshing reactively on `turn/start` instead was tried first and lost a
 *   real race: `system-prompt/assemble` runs immediately after `turn/start`
 *   in the same turn, before an async MCP round-trip can finish, so the
 *   section rendered empty and `renderPrompt()` silently drops empty
 *   sections — confirmed against a real dsh session log). The eager apply-time refresh must wait
 *   for `startProxy()`'s `ready` promise first (mode: spawn) — firing it
 *   immediately raced the just-spawned `memvault-proxy` process's own
 *   startup and lost too, and because `mcp-client.ts` used to memoize even a
 *   FAILED connection attempt, that one early loss silently broke every
 *   later call (every turn's refresh, every `notify_response`) for the rest
 *   of the process's life — also confirmed against a real run, also §4.1.
 * - `extractOnTurnEnd`: listens on `session/event`, buffers the latest
 *   `assistant/message` AND the user's own `user/message` text per turn, and
 *   calls `notify_response` with both once that turn's `turn/end` arrives
 *   with `reason.kind === 'completed'` (skips aborted/blocked/error/
 *   interrupted turns — there is nothing useful to extract from those).
 *   Passing the user's own text matters: MemVault's extractor signal words
 *   are first-person ("我偏好"/"我喜欢"), which match a user's own statement
 *   about themselves far more reliably than an assistant's restatement of
 *   it (confirmed on real runs).
 *   `user/message` events carry no `turn` field of their own (unlike
 *   `assistant/message`), so `currentTurn` tracks it from the most recent
 *   `turn/start`; only messages with `source.kind === 'user'` are genuine
 *   human input — plugin-injected synthetic context (sandbox policy notes,
 *   skill lists, file-change notices) also arrives as `user/message` with
 *   `source.kind: 'plugin'` and must be skipped.
 *
 */
export function apply(ctx: Context, config: Config): void {
  const spawned = config.mode === 'spawn' ? startProxy(ctx, config) : null
  const url = spawned ? spawned.url : config.url
  if (!url) {
    throw new Error('memvault: mode is "attach" but no url was configured')
  }
  const client = createMcpClient(ctx, url)
  const proxyReady = spawned ? spawned.ready : Promise.resolve()

  let cachedInjection = ''
  const refreshInjection = () => {
    proxyReady
      .then(() => client.readSessionInject())
      .then(text => { cachedInjection = text })
      .catch(error => ctx.logger.error(new Error('memvault: failed to refresh session-inject', { cause: error })))
  }

  if (config.injectOnAssemble) {
    ctx.systemPrompt.section({
      name: 'memvault:injection',
      order: INJECTION_SECTION_ORDER,
      text: () => cachedInjection,
    })
    refreshInjection()
  }

  if (config.extractOnTurnEnd || config.injectOnAssemble) {
    let currentTurn: number | undefined
    const pendingUserText = new Map<number, string>()
    const pendingAssistantText = new Map<number, string>()

    ctx.on('session/event', (session, event) => {
      if (event.type === 'turn/start') {
        currentTurn = event.data.turn
        return
      }
      if (event.type === 'user/message') {
        if (event.data.source.kind === 'user' && currentTurn !== undefined) {
          pendingUserText.set(currentTurn, extractMessageText(event.data))
        }
        return
      }
      if (event.type === 'assistant/message') {
        pendingAssistantText.set(event.data.turn, extractMessageText(event.data.message))
        return
      }
      if (event.type !== 'turn/end') return
      const assistantText = pendingAssistantText.get(event.data.turn)
      const userText = pendingUserText.get(event.data.turn)
      pendingAssistantText.delete(event.data.turn)
      pendingUserText.delete(event.data.turn)
      if (config.injectOnAssemble) refreshInjection()
      if (!config.extractOnTurnEnd || event.data.reason.kind !== 'completed') return
      if (!assistantText && !userText) return
      client.notifyResponse(assistantText ?? '', config.agentId, userText)
        .catch(error => ctx.logger.error(new Error('memvault: notify_response failed', { cause: error })))
    })
  }
}
