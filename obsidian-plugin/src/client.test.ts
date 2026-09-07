import { describe, it, expect, vi, beforeEach } from 'vitest';

const { requestUrl, Notice, TFile } = vi.hoisted(() => ({
  requestUrl: vi.fn(),
  Notice: vi.fn(),
  TFile: class TFile {},
}));

vi.mock('obsidian', () => ({
  App: class {},
  Plugin: class {},
  PluginSettingTab: class {},
  Setting: class {
    // Minimal builder chain used by settings/modal code when instantiated.
    setName() { return this; }
    setDesc() { return this; }
    addText() { return this; }
    addTextArea() { return this; }
    addDropdown() { return this; }
    addToggle() { return this; }
    addButton() { return this; }
  },
  ItemView: class {},
  WorkspaceLeaf: class {},
  Notice,
  Modal: class {},
  SuggestModal: class {},
  FuzzySuggestModal: class {},
  MarkdownView: class {},
  TFile,
  requestUrl,
}));

import MemVaultPlugin from './main';
import { RemoteMemory } from './sync';

function makePlugin() {
  const plugin = Object.create(MemVaultPlugin.prototype) as MemVaultPlugin;
  plugin.settings = {
    serverUrl: 'http://127.0.0.1:8080',
    refreshInterval: 10,
    apiKey: 'k3y',
    syncFolder: 'MemVault',
    syncDeleteOrphans: false,
  };
  return plugin;
}

function envelope(data: unknown) {
  return { json: { ok: true, data } };
}

function remoteMemory(overrides: Partial<RemoteMemory> = {}): RemoteMemory {
  return {
    id: `mem_${Math.random().toString(16).slice(2, 10)}`,
    content: 'User prefers Python',
    instruction: null,
    priority: 'MUST',
    memory_type: 'preference',
    namespace: 'global',
    tags: ['lang'],
    layer: 'L3',
    human_reviewed: true,
    updated_at: '2026-08-20T00:00:00Z',
    ...overrides,
  };
}

beforeEach(() => {
  requestUrl.mockReset();
  Notice.mockReset();
});

describe('api() — REST envelope handling', () => {
  it('unwraps {ok:true,data} and returns the payload', async () => {
    requestUrl.mockResolvedValue(envelope({ id: 'mem_x' }));
    const plugin = makePlugin();
    const data = await plugin.api('GET', '/api/memories');
    expect(data).toEqual({ id: 'mem_x' });
    const [opts] = requestUrl.mock.calls[0];
    expect(opts.url).toBe('http://127.0.0.1:8080/api/memories');
    expect(opts.method).toBe('GET');
  });

  it('throws a readable error when the envelope reports ok:false', async () => {
    requestUrl.mockResolvedValue({ json: { ok: false, error: 'forbidden' } });
    const plugin = makePlugin();
    await expect(plugin.api('GET', '/x')).rejects.toThrow('forbidden');
  });

  it('passes through responses that are not envelopes', async () => {
    requestUrl.mockResolvedValue({ json: { status: 'ok' } });
    const plugin = makePlugin();
    expect(await plugin.api('GET', '/health')).toEqual({ status: 'ok' });
  });

  it('sets JSON body and content-type when body is present', async () => {
    requestUrl.mockResolvedValue(envelope({}));
    const plugin = makePlugin();
    await plugin.api('POST', '/api/memories', { content: 'x' });
    const [opts] = requestUrl.mock.calls[0];
    expect(opts.body).toBe(JSON.stringify({ content: 'x' }));
    expect(opts.headers['Content-Type']).toBe('application/json');
    expect(opts.headers['X-MemVault-Api-Key']).toBe('k3y');
  });

  it('omits the api key header when not configured', async () => {
    requestUrl.mockResolvedValue(envelope({}));
    const plugin = makePlugin();
    plugin.settings.apiKey = '';
    await plugin.api('GET', '/x');
    expect(requestUrl.mock.calls[0][0].headers).toBeUndefined();
  });
});

