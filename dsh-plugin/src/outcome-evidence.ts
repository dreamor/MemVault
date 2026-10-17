/**
 * Mirrors the wire shape of `memvault_core::outcome_inference::OutcomeEvidence`
 * (`OutcomeEvidenceParams` in both crates/memvault-mcp/src/server.rs and
 * crates/memvault-proxy/src/handler.rs) — `camelCase` here maps to
 * `snake_case` field names when sent as the tool call's `evidence` argument.
 */
export interface OutcomeEvidence {
  task?: string
  retryHits: number
  rejectionHits: number
  correctionHits: number
  terminal: 'unresolved_tool_error' | 'unresolved_correction' | 'resolved'
  terminalDetail?: string
}

// Mirrors crates/memvault-core/src/friction.rs's REJECTION_PHRASES /
// CORRECTION_KEYWORDS. Deliberately duplicated rather than shared — no
// build artifact crosses the Rust/TS boundary here — so update both sides
// together if the signal wording changes.
const REJECTION_PHRASES = [
  "doesn't want to proceed",
  "don't want to proceed",
  'tool call was rejected',
  'tool use was rejected',
  'user rejected this',
  'user denied',
]

const CORRECTION_KEYWORDS = [
  'no, ',
  'not what i',
  "that's wrong",
  "that's not right",
  'stop, ',
  'stop.',
  'wait, ',
  'actually, ',
  'revert',
  'undo that',
  'roll that back',
  '不对',
  '不是这样',
  '不是这个意思',
  '停一下',
  '等一下',
  '撤销',
  '改回去',
  '别这样',
]

function containsAny(haystack: string, needles: string[]): boolean {
  const lower = haystack.toLowerCase()
  return needles.some(needle => lower.includes(needle.toLowerCase()))
}

function truncate(s: string, maxChars: number): string {
  return s.length <= maxChars ? s : `${s.slice(0, maxChars)}…`
}

/**
 * Accumulates the same three friction signals plus a terminal-state
 * classification as the Claude Code path (`friction::score` +
 * `outcome_inference`'s terminal-state scan, Rust side) — but incrementally
 * from dsh's own `session/event` stream instead of re-parsing a
 * Claude-Code-shaped JSONL transcript, which dsh never produces. Feed every
 * `tool/call`, `tool/result`, `user/message` (`source.kind === 'user'` only
 * — see index.ts's existing filter for synthetic/plugin-injected text), and
 * `assistant/message` event as it arrives; read `snapshot()`/`judgeContext()`
 * once, at `session/disposed`.
 */
export function createOutcomeEvidenceTracker() {
  const toolNames = new Map<string, string>()
  const errorStreaks = new Map<string, number>()
  let retryHits = 0
  let rejectionHits = 0
  let correctionHits = 0
  let humanTurnsSeen = 0
  let taskText: string | undefined
  let terminal: OutcomeEvidence['terminal'] = 'resolved'
  let terminalDetail: string | undefined
  let lastAssistantText = ''
  let lastUserText = ''

  function onToolCall(callId: string, name: string): void {
    toolNames.set(callId, name)
  }

  function onToolResult(callId: string, isError: boolean, text: string): void {
    const name = toolNames.get(callId) ?? callId
    const rejected = containsAny(text, REJECTION_PHRASES)
    const bad = isError || rejected
    if (isError) {
      const streak = (errorStreaks.get(name) ?? 0) + 1
      errorStreaks.set(name, streak)
      // A single error does not count — a retry success resets the streak,
      // exactly like friction.rs's `success_resets_the_error_streak`.
      if (streak >= 2) retryHits += 1
    } else {
      errorStreaks.set(name, 0)
    }
    if (rejected) rejectionHits += 1
    terminal = bad ? 'unresolved_tool_error' : 'resolved'
    terminalDetail = bad ? `${name}: ${truncate(text, 160)}` : undefined
  }

  function onUserMessage(text: string): void {
    humanTurnsSeen += 1
    if (taskText === undefined && text.trim()) taskText = text.trim()
    lastUserText = text
    // Not-the-opening-prompt rule, same as friction.rs: a correction keyword
    // in the very first human turn is the task itself, not a correction.
    if (humanTurnsSeen > 1 && containsAny(text, CORRECTION_KEYWORDS)) {
      correctionHits += 1
      terminal = 'unresolved_correction'
      terminalDetail = truncate(text, 160)
    } else {
      terminal = 'resolved'
      terminalDetail = undefined
    }
  }

  function onAssistantMessage(text: string): void {
    lastAssistantText = text
    terminal = 'resolved'
    terminalDetail = undefined
  }

  function snapshot(): OutcomeEvidence {
    return { task: taskText, retryHits, rejectionHits, correctionHits, terminal, terminalDetail }
  }

  /** Bounded free-text report for the optional LLM judge path (DATA, not instructions). */
  function judgeContext(): string {
    const lines = [
      taskText ? `user: ${taskText}` : undefined,
      lastUserText && lastUserText !== taskText ? `user: ${lastUserText}` : undefined,
      lastAssistantText ? `assistant: ${lastAssistantText}` : undefined,
    ].filter((line): line is string => Boolean(line))
    return lines.join('\n')
  }

  return { onToolCall, onToolResult, onUserMessage, onAssistantMessage, snapshot, judgeContext }
}
