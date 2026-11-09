import { describe, it, expect } from 'vitest';
import {
  decideAction,
  detectOrphans,
  buildIdIndex,
  slugify,
  fileNameFor,
  folderFor,
  memoryStatus,
  statusBanner,
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
    ai_generated: false,
    confidence: 0.9,
    access_count: 2,
    decay_score: 0.8,
    created_at: '2026-08-01T09:00:00Z',
    updated_at: '2026-08-14T09:00:00Z',
    occurred_at: null,
    expires_at: null,
    archived_at: null,
    source_agent: 'test-agent',
    visibility: 'scoped',
    superseded_by: null,
    proposes_supersede_of: null,
    skill_meta: null,
    friction_evidence: null,
    ...overrides,
  };
}

/**
 * An index entry as the *current* plugin writes it — `status` present and
 * matching a plain active memory, so the timestamp comparison is what a test
 * using this is actually exercising. Entries without `status` are a separate
 * case (pre-upgrade notes) and are built explicitly where they are the subject.
 */
function entry(overrides: Partial<FrontmatterIndexEntry> = {}): FrontmatterIndexEntry {
  return {
    path: 'MemVault/a.md',
    memvaultId: 'mem_abc12345',
    updatedAt: '2026-08-14T09:00:00Z',
    status: 'active',
    ...overrides,
  };
}

/** Fixed clock, so expiry tests do not depend on when the suite runs. */
const NOW = Date.parse('2026-09-01T00:00:00Z');

describe('decideAction', () => {
  it('creates when no existing note is indexed', () => {
    expect(decideAction(makeMemory(), undefined, NOW)).toBe('create');
  });

  it('updates when the remote memory is newer than the local note', () => {
    expect(
      decideAction(makeMemory({ updated_at: '2026-08-14T09:00:00Z' }), entry({
        updatedAt: '2026-08-01T00:00:00Z',
      }), NOW),
    ).toBe('update');
  });

  it('skips when the local note is already current', () => {
    expect(decideAction(makeMemory(), entry(), NOW)).toBe('skip');
  });

  it('treats an unparseable local timestamp as stale (forces update)', () => {
    expect(decideAction(makeMemory(), entry({ updatedAt: '' }), NOW)).toBe('update');
  });

  it('forces an update on an unparseable remote timestamp instead of skipping forever', () => {
    expect(decideAction(makeMemory({ updated_at: 'not-a-date' }), entry(), NOW)).toBe('update');
  });

  // A memory can leave `active` without `updated_at` moving: expiry is a
  // comparison against the clock, and archive/supersede are written by some
  // other memory's approval. `updated_at` alone never notices any of them.
  it('updates when the memory expired after the note was written', () => {
    const mem = makeMemory({ expires_at: '2026-08-20T00:00:00Z' });
    expect(memoryStatus(mem, NOW)).toBe('expired');
    expect(decideAction(mem, entry({ status: 'active' }), NOW)).toBe('update');
  });

  it('updates when the memory was superseded without its own timestamp moving', () => {
    const mem = makeMemory({ superseded_by: 'mem_newer' });
    expect(decideAction(mem, entry({ status: 'active' }), NOW)).toBe('update');
  });

  it('updates when the memory was archived without its own timestamp moving', () => {
    const mem = makeMemory({ archived_at: '2026-08-25T00:00:00Z' });
    expect(decideAction(mem, entry({ status: 'active' }), NOW)).toBe('update');
  });

  it('skips once the note already records the non-active status', () => {
    const mem = makeMemory({ superseded_by: 'mem_newer' });
    expect(decideAction(mem, entry({ status: 'superseded' }), NOW)).toBe('skip');
  });

  it('rewrites a pre-status note once, then converges', () => {
    // Notes written before `status` existed carry none. Defaulting the missing
    // value to 'active' instead would leave every one of them permanently
    // claiming to be live, which is the exact gap this closes.
    const legacy: FrontmatterIndexEntry = {
      path: 'MemVault/a.md',
      memvaultId: 'mem_abc12345',
      updatedAt: '2026-08-14T09:00:00Z',
    };
    expect(decideAction(makeMemory(), legacy, NOW)).toBe('update');
    // ...and after that sync the note has the key, so it settles.
    expect(decideAction(makeMemory(), entry(), NOW)).toBe('skip');
  });
});

