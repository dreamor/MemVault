import type { Message } from '@deepseek-ai/dsh-llm'

/**
 * Join the visible text blocks of a message (assistant or user).
 * `Message.content` is `ContentBlock[]` (@deepseek-ai/dsh-llm/lib/types/types.d.ts)
 * — a discriminated union of `text | reasoning | image | tool-call | tool-result`
 * blocks; only `text` blocks are user-visible, so reasoning and other block
 * types are intentionally skipped. `AssistantMessage`/`UserMessage` both
 * extend `Message` with the same `content` shape, so this one function
 * covers both.
 */
export function extractMessageText(message: Message): string {
  return message.content
    .filter((block): block is { type: 'text'; text: string } => block.type === 'text')
    .map(block => block.text)
    .join('\n')
    .trim()
}
