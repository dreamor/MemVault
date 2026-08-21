import { describe, it, expect } from 'vitest';
import {
  decideAction,
  detectOrphans,
  buildIdIndex,
  slugify,
  fileNameFor,
  buildFrontmatter,
  buildNoteContent,
  RemoteMemory,
  FrontmatterIndexEntry,
} from './sync';

function makeMemory(overrides: Partial<RemoteMemory> = {}): RemoteMemory {
  return {
    id: 'mem_abc12345',
    content: 'User prefers Python for coding',
    instruction: null,
    priority: 'MUST',
    memory_type: 'preference',
    namespace: 'global',
    tags: ['coding', 'python'],
    layer: 'L3',
    human_reviewed: true,
    updated_at: '2026-08-14T09:00:00Z',
    ...overrides,
  };
}

describe('decideAction', () => {
  it('creates when no existing note is indexed', () => {
    expect(decideAction(makeMemory(), undefined)).toBe('create');
  });

  it('updates when the remote memory is newer than the local note', () => {
    const existing: FrontmatterIndexEntry = {
      path: 'MemVault/a.md',
      memvaultId: 'mem_abc12345',
      updatedAt: '2026-08-01T00:00:00Z',
    };
    expect(decideAction(makeMemory({ updated_at: '2026-08-14T09:00:00Z' }), existing)).toBe(
      'update',
    );
  });

  it('skips when the local note is already current', () => {
    const existing: FrontmatterIndexEntry = {
      path: 'MemVault/a.md',
      memvaultId: 'mem_abc12345',
      updatedAt: '2026-08-14T09:00:00Z',
    };
    expect(decideAction(makeMemory({ updated_at: '2026-08-14T09:00:00Z' }), existing)).toBe(
      'skip',
    );
  });

  it('treats an unparseable local timestamp as stale (forces update)', () => {
    const existing: FrontmatterIndexEntry = {
      path: 'MemVault/a.md',
      memvaultId: 'mem_abc12345',
      updatedAt: '',
    };
    expect(decideAction(makeMemory(), existing)).toBe('update');
  });

  it('forces an update on an unparseable remote timestamp instead of skipping forever', () => {
    const existing: FrontmatterIndexEntry = {
      path: 'MemVault/a.md',
      memvaultId: 'mem_abc12345',
      updatedAt: '2026-08-14T09:00:00Z',
    };
    expect(decideAction(makeMemory({ updated_at: 'not-a-date' }), existing)).toBe('update');
  });
});

describe('detectOrphans', () => {
  it('returns local entries whose id no longer exists remotely', () => {
    const entries: FrontmatterIndexEntry[] = [
      { path: 'MemVault/a.md', memvaultId: 'mem_1', updatedAt: '' },
      { path: 'MemVault/b.md', memvaultId: 'mem_2', updatedAt: '' },
    ];
    const orphans = detectOrphans(entries, new Set(['mem_1']));
    expect(orphans).toHaveLength(1);
    expect(orphans[0].memvaultId).toBe('mem_2');
  });

  it('returns nothing when every local id still exists remotely', () => {
    const entries: FrontmatterIndexEntry[] = [
      { path: 'MemVault/a.md', memvaultId: 'mem_1', updatedAt: '' },
    ];
    expect(detectOrphans(entries, new Set(['mem_1']))).toHaveLength(0);
  });
});

describe('buildIdIndex', () => {
  it('indexes entries by memvault id', () => {
    const entries: FrontmatterIndexEntry[] = [
      { path: 'MemVault/a.md', memvaultId: 'mem_1', updatedAt: 't1' },
    ];
    const index = buildIdIndex(entries);
    expect(index.get('mem_1')?.path).toBe('MemVault/a.md');
    expect(index.get('mem_missing')).toBeUndefined();
  });
});

describe('slugify', () => {
  it('lowercases and hyphenates', () => {
    expect(slugify('User Prefers Python')).toBe('user-prefers-python');
  });

  it('falls back to "memory" for content with no sluggable characters', () => {
    expect(slugify('!!!')).toBe('memory');
  });

  it('truncates to maxLen', () => {
    const long = 'a'.repeat(100);
    expect(slugify(long, 10)).toHaveLength(10);
  });
});

describe('fileNameFor', () => {
  it('combines a content slug with a short id suffix', () => {
    const name = fileNameFor(makeMemory({ id: 'mem_deadbeef1234' }));
    expect(name).toMatch(/^user-prefers-python-for-coding--deadbeef\.md$/);
  });
});

describe('buildFrontmatter / buildNoteContent', () => {
  it('includes the memvault_id as the authoritative match key', () => {
    const fm = buildFrontmatter(makeMemory());
    expect(fm).toContain('memvault_id: mem_abc12345');
    expect(fm).toContain('memvault_updated_at: 2026-08-14T09:00:00Z');
  });

  it('appends an Instruction section only when present', () => {
    const withInstruction = buildNoteContent(makeMemory({ instruction: 'Always use type hints' }));
    expect(withInstruction).toContain('## Instruction\nAlways use type hints');

    const without = buildNoteContent(makeMemory({ instruction: null }));
    expect(without).not.toContain('## Instruction');
  });
});
