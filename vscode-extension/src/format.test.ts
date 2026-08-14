import { describe, it, expect } from 'vitest';
import {
  Memory,
  priorityIcon,
  treeItemLabel,
  treeItemDescription,
  treeItemTooltipLines,
  formatMemoryDetail,
} from './format';

function makeMemory(overrides: Partial<Memory> = {}): Memory {
  return {
    id: 'mem_abc123',
    memory_type: 'preference',
    content: 'User prefers Python for coding',
    instruction: null,
    priority: 'MUST',
    namespace: 'global',
    tags: ['coding', 'python'],
    layer: 'L3',
    skill_meta: null,
    access_count: 3,
    human_reviewed: true,
    created_at: '2026-08-01T00:00:00Z',
    updated_at: '2026-08-14T09:00:00Z',
    ...overrides,
  };
}

describe('priorityIcon', () => {
  it('maps MUST/REFERENCE/BACKGROUND to distinct icons', () => {
    expect(priorityIcon('MUST')).toBe('🔴');
    expect(priorityIcon('REFERENCE')).toBe('🔵');
    expect(priorityIcon('BACKGROUND')).toBe('⚪');
  });
});

describe('treeItemLabel', () => {
  it('prefixes the content with the priority icon and truncates', () => {
    const label = treeItemLabel(makeMemory({ content: 'a'.repeat(100) }), 10);
    expect(label).toBe(`🔴 ${'a'.repeat(10)}`);
  });
});

describe('treeItemDescription', () => {
  it('shows layer and type', () => {
    expect(treeItemDescription(makeMemory())).toBe('[L3] preference');
  });
});

describe('treeItemTooltipLines', () => {
  it('includes core fields and omits instruction/skill sections when absent', () => {
    const lines = treeItemTooltipLines(makeMemory());
    expect(lines.some((l) => l.startsWith('ID: mem_abc123'))).toBe(true);
    expect(lines.some((l) => l.startsWith('Instruction'))).toBe(false);
    expect(lines.some((l) => l.startsWith('Skill trigger'))).toBe(false);
  });

  it('includes instruction and skill meta when present', () => {
    const lines = treeItemTooltipLines(
      makeMemory({
        instruction: 'Use type hints',
        skill_meta: { trigger: 'deploy', steps: ['build', 'test'], verification: 'health ok', version: 1 },
      }),
    );
    expect(lines.some((l) => l === 'Instruction: Use type hints')).toBe(true);
    expect(lines.some((l) => l === 'Skill trigger: deploy')).toBe(true);
    expect(lines.some((l) => l === 'Skill steps: build → test')).toBe(true);
    expect(lines.some((l) => l === 'Verification: health ok')).toBe(true);
  });
});

describe('formatMemoryDetail', () => {
  it('renders a YAML-ish detail document with content', () => {
    const text = formatMemoryDetail(makeMemory());
    expect(text).toContain('# Memory: mem_abc123');
    expect(text).toContain('priority: MUST');
    expect(text).toContain('## Content\nUser prefers Python for coding');
  });

  it('omits the Skill Meta section when skill_meta is null', () => {
    const text = formatMemoryDetail(makeMemory());
    expect(text).not.toContain('## Skill Meta');
  });

  it('renders the Skill Meta section with numbered steps when present', () => {
    const text = formatMemoryDetail(
      makeMemory({
        skill_meta: { trigger: 'deploy', steps: ['build', 'test'], verification: 'health ok', version: 2 },
      }),
    );
    expect(text).toContain('## Skill Meta');
    expect(text).toContain('trigger: deploy');
    expect(text).toContain('  1. build');
    expect(text).toContain('  2. test');
    expect(text).toContain('version: 2');
  });
});