describe('REST client methods — request shapes', () => {
  it('listMemories GETs /api/memories with limit', async () => {
    requestUrl.mockResolvedValue(envelope([remoteMemory()]));
    const plugin = makePlugin();
    const list = await plugin.listMemories(25);
    expect(list).toHaveLength(1);
    expect(requestUrl.mock.calls[0][0].url).toContain('/api/memories?limit=25');
  });

  it('searchMemories POSTs query and top_k', async () => {
    requestUrl.mockResolvedValue(envelope([{ memory: remoteMemory(), score: 1 }]));
    const plugin = makePlugin();
    const results = await plugin.searchMemories('python', 5);
    expect(results[0].score).toBe(1);
    const [opts] = requestUrl.mock.calls[0];
    expect(opts.url).toContain('/api/search');
    expect(JSON.parse(opts.body)).toEqual({ query: 'python', top_k: 5 });
  });

  it('saveMemory POSTs the full payload and notifies with the id', async () => {
    requestUrl.mockResolvedValue(envelope({ id: 'mem_1' }));
    const plugin = makePlugin();
    await plugin.saveMemory('prefers Go', 'MUST', 'preference');
    const [opts] = requestUrl.mock.calls[0];
    expect(opts.url).toContain('/api/memories');
    const body = JSON.parse(opts.body);
    expect(body.content).toBe('prefers Go');
    expect(body.priority).toBe('MUST');
    expect(body.type).toBe('preference');
    expect(body.agent_id).toBe('obsidian');
    expect(Notice).toHaveBeenCalledWith('Saved: mem_1');
  });

  it('getStats GETs /api/stats and returns the dashboard aggregate', async () => {
    const stats = {
      total: 10,
      must_count: 3,
      reference_count: 5,
      reviewed_count: 7,
      agents: ['obsidian'],
      namespaces: ['global'],
      layers: { l0: 1, l1: 2, l2: 3, l3: 4 },
      skills: 2,
    };
    requestUrl.mockResolvedValue(envelope(stats));
    const plugin = makePlugin();
    const result = await plugin.getStats();
    expect(result).toEqual(stats);
    const [opts] = requestUrl.mock.calls[0];
    expect(opts.method).toBe('GET');
    expect(opts.url).toContain('/api/stats');
  });

  it('getInbox unwraps {memories,total}', async () => {
    requestUrl.mockResolvedValue(envelope({ memories: [remoteMemory()], total: 1 }));
    const plugin = makePlugin();
    const inbox = await plugin.getInbox();
    expect(inbox).toHaveLength(1);
  });

  it('approve/reject/delete hit the right endpoints', async () => {
    requestUrl.mockResolvedValue(envelope({}));
    const plugin = makePlugin();
    await plugin.approveMemory('mem_1');
    expect(requestUrl.mock.calls[0][0]).toMatchObject({ method: 'POST', url: expect.stringContaining('/api/inbox/mem_1/approve') });
    await plugin.rejectMemory('mem_1');
    expect(requestUrl.mock.calls[1][0]).toMatchObject({ method: 'POST', url: expect.stringContaining('/api/inbox/mem_1/reject') });
    await plugin.deleteMemory('mem_1');
    expect(requestUrl.mock.calls[2][0]).toMatchObject({ method: 'DELETE', url: expect.stringContaining('/api/memories/mem_1') });
  });

  it('createMemoryFull POSTs type (not memory_type) with agent context', async () => {
    requestUrl.mockResolvedValue(envelope({}));
    const plugin = makePlugin();
    await plugin.createMemoryFull({
      content: 'c',
      instruction: 'i',
      priority: 'REFERENCE',
      memoryType: 'fact',
      namespace: 'global',
      tags: ['a', 'b'],
    });
    const body = JSON.parse(requestUrl.mock.calls[0][0].body);
    expect(body.type).toBe('fact');
    expect(body.memory_type).toBeUndefined();
    expect(body.tags).toEqual(['a', 'b']);
    expect(body.instruction).toBe('i');
  });

  it('updateMemory PUTs a patch', async () => {
    requestUrl.mockResolvedValue(envelope({}));
    const plugin = makePlugin();
    await plugin.updateMemory('mem_1', {
      content: 'new',
      instruction: '',
      priority: 'MUST',
      memoryType: 'skill',
      namespace: 'n',
      tags: [],
    });
    const [opts] = requestUrl.mock.calls[0];
    expect(opts.method).toBe('PUT');
    expect(opts.url).toContain('/api/memories/mem_1');
    const body = JSON.parse(opts.body);
    expect(body.instruction).toBeNull();
    expect(body.type).toBe('skill');
  });

  it('supersedeMemory POSTs replacement_id to /api/memories/:id/supersede', async () => {
    requestUrl.mockResolvedValue(envelope({ superseded: 'mem_1', replacement_id: 'mem_2' }));
    const plugin = makePlugin();
    await plugin.supersedeMemory('mem_1', 'mem_2');
    const [opts] = requestUrl.mock.calls[0];
    expect(opts.method).toBe('POST');
    expect(opts.url).toContain('/api/memories/mem_1/supersede');
    expect(JSON.parse(opts.body)).toEqual({ replacement_id: 'mem_2' });
  });

  it('quickEditInbox POSTs edited_content to /api/inbox/:id/edit', async () => {
    requestUrl.mockResolvedValue(envelope({ edited: 'mem_1' }));
    const plugin = makePlugin();
    await plugin.quickEditInbox('mem_1', 'edited now');
    const [opts] = requestUrl.mock.calls[0];
    expect(opts.method).toBe('POST');
    expect(opts.url).toContain('/api/inbox/mem_1/edit');
    expect(JSON.parse(opts.body)).toEqual({ edited_content: 'edited now' });
  });

  it('extractMemories POSTs text/mode with auto_save false and returns candidates', async () => {
    const result = {
      memories: [{ content: 'User prefers tabs', instruction: null, type: 'preference', priority: 'REFERENCE', tags: ['style'], confidence: 0.7 }],
      coverage: { input_lines: 3, empty_lines: 0, extracted_lines: 1, no_signal_lines: 2 },
      saved_ids: [],
    };
    requestUrl.mockResolvedValue(envelope(result));
    const plugin = makePlugin();
    const got = await plugin.extractMemories('User prefers tabs over spaces', 'rule');
    expect(got).toEqual(result);
    const [opts] = requestUrl.mock.calls[0];
    expect(opts.method).toBe('POST');
    expect(opts.url).toContain('/api/extract');
    expect(JSON.parse(opts.body)).toEqual({ text: 'User prefers tabs over spaces', mode: 'rule', auto_save: false });
  });

  it('runDedup/runDecay/runPromote POST to maintenance endpoints', async () => {
    requestUrl.mockResolvedValue(envelope({ unique_count: 2, duplicate_count: 1 }));
    const plugin = makePlugin();
    const dedup = await plugin.runDedup();
    expect(dedup).toEqual({ unique_count: 2, duplicate_count: 1 });
    expect(requestUrl.mock.calls[0][0].url).toContain('/api/dedup');

    requestUrl.mockResolvedValue(envelope({ updated: 1, archived: 0 }));
    const decay = await plugin.runDecay();
    expect(decay).toEqual({ updated: 1, archived: 0 });
    expect(requestUrl.mock.calls[1][0].url).toContain('/api/decay');

    requestUrl.mockResolvedValue(envelope({ promoted_to_l2: 1, promoted_to_l3: 0 }));
    const promote = await plugin.runPromote();
    expect(promote).toEqual({ promoted_to_l2: 1, promoted_to_l3: 0 });
    expect(requestUrl.mock.calls[2][0].url).toContain('/api/promote');
  });
});

