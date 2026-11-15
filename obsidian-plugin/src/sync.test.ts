import { describe, it, expect, vi } from 'vitest';
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
  RemoteSyncPage,
  readRemoteSnapshot,
  SYNC_PAGE_SIZE,
  SYNC_SNAPSHOT_ATTEMPTS,
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

function completeSnapshot(ids: string[]) {
  return {
    complete: true as const, records: ids.map((id) => makeMemory({ id })),
    totalCount: ids.length, membershipDigest: 'fixture-digest',
  };
}

function page(ids: string[], overrides: Partial<RemoteSyncPage> = {}): RemoteSyncPage {
  return {
    records: ids.map((id) => makeMemory({ id })), next_cursor: null, has_more: false,
    total_count: ids.length, membership_digest: 'fixture-digest', ...overrides,
  };
}

// Pure synthetic records only: no real server, database or Obsidian vault.
describe('readRemoteSnapshot', () => {
  it('exhausts more than 10,000 records with tied timestamps in deterministic ID order', async () => {
    const ids = Array.from({ length: 10001 }, (_, i) => `mem_${String(i).padStart(5, '0')}`);
    const calls: (string | undefined)[] = [];
    const snapshot = await readRemoteSnapshot(async (cursor) => {
      calls.push(cursor);
      const start = cursor ? ids.indexOf(cursor) + 1 : 0;
      const batch = ids.slice(start, start + SYNC_PAGE_SIZE);
      const hasMore = start + batch.length < ids.length;
      return page(batch, { total_count: ids.length, has_more: hasMore,
        next_cursor: hasMore ? batch[batch.length - 1] : null });
    });
    expect(snapshot.complete).toBe(true);
    expect(snapshot.records.map((m) => m.id)).toEqual(ids);
    expect(calls).toHaveLength(11);
    expect(calls[10]).toBe('mem_09999');
  });

  it.each([4, 1])('accepts UTF-8 ordered custom IDs with page size %s, including non-BMP cursors', async (pageSize) => {
    // UTF-8/BINARY puts E000 before non-BMP code points; UTF-16 puts it after.
    const ids = ['mem_a', 'mem_', 'mem_\u{1F600}', 'mem_\u{1F601}'];
    const calls: (string | undefined)[] = [];
    const snapshot = await readRemoteSnapshot(async (cursor) => {
      calls.push(cursor);
      const start = cursor === undefined ? 0 : ids.indexOf(cursor) + 1;
      const batch = ids.slice(start, start + pageSize);
      const more = start + batch.length < ids.length;
      return page(batch, { total_count: ids.length, has_more: more,
        next_cursor: more ? batch[batch.length - 1] : null });
    });
    expect(snapshot.complete).toBe(true);
    expect(snapshot.records.map((memory) => memory.id)).toEqual(ids);
    expect(calls).toEqual(pageSize === 4 ? [undefined] : [undefined, ...ids.slice(0, -1)]);
  });

  it.each([
    { ids: ['mem_b', 'mem_a'], pageSize: 2 },
    { ids: ['mem_b', 'mem_a'], pageSize: 1 },
    { ids: ['mem_\u{1F600}', 'mem_'], pageSize: 2 },
    { ids: ['mem_\u{1F600}', 'mem_'], pageSize: 1 },
  ])('still rejects descending binary ID order %#', async ({ ids, pageSize }) => {
    const snapshot = await readRemoteSnapshot(async (cursor) => {
      const start = cursor === undefined ? 0 : ids.indexOf(cursor) + 1;
      const batch = ids.slice(start, start + pageSize);
      const more = start + batch.length < ids.length;
      return page(batch, { total_count: ids.length, has_more: more,
        next_cursor: more ? batch[batch.length - 1] : null });
    });
    expect(snapshot).toMatchObject({ complete: false, reason: 'remote records are not in deterministic ID order' });
  });

  it('accepts equal and prefix UTF-8 IDs without losing deduplication', async () => {
    const ids = ['mem_\u{1F600}', 'mem_\u{1F600}', 'mem_\u{1F600}suffix'];
    const snapshot = await readRemoteSnapshot(async () => page(ids, { total_count: 2 }));
    expect(snapshot.complete).toBe(true);
    expect(snapshot.records.map((memory) => memory.id)).toEqual([ids[0], ids[2]]);
  });

  it.each(['total_count', 'membership_digest'] as const)('restarts on changed %s, discarding the earlier traversal', async (field) => {
    const fetchPage = vi.fn()
      .mockResolvedValueOnce(page(['mem_a'], { total_count: 2, has_more: true, next_cursor: 'mem_a' }))
      .mockResolvedValueOnce(page(['mem_b'], { total_count: 2, [field]: field === 'total_count' ? 3 : 'changed' }))
      .mockResolvedValueOnce(page(['mem_c']));
    const snapshot = await readRemoteSnapshot(fetchPage);
    expect(snapshot.complete).toBe(true);
    expect(snapshot.records.map((m) => m.id)).toEqual(['mem_c']);
    expect(fetchPage.mock.calls.map(([cursor]) => cursor)).toEqual([undefined, 'mem_a', undefined]);
  });

  it.each(['total_count', 'membership_digest'] as const)('bounds retries on repeated %s mutation', async (field) => {
    const fetchPage = vi.fn(async (cursor?: string) => cursor
      ? page(['mem_b'], { total_count: 2, [field]: field === 'total_count' ? 3 : 'changed' })
      : page(['mem_a'], { total_count: 2, has_more: true, next_cursor: 'mem_a' }));
    const snapshot = await readRemoteSnapshot(fetchPage);
    expect(snapshot.complete).toBe(false);
    expect(fetchPage).toHaveBeenCalledTimes(SYNC_SNAPSHOT_ATTEMPTS * 2);
  });

  it('rejects overlapping pages instead of treating deduplicated row count as exclusive keyset evidence', async () => {
    const fetchPage = vi.fn()
      .mockResolvedValueOnce(page(['mem_a'], { total_count: 2, has_more: true, next_cursor: 'opaque-first' }))
      .mockResolvedValueOnce(page(['mem_a', 'mem_b'], { total_count: 2 }));
    const snapshot = await readRemoteSnapshot(fetchPage);
    expect(snapshot).toMatchObject({ complete: false, reason: 'remote records are not in deterministic ID order' });
    expect(fetchPage).toHaveBeenCalledTimes(2);
  });

  it('follows non-lexicographic opaque tokens independently of record ID ordering', async () => {
    const tokens = [`v1:${'f'.repeat(64)}`, `v1:${'a'.repeat(64)}`];
    const fetchPage = vi.fn()
      .mockResolvedValueOnce(page(['mem_a'], { total_count: 3, has_more: true, next_cursor: tokens[0] }))
      .mockResolvedValueOnce(page(['mem_b'], { total_count: 3, has_more: true, next_cursor: tokens[1] }))
      .mockResolvedValueOnce(page(['mem_c'], { total_count: 3 }));
    const snapshot = await readRemoteSnapshot(fetchPage);
    expect(snapshot.complete).toBe(true);
    expect(snapshot.records.map((memory) => memory.id)).toEqual(['mem_a', 'mem_b', 'mem_c']);
    expect(fetchPage.mock.calls.map(([cursor]) => cursor)).toEqual([undefined, ...tokens]);
  });

  it.each([
    '', '\u0000', '\u0001', '\n', '\r', '\u001f', '\u007f', '\u0080', '\u0085', '\u009f',
    'opaque\ncursor', 'a'.repeat(1025), 'é'.repeat(513), '\u{1F600}'.repeat(257),
  ])(
    'rejects an invalid returned cursor by UTF-8 transport bounds %#', async (cursor) => {
      const fetchPage = vi.fn(async () => page(['mem_a'], {
        total_count: 2, has_more: true, next_cursor: cursor,
      }));
      const snapshot = await readRemoteSnapshot(fetchPage);
      expect(snapshot).toMatchObject({ complete: false, reason: 'invalid remote membership evidence' });
      expect(fetchPage).toHaveBeenCalledTimes(1);
    },
  );

  it.each(['a'.repeat(1024), 'é'.repeat(512), '\u{1F600}'.repeat(256)])('accepts an opaque cursor at the exact 1024-byte limit %#', async (cursor) => {
    const fetchPage = vi.fn()
      .mockResolvedValueOnce(page(['mem_a'], { total_count: 2, has_more: true, next_cursor: cursor }))
      .mockResolvedValueOnce(page(['mem_b'], { total_count: 2 }));
    expect((await readRemoteSnapshot(fetchPage)).complete).toBe(true);
    expect(fetchPage.mock.calls[1]).toEqual([cursor]);
  });

  it.each([
    ' ', '~', String.fromCharCode(0xa0),
    `opaque_é_漢_\u{1F600}_${String.fromCharCode(0x2028, 0x2029)}`, 'opaque_\ud800_\udc00',
  ])('preserves non-control Unicode in opaque cursors without normalization %#', async (cursor) => {
    const fetchPage = vi.fn()
      .mockResolvedValueOnce(page(['mem_a'], { total_count: 2, has_more: true, next_cursor: cursor }))
      .mockResolvedValueOnce(page(['mem_b'], { total_count: 2 }));
    expect((await readRemoteSnapshot(fetchPage)).complete).toBe(true);
    expect(fetchPage.mock.calls[1]).toEqual([cursor]);
  });

  it('accepts empty, control-bearing and long record IDs in UTF-8 order within a page', async () => {
    const ids = ['', 'mem_\u0001', `mem_${'a'.repeat(2048)}`, 'mem_', 'mem_\u{1F600}'];
    const snapshot = await readRemoteSnapshot(async () => page(ids));
    expect(snapshot.complete).toBe(true);
    expect(snapshot.records.map((memory) => memory.id)).toEqual(ids);
  });

  it('uses an empty last record ID as a real anchor when the next page advances', async () => {
    const fetchPage = vi.fn()
      .mockResolvedValueOnce(page([''], { total_count: 2, has_more: true, next_cursor: 'opaque-empty-anchor' }))
      .mockResolvedValueOnce(page(['mem_a'], { total_count: 2 }));
    const snapshot = await readRemoteSnapshot(fetchPage);
    expect(snapshot.complete).toBe(true);
    expect(snapshot.records.map((memory) => memory.id)).toEqual(['', 'mem_a']);
  });

  it('rejects an equal empty ID on the next page instead of losing the last-record sentinel', async () => {
    const fetchPage = vi.fn()
      .mockResolvedValueOnce(page([''], { total_count: 2, has_more: true, next_cursor: 'opaque-empty-anchor' }))
      .mockResolvedValueOnce(page(['', 'mem_a'], { total_count: 2 }));
    expect(await readRemoteSnapshot(fetchPage)).toMatchObject({
      complete: false, reason: 'remote records are not in deterministic ID order',
    });
    expect(fetchPage).toHaveBeenCalledTimes(2);
  });

  it('accepts an evidenced empty store', async () => {
    expect(await readRemoteSnapshot(async () => page([]))).toMatchObject({ complete: true, totalCount: 0 });
  });

  it('bounds retries when terminal unique count disagrees with the total', async () => {
    const fetchPage = vi.fn(async () => page(['mem_a', 'mem_a'], { total_count: 2 }));
    expect((await readRemoteSnapshot(fetchPage)).complete).toBe(false);
    expect(fetchPage).toHaveBeenCalledTimes(SYNC_SNAPSHOT_ATTEMPTS);
  });

  it('does not complete after a repeated cursor', async () => {
    const fetchPage = vi.fn()
      .mockResolvedValueOnce(page(['mem_a'], { total_count: 3, has_more: true, next_cursor: 'same' }))
      .mockResolvedValueOnce(page(['mem_b'], { total_count: 3, has_more: true, next_cursor: 'same' }));
    expect((await readRemoteSnapshot(fetchPage)).complete).toBe(false);
    expect(fetchPage).toHaveBeenCalledTimes(2);
  });

  it('does not complete when fresh cursors repeat only already seen IDs', async () => {
    const fetchPage = vi.fn()
      .mockResolvedValueOnce(page(['mem_a'], { total_count: 3, has_more: true, next_cursor: 'first' }))
      .mockResolvedValueOnce(page(['mem_a'], { total_count: 3, has_more: true, next_cursor: 'second' }));
    expect((await readRemoteSnapshot(fetchPage)).complete).toBe(false);
    expect(fetchPage).toHaveBeenCalledTimes(2);
  });

  it('preserves partial records but not completeness after a failed page', async () => {
    const fetchPage = vi.fn()
      .mockResolvedValueOnce(page(['mem_a'], { total_count: 2, has_more: true, next_cursor: 'mem_a' }))
      .mockRejectedValueOnce(new Error('offline'));
    const snapshot = await readRemoteSnapshot(fetchPage);
    expect(snapshot.complete).toBe(false);
    expect(snapshot.records.map((m) => m.id)).toEqual(['mem_a']);
  });

  it.each([
    { membership_digest: '' }, { membership_digest: undefined }, { total_count: -1 },
    { total_count: 1.5 }, { next_cursor: '' }, { next_cursor: 'unexpected' },
    { has_more: true, next_cursor: null }, { records: [makeMemory({ id: undefined as any })] },
    { records: [makeMemory({ id: 'mem_b' }), makeMemory({ id: 'mem_a' })], total_count: 2 },
    { records: Array.from({ length: 1001 }, () => makeMemory()), total_count: 1001 },
  ])('refuses incomplete or malformed evidence %#', async (overrides) => {
    const snapshot = await readRemoteSnapshot(async () => page(['mem_a'], overrides as Partial<RemoteSyncPage>));
    expect(snapshot.complete).toBe(false);
  });

  it.each([{ records: [] }, { records: [makeMemory()] }])('does not infer completeness from a legacy array %#', async ({ records }) => {
    const snapshot = await readRemoteSnapshot(async () => records);
    expect(snapshot.complete).toBe(false);
    expect(snapshot.records).toEqual(records);
  });
});