describe('memoryStatus', () => {
  it('is active for an ordinary memory', () => {
    expect(memoryStatus(makeMemory(), NOW)).toBe('active');
  });

  it('is active while expires_at is still in the future', () => {
    expect(memoryStatus(makeMemory({ expires_at: '2026-12-01T00:00:00Z' }), NOW)).toBe('active');
  });

  it('is expired once expires_at has passed', () => {
    expect(memoryStatus(makeMemory({ expires_at: '2026-08-20T00:00:00Z' }), NOW)).toBe('expired');
  });

  it('is archived when archived_at is set', () => {
    expect(
      memoryStatus(makeMemory({ archived_at: '2026-08-25T00:00:00Z' }), NOW),
    ).toBe('archived');
  });

  it('is superseded when superseded_by is set', () => {
    expect(memoryStatus(makeMemory({ superseded_by: 'mem_newer' }), NOW)).toBe('superseded');
  });

  it('is proposal when proposes_supersede_of is set', () => {
    expect(
      memoryStatus(makeMemory({ proposes_supersede_of: 'mem_target' }), NOW),
    ).toBe('proposal');
  });

  // Precedence is deliberate and worth pinning: supersession archives its
  // target, so a superseded memory usually carries both, and `superseded` is
  // the more informative of the two. A proposal outranks everything because it
  // is the only state whose resolution touches a *different* memory.
  it('reports superseded rather than archived when both are set', () => {
    expect(
      memoryStatus(
        makeMemory({ superseded_by: 'mem_newer', archived_at: '2026-08-25T00:00:00Z' }),
        NOW,
      ),
    ).toBe('superseded');
  });

  it('reports proposal over every other state', () => {
    expect(
      memoryStatus(
        makeMemory({
          proposes_supersede_of: 'mem_target',
          superseded_by: 'mem_newer',
          archived_at: '2026-08-25T00:00:00Z',
          expires_at: '2026-08-20T00:00:00Z',
        }),
        NOW,
      ),
    ).toBe('proposal');
  });

  it('reports archived rather than expired when both apply', () => {
    expect(
      memoryStatus(
        makeMemory({ archived_at: '2026-08-25T00:00:00Z', expires_at: '2026-08-20T00:00:00Z' }),
        NOW,
      ),
    ).toBe('archived');
  });

  it('treats an unparseable expires_at as not-expired', () => {
    // Guessing "expired" from corrupt data would hide a live memory, which is
    // the worse of the two errors.
    expect(memoryStatus(makeMemory({ expires_at: 'not-a-date' }), NOW)).toBe('active');
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

describe('folderFor', () => {
  it('maps episode to 10-Daily', () => {
    expect(folderFor(makeMemory({ memory_type: 'episode' }))).toBe('10-Daily');
  });
  it('maps entity to 20-Entities', () => {
    expect(folderFor(makeMemory({ memory_type: 'entity' }))).toBe('20-Entities');
  });
  it('maps skill to 40-Skills', () => {
    expect(folderFor(makeMemory({ memory_type: 'skill' }))).toBe('40-Skills');
  });
  it('maps fact/preference to 30-Memories', () => {
    expect(folderFor(makeMemory({ memory_type: 'fact' }))).toBe('30-Memories');
    expect(folderFor(makeMemory({ memory_type: 'preference' }))).toBe('30-Memories');
  });
  it('is case-insensitive', () => {
    expect(folderFor(makeMemory({ memory_type: 'Episode' }))).toBe('10-Daily');
  });
});

describe('buildFrontmatter / buildNoteContent', () => {
  it('includes the memvault_id as the authoritative match key', () => {
    const fm = buildFrontmatter(makeMemory(), NOW);
    expect(fm).toContain('memvault_id: mem_abc12345');
    expect(fm).toContain('memvault_updated_at: 2026-08-14T09:00:00Z');
  });

  it('always carries a status, so a note is never silently unlabelled', () => {
    expect(buildFrontmatter(makeMemory(), NOW)).toContain('status: active');
  });

  it('omits status timestamps that are not set', () => {
    // These are `Option`s on the Rust side and null is the common case, so an
    // active memory's properties stay as short as they were before status
    // existed. `expires_at: null` on every note would be noise, and YAML null
    // is indistinguishable from the string "null" once Obsidian parses it.
    const fm = buildFrontmatter(makeMemory(), NOW);
    for (const key of ['occurred_at', 'expires_at', 'archived_at', 'superseded_by', 'proposes_supersede_of']) {
      expect(fm, `${key} must be absent when unset`).not.toContain(`${key}:`);
    }
  });

  it('emits each status timestamp that is set', () => {
    const fm = buildFrontmatter(
      makeMemory({
        occurred_at: '2026-07-01T00:00:00Z',
        expires_at: '2026-08-20T00:00:00Z',
        archived_at: '2026-08-25T00:00:00Z',
        superseded_by: 'mem_newer',
      }),
      NOW,
    );
    expect(fm).toContain('occurred_at: 2026-07-01T00:00:00Z');
    expect(fm).toContain('expires_at: 2026-08-20T00:00:00Z');
    expect(fm).toContain('archived_at: 2026-08-25T00:00:00Z');
    expect(fm).toContain('superseded_by: mem_newer');
    expect(fm).toContain('status: superseded');
  });

  it('keeps the raw timestamps alongside the derived status', () => {
    // `status` is a snapshot taken at sync time; a memory that expires later
    // still reads `active` until the next sync. Emitting `expires_at` too lets
    // a Dataview query compute liveness live instead of trusting the snapshot.
    const fm = buildFrontmatter(makeMemory({ expires_at: '2026-12-01T00:00:00Z' }), NOW);
    expect(fm).toContain('status: active');
    expect(fm).toContain('expires_at: 2026-12-01T00:00:00Z');
  });

  it('appends an Instruction section only when present', () => {
    const withInstruction = buildNoteContent(
      makeMemory({ instruction: 'Always use type hints' }),
      NOW,
    );
    expect(withInstruction).toContain('## Instruction\nAlways use type hints');

    const without = buildNoteContent(makeMemory({ instruction: null }), NOW);
    expect(without).not.toContain('## Instruction');
  });
});

describe('statusBanner', () => {
  it('is absent for an active memory', () => {
    // The common case should not carry decoration, and a banner on every note
    // would train the reader to ignore the ones that matter.
    expect(statusBanner(makeMemory(), NOW)).toBeNull();
    expect(buildNoteContent(makeMemory(), NOW)).not.toContain('[!');
  });

  it('warns for an expired memory', () => {
    const banner = statusBanner(makeMemory({ expires_at: '2026-08-20T00:00:00Z' }), NOW);
    expect(banner).toContain('[!warning] Expired');
    expect(banner).toContain('2026-08-20T00:00:00Z');
  });

  it('warns for a superseded memory and names the replacement', () => {
    const banner = statusBanner(makeMemory({ superseded_by: 'mem_newer' }), NOW);
    expect(banner).toContain('[!warning] Superseded');
    expect(banner).toContain('mem_newer');
  });

  it('notes an archived memory', () => {
    expect(statusBanner(makeMemory({ archived_at: '2026-08-25T00:00:00Z' }), NOW)).toContain(
      '[!info] Archived',
    );
  });

  it('flags a pending proposal and its consequence', () => {
    // Approving a proposal archives its target — the one side effect a reviewer
    // cannot infer from the proposal's own content.
    const banner = statusBanner(makeMemory({ proposes_supersede_of: 'mem_target' }), NOW);
    expect(banner).toContain('[!todo] Pending proposal');
    expect(banner).toContain('mem_target');
    expect(banner).toContain('archives that memory');
  });

  it('places the banner before the content, where reading mode shows it', () => {
    // Frontmatter alone does not carry the signal: Obsidian collapses
    // properties by default and hides them entirely in reading mode.
    const note = buildNoteContent(makeMemory({ superseded_by: 'mem_newer' }), NOW);
    const bannerAt = note.indexOf('[!warning] Superseded');
    const contentAt = note.indexOf('User prefers Python');
    expect(bannerAt).toBeGreaterThan(-1);
    expect(contentAt).toBeGreaterThan(-1);
    expect(bannerAt).toBeLessThan(contentAt);
  });

  it('survives alongside an Instruction section', () => {
    const note = buildNoteContent(
      makeMemory({ expires_at: '2026-08-20T00:00:00Z', instruction: 'Use type hints' }),
      NOW,
    );
    expect(note).toContain('[!warning] Expired');
    expect(note).toContain('## Instruction\nUse type hints');
  });
});