function fakeDataApp() {
  const vault = {
    getAbstractFileByPath: vi.fn(() => null),
    createFolder: vi.fn(async () => {}),
    create: vi.fn(async () => {}),
    modify: vi.fn(async () => {}),
    createBinary: vi.fn(async () => {}),
    modifyBinary: vi.fn(async () => {}),
  };
  return { app: { vault } as any, vault };
}

describe('exportVault / importFromJson / importFromMarkdown / backupVault / checkpoints', () => {
  it('exportVault writes a json export under <syncFolder>/_exports', async () => {
    requestUrl.mockResolvedValue(envelope({ format: 'json', content: '{"memories":[]}' }));
    const plugin = makePlugin();
    const { app, vault } = fakeDataApp();
    plugin.app = app;

    const result = await plugin.exportVault('json');

    expect(vault.createFolder).toHaveBeenCalledWith('MemVault/_exports');
    expect(vault.create).toHaveBeenCalledWith('MemVault/_exports/export-json-all.json', '{"memories":[]}');
    expect(result.savedPaths).toEqual(['MemVault/_exports/export-json-all.json']);
    expect(requestUrl.mock.calls[0][0].url).toContain('/api/export?format=json');
  });

  it('exportVault writes one file per markdown export entry', async () => {
    requestUrl.mockResolvedValue(envelope({ format: 'markdown', files: [{ filename: 'mem_1.md', content: '---\nid: mem_1\n---\nbody' }] }));
    const plugin = makePlugin();
    const { app, vault } = fakeDataApp();
    plugin.app = app;

    const result = await plugin.exportVault('markdown', 'project:x');

    expect(vault.create).toHaveBeenCalledWith('MemVault/_exports/mem_1.md', '---\nid: mem_1\n---\nbody');
    expect(result.savedPaths).toEqual(['MemVault/_exports/mem_1.md']);
    expect(requestUrl.mock.calls[0][0].url).toContain('namespace=project%3Ax');
  });

  it('importFromJson POSTs format/content', async () => {
    requestUrl.mockResolvedValue(envelope({ imported: 1 }));
    const plugin = makePlugin();
    const result = await plugin.importFromJson('{"memories":[]}');
    expect(result).toEqual({ imported: 1 });
    const [opts] = requestUrl.mock.calls[0];
    expect(JSON.parse(opts.body)).toEqual({ format: 'json', content: '{"memories":[]}' });
  });

  it('importFromMarkdown POSTs format/files and surfaces skipped entries', async () => {
    requestUrl.mockResolvedValue(envelope({ imported: 0, skipped: [{ filename: 'a.md', reason: 'bad frontmatter' }] }));
    const plugin = makePlugin();
    const result = await plugin.importFromMarkdown([{ filename: 'a.md', content: 'x' }]);
    expect(result.skipped).toHaveLength(1);
    const [opts] = requestUrl.mock.calls[0];
    expect(JSON.parse(opts.body)).toEqual({ format: 'markdown', files: [{ filename: 'a.md', content: 'x' }] });
  });

  it('backupVault downloads the sqlite file into <syncFolder>/_backups using the response filename', async () => {
    const buffer = new TextEncoder().encode('SQLite format 3\0FAKE').buffer;
    requestUrl.mockResolvedValue({ arrayBuffer: buffer, headers: { 'content-disposition': 'attachment; filename="memvault-backup-x.db"' } });
    const plugin = makePlugin();
    const { app, vault } = fakeDataApp();
    plugin.app = app;

    const path = await plugin.backupVault();

    expect(path).toBe('MemVault/_backups/memvault-backup-x.db');
    expect(vault.createFolder).toHaveBeenCalledWith('MemVault/_backups');
    expect(vault.createBinary).toHaveBeenCalledWith('MemVault/_backups/memvault-backup-x.db', buffer);
  });

  it('listCheckpoints queries the global endpoint without a memoryId', async () => {
    requestUrl.mockResolvedValue(envelope([{ history_id: 1, memory_id: 'mem_1', operation: 'update', changed_at: '2026-08-01T00:00:00Z' }]));
    const plugin = makePlugin();
    const entries = await plugin.listCheckpoints();
    expect(entries).toHaveLength(1);
    expect(requestUrl.mock.calls[0][0].url).toContain('/api/checkpoints?limit=20');
  });

  it('listCheckpoints scopes to a memory when memoryId is given', async () => {
    requestUrl.mockResolvedValue(envelope([]));
    const plugin = makePlugin();
    await plugin.listCheckpoints('mem_1', 5);
    expect(requestUrl.mock.calls[0][0].url).toContain('/api/memories/mem_1/checkpoints?limit=5');
  });

  it('restoreCheckpoint POSTs to /api/checkpoints/:historyId/restore', async () => {
    requestUrl.mockResolvedValue(envelope({ id: 'mem_1', content: 'restored' }));
    const plugin = makePlugin();
    await plugin.restoreCheckpoint(5);
    const [opts] = requestUrl.mock.calls[0];
    expect(opts.method).toBe('POST');
    expect(opts.url).toContain('/api/checkpoints/5/restore');
  });
});

