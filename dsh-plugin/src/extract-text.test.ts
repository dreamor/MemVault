import type { Message, UserMessage } from '@deepseek-ai/dsh-llm'
import { describe, expect, it } from 'vitest'
import { extractMessageText } from './extract-text.js'

function message(content: Message['content']): Message {
  return { content } as Message
}

function userMessage(content: UserMessage['content']): UserMessage {
  return { content, role: 'user' } as UserMessage
}

describe('extractMessageText', () => {
  it('joins text blocks in order', () => {
    const msg = message([
      { type: 'text', text: 'part one' },
      { type: 'text', text: 'part two' },
    ])
    expect(extractMessageText(msg)).toBe('part one\npart two')
  })

  it('skips reasoning, tool-call, and tool-result blocks', () => {
    const msg = message([
      { type: 'reasoning', text: 'thinking...' },
      { type: 'tool-call', id: 'call_1' as never, name: 'x', arguments: '{}' },
      { type: 'text', text: 'the actual answer' },
    ])
    expect(extractMessageText(msg)).toBe('the actual answer')
  })

  it('returns an empty string for content with no text blocks', () => {
    expect(extractMessageText(message([{ type: 'reasoning', text: 'only thinking' }]))).toBe('')
  })

  it('works on a user message just as well as an assistant message', () => {
    const msg = userMessage([{ type: 'text', text: '我偏好用 tabs 缩进' }])
    expect(extractMessageText(msg)).toBe('我偏好用 tabs 缩进')
  })
})