describe('detectOrphans', () => {
  it('never nominates orphans from an incomplete remote snapshot', () => {
    const entries = [entry()];
    expect(detectOrphans(entries, { complete: false, records: [], reason: 'no evidence' })).toEqual([]);
  });

  it('returns local entries whose id no longer exists remotely', () => {
    const entries: FrontmatterIndexEntry[] = [
      { path: 'MemVault/a.md', memvaultId: 'mem_1', updatedAt: '' },
      { path: 'MemVault/b.md', memvaultId: 'mem_2', updatedAt: '' },
    ];
    const orphans = detectOrphans(entries, completeSnapshot(['mem_1']));
    expect(orphans).toHaveLength(1);
    expect(orphans[0].memvaultId).toBe('mem_2');
  });

  it('returns nothing when every local id still exists remotely', () => {
    const entries: FrontmatterIndexEntry[] = [
      { path: 'MemVault/a.md', memvaultId: 'mem_1', updatedAt: '' },
    ];
    expect(detectOrphans(entries, completeSnapshot(['mem_1']))).toHaveLength(0);
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

describe('read-only fact protocol compatibility', () => {
  it('keeps the legacy note and frontmatter shape when fact fields are absent or null', () => {
    const legacy = makeMemory();
    expect(legacy).not.toHaveProperty('fact_assertion');
    expect(legacy).not.toHaveProperty('fact_management');
    expect(legacy).not.toHaveProperty('identity_verified');
    const withNulls = makeMemory({ fact_assertion: null, fact_management: null });
    expect(buildNoteContent(withNulls, NOW)).toBe(buildNoteContent(legacy, NOW));
    expect(buildFrontmatter(legacy, NOW)).toBe([
      '---',
      'memvault_id: mem_abc12345',
      'memvault_updated_at: 2026-08-14T09:00:00Z',
      'status: active',
      'priority: MUST',
      'type: preference',
      'layer: L3',
      'tags: ["coding", "python"]',
      'namespace: global',
      'human_reviewed: true',
      '---',
    ].join('\n'));
  });

  it.each([true, false])(
    'retains identity_verified=%s only as a server read report, not a note property',
    (identityVerified) => {
      const legacy = makeMemory();
      const memory = makeMemory({ identity_verified: identityVerified });
      expect(buildNoteContent(memory, NOW)).toBe(buildNoteContent(legacy, NOW));
      expect(memory.identity_verified).toBe(identityVerified);
    },
  );

  it.each(['single', 'multi', 'event', 'unknown'] as const)(
    'accepts %s assertions without synthesizing note properties',
    (cardinality) => {
      const legacy = makeMemory();
      const withAssertion = makeMemory({
        fact_assertion: {
          key: 'coding.language',
          value: 'Python',
          source_namespace: 'project:example',
          source_order: 7,
          cardinality,
        },
      });
      expect(buildNoteContent(withAssertion, NOW)).toBe(buildNoteContent(legacy, NOW));
      expect(withAssertion.fact_assertion?.cardinality).toBe(cardinality);
    },
  );

  it.each(['active', 'suspended'] as const)(
    'does not claim fact resolution or change %s management when rendering edited text',
    (state) => {
      const legacy = makeMemory({ content: 'User now prefers Go for coding' });
      const memory = makeMemory({
        content: legacy.content,
        fact_assertion: {
          key: 'coding.language',
          value: 'Python',
          source_namespace: 'project:example',
          source_order: 7,
          cardinality: 'single',
        },
        fact_management: {
          record_id: legacy.id,
          producer_id: 'verified-producer',
          namespace: 'global',
          source_namespace: 'project:example',
          key: 'coding.language',
          source_order: 7,
          state,
        },
      });
      const before = JSON.stringify(memory);
      // This is only a Markdown projection, not a writer or a resolver. The
      // existing liveness status is not a claim of managed-fact resolution.
      expect(buildNoteContent(memory, NOW)).toBe(buildNoteContent(legacy, NOW));
      expect(JSON.stringify(memory)).toBe(before);
      expect(memory.fact_management?.state).toBe(state);
    },
  );
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