describe('settings', () => {
  it('loadSettings merges stored values over defaults', async () => {
    const plugin = makePlugin();
    plugin.loadData = vi.fn(async () => ({ serverUrl: 'http://x', refreshInterval: 5 })) as any;
    await plugin.loadSettings();
    expect(plugin.settings.serverUrl).toBe('http://x');
    expect(plugin.settings.refreshInterval).toBe(5);
    expect(plugin.settings.syncFolder).toBe('MemVault'); // default preserved
  });

  it('saveSettings persists the current settings object', async () => {
    const plugin = makePlugin();
    plugin.saveData = vi.fn(async () => {}) as any;
    await plugin.saveSettings();
    expect(plugin.saveData).toHaveBeenCalledWith(plugin.settings);
  });
});

describe('syncVaultFromServer', () => {
  function fakeApp(memories: RemoteMemory[], existing: { path: string; id: string; updatedAt: string }[]) {
    const files = existing.map((e) => Object.assign(new TFile(), { path: e.path }));
    const vault = {
      getAbstractFileByPath: vi.fn((p: string) => files.find((f) => f.path === p) ?? null),
      getMarkdownFiles: vi.fn(() => files as any),
      createFolder: vi.fn(async () => {}),
      create: vi.fn(async () => {}),
      modify: vi.fn(async () => {}),
      delete: vi.fn(async () => {}),
    };
    const metadataCache = {
      getFileCache: vi.fn((file: any) => {
        const hit = existing.find((e) => e.path === file.path);
        return hit ? { frontmatter: { memvault_id: hit.id, memvault_updated_at: hit.updatedAt } } : null;
      }),
    };
    return { app: { vault, metadataCache } as any, vault, metadataCache };
  }

  it('creates new notes, skips current, and updates stale ones', async () => {
    const mems = [
      remoteMemory({ id: 'mem_create', content: 'brand new', updated_at: '2026-08-20T00:00:00Z' }),
      remoteMemory({ id: 'mem_same', content: 'same', updated_at: '2026-08-20T00:00:00Z' }),
      remoteMemory({ id: 'mem_stale', content: 'stale', updated_at: '2026-08-22T00:00:00Z' }),
    ];
    const existing = [
      { path: 'MemVault/same--x.md', id: 'mem_same', updatedAt: '2026-08-20T00:00:00Z' },
      { path: 'MemVault/stale--x.md', id: 'mem_stale', updatedAt: '2026-08-01T00:00:00Z' },
    ];
    const { app, vault } = fakeApp(mems, existing);
    requestUrl.mockResolvedValue(envelope(mems));

    const plugin = makePlugin();
    plugin.app = app;
    await plugin.syncVaultFromServer();

    expect(vault.createFolder).toHaveBeenCalledWith('MemVault');
    expect(vault.create).toHaveBeenCalledTimes(1);
    expect(vault.modify).toHaveBeenCalledTimes(1);
    const createPath = (vault.create.mock.calls[0][0] as string);
    expect(createPath.startsWith('MemVault/')).toBe(true);
    expect(Notice).toHaveBeenCalledWith(expect.stringContaining('1 created, 1 updated, 1 unchanged'));
  });

  it('deletes orphaned notes only when enabled', async () => {
    const mems = [remoteMemory({ id: 'mem_keep' })];
    const existing = [
      { path: 'MemVault/keep--x.md', id: 'mem_keep', updatedAt: '2026-08-20T00:00:00Z' },
      { path: 'MemVault/orphan--x.md', id: 'mem_gone', updatedAt: '2026-08-01T00:00:00Z' },
    ];
    const { app, vault } = fakeApp(mems, existing);
    requestUrl.mockResolvedValue(envelope(mems));

    const plugin = makePlugin();
    plugin.app = app;
    plugin.settings.syncDeleteOrphans = true;
    await plugin.syncVaultFromServer();

    expect(vault.create).not.toHaveBeenCalled(); // keep already current
    expect(vault.delete).toHaveBeenCalledTimes(1);
    const deletedPath = vault.delete.mock.calls[0][0].path;
    expect(deletedPath).toBe('MemVault/orphan--x.md');
    expect(Notice).toHaveBeenCalledWith(expect.stringContaining('1 unchanged, 1 removed'));
  });
});
