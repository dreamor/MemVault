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
  // These supported Obsidian APIs are mocked only for the scalar properties
  // consumed by sync; the production code uses Obsidian's YAML parser.
  getFrontMatterInfo: (content: string) => {
    const match = content.match(/^---\r?\n([\s\S]*?)\r?\n---(?:\r?\n|$)/);
    return { exists: !!match, frontmatter: match?.[1] ?? '' };
  },
  parseYaml: (yaml: string) => Object.fromEntries(yaml.split('\n').flatMap((line) => {
    const match = line.match(/^([^:]+):\s*(.*)$/);
    if (!match) return [];
    const value = match[2].startsWith('"') ? JSON.parse(match[2]) as unknown
      : match[1] === 'memvault_updated_at' && !Number.isNaN(Date.parse(match[2])) ? new Date(match[2]) : match[2];
    return [[match[1], value]];
  })),
}));

import MemVaultPlugin, { MemVaultSettingTab } from './main';
import { RemoteMemory, fileNameFor } from './sync';

function makePlugin() {
  const plugin = Object.create(MemVaultPlugin.prototype) as MemVaultPlugin;
  plugin.settings = {
    serverUrl: 'http://127.0.0.1:3777',
    refreshInterval: 10,
    apiKey: 'k3y',
    syncFolder: 'MemVault',
    syncDeleteOrphans: false,
  };
  return plugin;
}

function envelope(data: unknown, status = 200) {
  return { status, json: { ok: true, data } };
}

function syncPage(records: RemoteMemory[], overrides: Record<string, unknown> = {}) {
  return {
    records, total_count: records.length, membership_digest: 'fixture-membership',
    next_cursor: null, has_more: false, ...overrides,
  };
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
    ai_generated: false,
    confidence: 0.9,
    access_count: 0,
    decay_score: 1,
    created_at: '2026-08-01T00:00:00Z',
    updated_at: '2026-08-20T00:00:00Z',
    occurred_at: null,
    expires_at: null,
    archived_at: null,
    source_agent: 'test',
    visibility: 'scoped',
    superseded_by: null,
    proposes_supersede_of: null,
    skill_meta: null,
    friction_evidence: null,
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
    expect(opts.url).toBe('http://127.0.0.1:3777/api/memories');
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
  it.each([{}, { fact_assertion: null, fact_management: null }, { identity_verified: false }])(
    'syncPage maps wire fields and preserves optional reports: %j', async (fields) => {
      const expected = remoteMemory(fields);
      const { memory_type, ...rest } = expected;
      requestUrl.mockResolvedValue(envelope(syncPage([{ ...rest, type: memory_type } as any])));
      const plugin = makePlugin();
      const result = await plugin.syncPage('id /?&', 'project:fixture &');
      expect(result).toMatchObject({ records: [expected], total_count: 1 });
      const opts = requestUrl.mock.calls[0][0];
      const url = new URL(opts.url);
      expect(url.pathname).toBe('/api/sync/memories');
      expect(url.searchParams.get('limit')).toBe('1000');
      expect(url.searchParams.get('cursor')).toBe('id /?&');
      expect(url.searchParams.get('namespace')).toBe('project:fixture &');
      expect(opts.headers).toEqual({ 'X-MemVault-Api-Key': 'k3y' });
      expect(opts.body).toBeUndefined();
    },
  );

  it.each(['', '\u0001', '\u007f', '\u0085', 'a'.repeat(1025), 'é'.repeat(513), '\u{1F600}'.repeat(257)])(
    'rejects invalid input cursor bounds before issuing HTTP %#', async (cursor) => {
      await expect(makePlugin().syncPage(cursor)).rejects.toThrow('Invalid sync cursor');
      expect(requestUrl).not.toHaveBeenCalled();
    },
  );

  it.each(['a'.repeat(1024), 'é'.repeat(512)])('URL-encodes an opaque input cursor at the byte limit %#', async (cursor) => {
    requestUrl.mockResolvedValue(envelope(syncPage([])));
    await makePlugin().syncPage(cursor);
    const url = new URL(requestUrl.mock.calls[0][0].url);
    expect(url.pathname).toBe('/api/sync/memories');
    expect(url.searchParams.get('cursor')).toBe(cursor);
  });

  it('listMemories GETs /api/memories with limit', async () => {
    requestUrl.mockResolvedValue(envelope([remoteMemory()]));
    const plugin = makePlugin();
    const list = await plugin.listMemories(25);
    expect(list).toHaveLength(1);
    expect(requestUrl.mock.calls[0][0].url).toContain('/api/memories?limit=25');
  });

  it.each([
    {},
    { fact_assertion: null, fact_management: null },
    { identity_verified: false, fact_assertion: null, fact_management: null },
  ])('list and search preserve legacy/nullable protocol fields: %j', async (fields) => {
    const expected = remoteMemory(fields);
    const { memory_type, ...rest } = expected;
    const wire = { ...rest, type: memory_type };
    requestUrl
      .mockResolvedValueOnce(envelope([wire]))
      .mockResolvedValueOnce(envelope([{ memory: wire, score: 0.75 }]));
    const plugin = makePlugin();
    expect(await plugin.listMemories()).toEqual([expected]);
    expect(await plugin.searchMemories('python')).toEqual([{ memory: expected, score: 0.75 }]);
  });

  it.each(['active', 'suspended'] as const)(
    'list and search retain server-owned %s management without resolving it',
    async (state) => {
      const expected = remoteMemory({
        id: 'mem_managed',
        identity_verified: true,
        fact_assertion: state === 'active' ? {
          key: 'coding.language',
          value: 'Python',
          source_namespace: 'project:example',
          source_order: 7,
          cardinality: 'single',
        } : null,
        fact_management: {
          record_id: 'mem_managed',
          producer_id: 'verified-producer',
          namespace: 'global',
          source_namespace: 'project:example',
          key: 'coding.language',
          source_order: 7,
          state,
        },
      });
      const { memory_type, ...rest } = expected;
      const wire = { ...rest, type: memory_type };
      const before = JSON.stringify(wire);
      requestUrl
        .mockResolvedValueOnce(envelope([wire]))
        .mockResolvedValueOnce(envelope([{ memory: wire, score: 0.75 }]))
        .mockResolvedValueOnce(envelope({ id: 'mem_new' }));
      const plugin = makePlugin();
      expect(await plugin.listMemories()).toEqual([expected]);
      const results = await plugin.searchMemories('python');
      expect(results).toEqual([{ memory: expected, score: 0.75 }]);
      expect(JSON.stringify(wire)).toBe(before);
      // Capturing edited text is a new public save, not a way to reattach the
      // old assertion, replay source order, verify identity, or reactivate it.
      await plugin.saveMemory('User now prefers Go', results[0].memory.priority, results[0].memory.memory_type);
      expect(JSON.parse(requestUrl.mock.calls[2][0].body)).toEqual({
        content: 'User now prefers Go',
        priority: 'MUST',
        type: 'preference',
        agent_id: 'obsidian',
        agent_type: 'note-editor',
        namespace: 'global',
      });
      expect(expected.fact_management?.state).toBe(state);
    },
  );

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
    expect(Object.keys(body).sort()).toEqual([
      'agent_id', 'agent_type', 'content', 'namespace', 'priority', 'type',
    ]);
    expect(Notice).toHaveBeenCalledWith('Saved: mem_1');
  });







});

describe('settings', () => {
  it('uses the managed backend port by default', async () => {
    const plugin = makePlugin();
    plugin.loadData = vi.fn(async () => null) as any;
    await plugin.loadSettings();
    expect(plugin.settings.serverUrl).toBe('http://127.0.0.1:3777');
  });

  it('does not rewrite an existing legacy URL while loading settings', async () => {
    const plugin = makePlugin();
    plugin.loadData = vi.fn(async () => ({ serverUrl: 'http://127.0.0.1:8080' })) as any;
    await plugin.loadSettings();
    expect(plugin.settings.serverUrl).toBe('http://127.0.0.1:8080');
  });

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

describe('server detection', () => {
  it('accepts the managed backend on 3777', async () => {
    requestUrl.mockResolvedValue({ json: { status: 'ok', service: 'memvault-mcp' } });
    const plugin = makePlugin();
    expect(await plugin.detectServerUrl()).toBe('http://127.0.0.1:3777');
    expect(requestUrl.mock.calls[0][0].url).toBe('http://127.0.0.1:3777/api/health');
  });

  it('falls back to the legacy 8080 port only after 3777 fails', async () => {
    requestUrl
      .mockRejectedValueOnce(new Error('connection refused'))
      .mockResolvedValueOnce({ json: { status: 'ok', service: 'memvault-mcp' } });
    const plugin = makePlugin();
    expect(await plugin.detectServerUrl()).toBe('http://127.0.0.1:8080');
    expect(requestUrl.mock.calls.map(([opts]) => opts.url)).toEqual([
      'http://127.0.0.1:3777/api/health',
      'http://127.0.0.1:8080/api/health',
    ]);
  });

  it('rejects a different service on both candidate ports', async () => {
    requestUrl.mockResolvedValue({ json: { status: 'ok', service: 'other-server' } });
    const plugin = makePlugin();
    expect(await plugin.detectServerUrl()).toBeNull();
    expect(requestUrl).toHaveBeenCalledTimes(2);
  });

  it('saves an explicitly detected URL and notifies the user', async () => {
    requestUrl.mockResolvedValue({ json: { status: 'ok', service: 'memvault-mcp' } });
    const plugin = makePlugin();
    plugin.settings.serverUrl = 'http://127.0.0.1:9999';
    plugin.saveData = vi.fn(async () => {}) as any;
    plugin.refreshOpenViews = vi.fn();
    expect(await plugin.detectAndSaveServer()).toBe(true);
    expect(plugin.settings.serverUrl).toBe('http://127.0.0.1:3777');
    expect(plugin.saveData).toHaveBeenCalledWith(plugin.settings);
    expect(plugin.refreshOpenViews).toHaveBeenCalled();
    expect(Notice).toHaveBeenCalledWith('Detected MemVault server at http://127.0.0.1:3777');
  });
});

describe('syncVaultFromServer', () => {
  function fakeApp(
    memories: RemoteMemory[],
    existing: { path: string; id: string; updatedAt: string; status?: string | null }[],
  ) {
    const files = new Map(existing.map((e) => [
      e.path, Object.assign(new TFile(), { path: e.path, content: 'Existing note' }),
    ]));
    const entries = new Map(existing.map((e) => [e.path, e]));
    const folders = new Set<string>();
    const recordWrite = (path: string, content: string) => {
      entries.set(path, {
        path,
        id: content.match(/^memvault_id: (.*)$/m)?.[1] ?? '',
        updatedAt: content.match(/^memvault_updated_at: (.*)$/m)?.[1] ?? '',
        status: content.match(/^status: (.*)$/m)?.[1] ?? null,
      });
    };
    // Persist writes and reject duplicate creates like the real vault. This also
    // lets a subsequent sync index the notes created by the preceding one.
    const vault = {
      getAbstractFileByPath: vi.fn((p: string) => files.get(p) ?? (folders.has(p) ? { path: p } : null)),
      getMarkdownFiles: vi.fn(() => [...files.values()] as any),
      createFolder: vi.fn(async (path: string) => { folders.add(path); }),
      create: vi.fn(async (path: string, content: string) => {
        if (files.has(path) || folders.has(path)) throw new Error(`File already exists: ${path}`);
        const file = Object.assign(new TFile(), { path, content });
        files.set(path, file);
        recordWrite(path, content);
        return file;
      }),
      modify: vi.fn(async (file: { path: string }, content: string) => {
        const current = files.get(file.path);
        if (current) current.content = content;
        recordWrite(file.path, content);
      }),
      read: vi.fn(async (file: { path: string }) => {
        const current = files.get(file.path);
        if (!current) throw new Error('Fixture file missing');
        return current.content;
      }),
      rename: vi.fn(async (file: { path: string }, path: string) => {
        const current = files.get(file.path)!;
        files.delete(file.path);
        entries.delete(file.path);
        current.path = path;
        files.set(path, current);
        recordWrite(path, current.content);
      }),
      delete: vi.fn(async (file: { path: string }) => {
        files.delete(file.path);
        entries.delete(file.path);
      }),
    };
    const metadataCache = {
      getFileCache: vi.fn((file: any) => {
        const hit = entries.get(file.path);
        if (!hit) return null;
        const frontmatter: Record<string, unknown> = {
          memvault_id: hit.id,
          memvault_updated_at: hit.updatedAt,
        };
        // Notes written by the current plugin carry a status, and the sync
        // decision compares against it. Defaulted to 'active' so these tests
        // stay about timestamps. Passing `status: null` models a note from
        // before status existed, where the key is absent from Obsidian's cache
        // entirely rather than present-and-empty — the distinction main.ts's
        // `typeof === 'string'` guard turns into `undefined`.
        if (hit.status !== null) frontmatter.status = hit.status ?? 'active';
        return { frontmatter };
      }),
    };
    const fileManager = { trashFile: vi.fn(async (file: { path: string }) => {
      files.delete(file.path);
      entries.delete(file.path);
    }) };
    return { app: { vault, fileManager, metadataCache } as any, vault, fileManager, metadataCache };
  }

  function deferred() {
    let resolve!: () => void;
    let reject!: (error: Error) => void;
    const promise = new Promise<void>((yes, no) => { resolve = yes; reject = no; });
    return { promise, resolve, reject };
  }

  /** Capture the real registered command and the sidebar's real Sync handler. */
  async function syncTriggers(plugin: MemVaultPlugin) {
    const registerView = vi.fn();
    const addCommand = vi.fn();
    plugin.loadData = vi.fn(async () => plugin.settings);
    plugin.registerView = registerView;
    plugin.addCommand = addCommand;
    plugin.addRibbonIcon = vi.fn();
    plugin.addSettingTab = vi.fn();
    await plugin.onload();
    const command = addCommand.mock.calls.find(([c]) => c.id === 'sync-vault')![0];
    const view = registerView.mock.calls[0][1]({});
    const button = { setAttr: vi.fn(), onclick: null as (() => Promise<void>) | null };
    view.renderHeader({ createDiv: () => ({ createSpan: vi.fn(), createEl: () => button }) });
    return { command: () => command.callback(), button: () => button.onclick!() };
  }

  it.each(['missing-cache', 'missing-frontmatter', 'empty-frontmatter'])('converges across completed syncs before deferred metadata publication: %s', async (lag) => {
    const memory = remoteMemory({ id: 'mem_12345678-first' });
    const { app, vault, metadataCache } = fakeApp([memory], []);
    const liveCache = metadataCache.getFileCache.getMockImplementation()!;
    const publication = deferred();
    const published = publication.promise.then(() => metadataCache.getFileCache.mockImplementation(liveCache));
    metadataCache.getFileCache.mockReturnValue(lag === 'missing-cache' ? null
      : lag === 'empty-frontmatter' ? { frontmatter: {} } as any : {} as any);
    requestUrl.mockResolvedValue(envelope(syncPage([memory])));
    const plugin = makePlugin();
    plugin.app = app;
    try {
      await plugin.syncVaultFromServer();
      await plugin.syncVaultFromServer();
      expect(vault.getMarkdownFiles()).toHaveLength(1);
      expect(vault.create).toHaveBeenCalledTimes(1);
      expect(vault.modify).not.toHaveBeenCalled();
      expect(vault.read).toHaveBeenCalledTimes(1);
    } finally {
      publication.resolve();
      await published;
    }
    await plugin.syncVaultFromServer();
    expect(vault.create).toHaveBeenCalledTimes(1);
    expect(vault.read).toHaveBeenCalledTimes(1);
  });

  it('rediscovers the actual full ID after plugin reload while metadata is unpublished', async () => {
    const memory = remoteMemory({ id: 'mem_reload' });
    const { app, vault, metadataCache } = fakeApp([memory], []);
    metadataCache.getFileCache.mockReturnValue(null);
    requestUrl.mockResolvedValue(envelope(syncPage([memory])));
    const first = makePlugin();
    first.app = app;
    await first.syncVaultFromServer();
    const reloaded = makePlugin();
    reloaded.app = app;
    await reloaded.syncVaultFromServer();
    expect(vault.getMarkdownFiles()).toHaveLength(1);
    expect(vault.create).toHaveBeenCalledTimes(1);
    expect(vault.modify).not.toHaveBeenCalled();
  });

  it('rediscovers a manually renamed note during metadata lag rather than trusting a cached path', async () => {
    const memory = remoteMemory({ id: 'mem_rename' });
    const { app, vault, metadataCache } = fakeApp([memory], []);
    metadataCache.getFileCache.mockReturnValue(null);
    requestUrl.mockResolvedValue(envelope(syncPage([memory])));
    const plugin = makePlugin();
    plugin.app = app;
    await plugin.syncVaultFromServer();
    await vault.rename(vault.getMarkdownFiles()[0], 'MemVault/manual-name.md');
    await plugin.syncVaultFromServer();
    expect(vault.getMarkdownFiles()).toHaveLength(1);
    expect(vault.getMarkdownFiles()[0].path).toBe('MemVault/manual-name.md');
    expect(vault.create).toHaveBeenCalledTimes(1);
    expect(vault.modify).not.toHaveBeenCalled();
  });

  it('recreates a deleted note during metadata lag instead of retaining a stale path mapping', async () => {
    const memory = remoteMemory({ id: 'mem_deleted' });
    const { app, vault, metadataCache } = fakeApp([memory], []);
    metadataCache.getFileCache.mockReturnValue(null);
    requestUrl.mockResolvedValue(envelope(syncPage([memory])));
    const plugin = makePlugin();
    plugin.app = app;
    await plugin.syncVaultFromServer();
    const path = vault.getMarkdownFiles()[0].path;
    await vault.delete(vault.getMarkdownFiles()[0]);
    await plugin.syncVaultFromServer();
    expect(vault.getMarkdownFiles()).toHaveLength(1);
    expect(vault.getMarkdownFiles()[0].path).toBe(path);
    expect(vault.create).toHaveBeenCalledTimes(2);
  });

  it.each(['changed-ID', 'unmanaged'])('does not treat a previously written path as self after %s during metadata lag', async (change) => {
    const memory = remoteMemory({ id: 'mem_old' });
    const { app, vault, metadataCache } = fakeApp([memory], []);
    metadataCache.getFileCache.mockReturnValue(null);
    requestUrl.mockResolvedValue(envelope(syncPage([memory])));
    const plugin = makePlugin();
    plugin.app = app;
    await plugin.syncVaultFromServer();
    const file = vault.getMarkdownFiles()[0];
    const content = change === 'changed-ID'
      ? file.content.replace(/^memvault_id: .*$/m, 'memvault_id: mem_other')
      : 'Unmanaged user note';
    await vault.modify(file, content);
    vault.modify.mockClear();
    await plugin.syncVaultFromServer();
    expect(file.content).toBe(content);
    expect(vault.getMarkdownFiles()).toHaveLength(2);
    expect(vault.create).toHaveBeenCalledTimes(2);
    expect(vault.modify).not.toHaveBeenCalled();
  });

  it('fails safely on a cache-lag file read error, clears the flight, and retries without duplicate writes', async () => {
    const memory = remoteMemory({ id: 'mem_read_failure' });
    const { app, vault, metadataCache } = fakeApp([memory], []);
    metadataCache.getFileCache.mockReturnValue(null);
    requestUrl.mockResolvedValue(envelope(syncPage([memory])));
    const plugin = makePlugin();
    plugin.app = app;
    await plugin.syncVaultFromServer();
    vault.read.mockRejectedValueOnce(new Error('Fixture read failure'));
    await expect(plugin.syncVaultFromServer()).rejects.toThrow('Fixture read failure');
    await plugin.syncVaultFromServer();
    expect(vault.getMarkdownFiles()).toHaveLength(1);
    expect(vault.create).toHaveBeenCalledTimes(1);
    expect(vault.modify).not.toHaveBeenCalled();
  });

  it.each([
    ['same title', 'same title'],
    ['Same title!', 'same title?'],
  ])('preserves colliding short-ID filenames for titles %j and %j, then converges', async (left, right) => {
    const memories = [
      remoteMemory({ id: 'mem_12345678-first', content: left }),
      remoteMemory({ id: 'mem_12345678-second', content: right }),
    ];
    const { app, vault } = fakeApp(memories, []);
    requestUrl.mockResolvedValue(envelope(syncPage(memories)));
    const plugin = makePlugin();
    plugin.app = app;
    await plugin.syncVaultFromServer();
    const paths = vault.create.mock.calls.map(([path]) => path);
    expect(new Set(paths).size).toBe(2);
    expect(paths[0]).toBe(`MemVault/30-Memories/${fileNameFor(memories[0])}`);
    expect(vault.getMarkdownFiles()).toHaveLength(2);
    expect(vault.create.mock.calls.map(([, content]) => content.match(/^memvault_id: (.*)$/m)![1]))
      .toEqual(memories.map((memory) => memory.id));
    await plugin.syncVaultFromServer();
    expect(vault.create).toHaveBeenCalledTimes(2);
    expect(vault.modify).not.toHaveBeenCalled();
  });

  it.each(['', 'mem_unrelated'])('does not overwrite or reuse an occupied unrelated path with local ID %j', async (id) => {
    const memory = remoteMemory({ id: 'mem_12345678-first', content: 'same title' });
    const path = `MemVault/30-Memories/${fileNameFor(memory)}`;
    const { app, vault } = fakeApp([memory], [{ path, id, updatedAt: '' }]);
    const unrelated = vault.getAbstractFileByPath(path);
    requestUrl.mockResolvedValue(envelope(syncPage([memory])));
    const plugin = makePlugin();
    plugin.app = app;
    await plugin.syncVaultFromServer();
    expect(vault.create.mock.calls[0][0]).not.toBe(path);
    expect(vault.getAbstractFileByPath(path)).toBe(unrelated);
    expect(unrelated).toMatchObject({ content: 'Existing note' });
    expect(vault.getMarkdownFiles()).toHaveLength(2);
    expect(vault.modify).not.toHaveBeenCalled();
  });

  it('preserves full-ID indexed renamed paths across title and kind changes', async () => {
    const memory = remoteMemory({ id: 'mem_12345678-second', content: 'changed title', memory_type: 'skill' });
    const path = 'MemVault/custom/renamed-by-user.md';
    const { app, vault } = fakeApp([memory], [{ path, id: memory.id, updatedAt: '' }]);
    requestUrl.mockResolvedValue(envelope(syncPage([memory])));
    const plugin = makePlugin();
    plugin.app = app;
    await plugin.syncVaultFromServer();
    await plugin.syncVaultFromServer();
    expect(vault.create).not.toHaveBeenCalled();
    expect(vault.modify).toHaveBeenCalledTimes(1);
    expect(vault.modify.mock.calls[0][0]).toMatchObject({ path });
  });

  it('allocates distinct bounded paths for escaped and long IDs without truncation collisions', async () => {
    const memories = [
      remoteMemory({ id: 'mem_/../?*one', content: 'same title' }),
      remoteMemory({ id: 'mem_/../?*two', content: 'same title' }),
      remoteMemory({ id: `mem_${'a'.repeat(2048)}one`, content: '同'.repeat(80) }),
      remoteMemory({ id: `mem_${'a'.repeat(2048)}two`, content: '同'.repeat(80) }),
    ];
    const { app, vault } = fakeApp(memories, []);
    // The legacy array is intentionally incomplete; note updates remain usable.
    requestUrl.mockResolvedValue(envelope(memories));
    const plugin = makePlugin();
    plugin.app = app;
    await plugin.syncVaultFromServer();
    const paths = vault.create.mock.calls.map(([path]) => path);
    expect(new Set(paths).size).toBe(4);
    for (const path of paths) {
      const name = path.slice('MemVault/30-Memories/'.length);
      expect(name).not.toMatch(/[\\/:*?"<>|\u0000-\u001f]/);
      expect(new TextEncoder().encode(name).length).toBeLessThanOrEqual(255);
    }
  });

  it.each([2, 3])('coalesces %s overlapping real command/sidebar/repeated triggers before HTTP and create', async (count) => {
    const memory = remoteMemory({ id: 'mem_12345678-first' });
    const { app, vault } = fakeApp([memory], []);
    const httpStarted = deferred();
    const releaseHttp = deferred();
    const createStarted = deferred();
    const releaseCreate = deferred();
    requestUrl.mockImplementation(async () => {
      httpStarted.resolve();
      await releaseHttp.promise;
      return envelope(syncPage([memory]));
    });
    const create = vault.create.getMockImplementation()!;
    vault.create.mockImplementation(async (path, content) => {
      createStarted.resolve();
      await releaseCreate.promise;
      return create(path, content);
    });
    const plugin = makePlugin();
    plugin.app = app;
    const triggers = await syncTriggers(plugin);
    // A repeated public command is external, not the sidebar refresh timer.
    const calls = [triggers.command(), triggers.button()];
    if (count === 3) calls.push(triggers.command());
    const settled = Promise.allSettled(calls);
    try {
      await httpStarted.promise;
      expect(requestUrl).toHaveBeenCalledTimes(1);
      releaseHttp.resolve();
      await createStarted.promise;
      expect(vault.create).toHaveBeenCalledTimes(1);
    } finally {
      releaseHttp.resolve();
      releaseCreate.resolve();
      await settled;
    }
    expect((await settled).every((result) => result.status === 'fulfilled')).toBe(true);
    expect(vault.getMarkdownFiles()).toHaveLength(1);
    expect(Notice).not.toHaveBeenCalledWith(expect.stringContaining('Sync failed:'));
    await plugin.syncVaultFromServer();
    expect(vault.create).toHaveBeenCalledTimes(1);
    expect(vault.modify).not.toHaveBeenCalled();
  });

  it('rejects every shared caller on write failure, cleans up, and permits a later sync', async () => {
    const memory = remoteMemory({ id: 'mem_failure' });
    const { app, vault } = fakeApp([memory], []);
    requestUrl.mockResolvedValue(envelope(syncPage([memory])));
    const started = deferred();
    const gate = deferred();
    const error = new Error('Fixture write failure');
    const create = vault.create.getMockImplementation()!;
    vault.create.mockImplementation(async (path, content) => {
      started.resolve();
      await gate.promise;
      return create(path, content);
    });
    const plugin = makePlugin();
    plugin.app = app;
    const calls = [plugin.syncVaultFromServer(), plugin.syncVaultFromServer(), plugin.syncVaultFromServer()];
    const settled = Promise.allSettled(calls);
    try {
      await started.promise;
      expect(vault.create).toHaveBeenCalledTimes(1);
    } finally {
      gate.reject(error);
      await settled;
    }
    expect(await settled).toEqual(calls.map(() => ({ status: 'rejected', reason: error })));
    expect(vault.getMarkdownFiles()).toHaveLength(0);
    vault.create.mockImplementation(create);
    await plugin.syncVaultFromServer();
    await plugin.syncVaultFromServer();
    expect(vault.create).toHaveBeenCalledTimes(2); // one failed attempt, one successful write
    expect(vault.getMarkdownFiles()).toHaveLength(1);
  });

  it('does not duplicate updates while a sync write is deferred', async () => {
    const memory = remoteMemory({ id: 'mem_update' });
    const { app, vault } = fakeApp([memory], [{ path: 'MemVault/renamed.md', id: memory.id, updatedAt: '' }]);
    requestUrl.mockResolvedValue(envelope(syncPage([memory])));
    const started = deferred();
    const gate = deferred();
    const modify = vault.modify.getMockImplementation()!;
    vault.modify.mockImplementation(async (file, content) => {
      started.resolve();
      await gate.promise;
      await modify(file, content);
    });
    const plugin = makePlugin();
    plugin.app = app;
    const settled = Promise.allSettled([plugin.syncVaultFromServer(), plugin.syncVaultFromServer()]);
    try {
      await started.promise;
      expect(vault.modify).toHaveBeenCalledTimes(1);
    } finally {
      gate.resolve();
      await settled;
    }
    await plugin.syncVaultFromServer();
    expect(vault.modify).toHaveBeenCalledTimes(1);
    expect(vault.create).not.toHaveBeenCalled();
  });

  it('keeps orphan recheck and deferred trash in the same shared flight', async () => {
    const { app, vault, fileManager } = fakeApp([], [{ path: 'MemVault/gone.md', id: 'mem_gone', updatedAt: '' }]);
    requestUrl.mockImplementation(async (options) => new URL(options.url).pathname === '/api/sync/memories'
      ? envelope(syncPage([])) : { status: 404 });
    const started = deferred();
    const gate = deferred();
    const trash = fileManager.trashFile.getMockImplementation()!;
    fileManager.trashFile.mockImplementation(async (file) => {
      started.resolve();
      await gate.promise;
      await trash(file);
    });
    const plugin = makePlugin();
    plugin.app = app;
    plugin.settings.syncDeleteOrphans = true;
    const settled = Promise.allSettled([plugin.syncVaultFromServer(), plugin.syncVaultFromServer()]);
    try {
      await started.promise;
      expect(requestUrl).toHaveBeenCalledTimes(2); // one snapshot, one current-ID absence GET
      expect(fileManager.trashFile).toHaveBeenCalledTimes(1);
    } finally {
      gate.resolve();
      await settled;
    }
    expect((await settled).every((result) => result.status === 'fulfilled')).toBe(true);
    expect(vault.getMarkdownFiles()).toHaveLength(0);
    await plugin.syncVaultFromServer();
    expect(fileManager.trashFile).toHaveBeenCalledTimes(1);
  });

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
    const { app, vault, fileManager } = fakeApp(mems, existing);
    requestUrl
      .mockResolvedValueOnce(envelope(syncPage(mems)))
      .mockResolvedValueOnce({ status: 404, json: { ok: false, error: 'memory not found' } });

    const plugin = makePlugin();
    plugin.app = app;
    plugin.settings.syncDeleteOrphans = true;
    await plugin.syncVaultFromServer();

    expect(vault.create).not.toHaveBeenCalled(); // keep already current
    expect(fileManager.trashFile).toHaveBeenCalledTimes(1);
    const deletedPath = fileManager.trashFile.mock.calls[0][0].path;
    expect(deletedPath).toBe('MemVault/orphan--x.md');
    expect(Notice).toHaveBeenCalledWith(expect.stringContaining('1 unchanged, 1 removed'));
  });

  it('updates an old note beyond 10,000 memories and removes only a confirmed orphan', async () => {
    const memories = Array.from({ length: 10001 }, (_, i) => remoteMemory({
      id: `mem_${String(i).padStart(5, '0')}`, content: `fixture record ${i}`,
    }));
    const existing = [
      { path: 'MemVault/old.md', id: 'mem_10000', updatedAt: '2026-08-01T00:00:00Z' },
      { path: 'MemVault/gone.md', id: 'mem_gone', updatedAt: '2026-08-01T00:00:00Z' },
    ];
    const { app, vault, fileManager } = fakeApp(memories, existing);
    requestUrl.mockImplementation(async (opts) => {
      const url = new URL(opts.url);
      if (url.pathname === '/api/memories/mem_gone') return { status: 404 };
      expect(url.pathname).toBe('/api/sync/memories');
      expect(url.searchParams.get('limit')).toBe('1000');
      const cursor = url.searchParams.get('cursor');
      const start = cursor ? memories.findIndex((m) => m.id === cursor) + 1 : 0;
      const batch = memories.slice(start, start + 1000);
      const more = start + batch.length < memories.length;
      return envelope(syncPage(batch, { total_count: memories.length, has_more: more,
        next_cursor: more ? batch[batch.length - 1].id : null }));
    });
    const plugin = makePlugin();
    plugin.app = app;
    plugin.settings.syncDeleteOrphans = true;
    await plugin.syncVaultFromServer();
    expect(requestUrl).toHaveBeenCalledTimes(12);
    expect(vault.modify).toHaveBeenCalledTimes(1);
    expect(vault.modify.mock.calls[0][0]).toMatchObject({ path: 'MemVault/old.md' });
    expect(vault.modify.mock.calls[0][1]).toContain('fixture record 10000');
    expect(fileManager.trashFile.mock.calls.map(([f]) => f.path)).toEqual(['MemVault/gone.md']);
  });

  it('keeps collection traversal separate from a literal sync-page memory ID recheck', async () => {
    const { app, fileManager } = fakeApp([], [{
      path: 'MemVault/literal-id.md', id: 'sync-page', updatedAt: '',
    }]);
    requestUrl
      .mockResolvedValueOnce(envelope(syncPage([])))
      .mockResolvedValueOnce(envelope(remoteMemory({ id: 'sync-page' })));
    const plugin = makePlugin();
    plugin.app = app;
    plugin.settings.syncDeleteOrphans = true;
    await plugin.syncVaultFromServer();
    expect(requestUrl.mock.calls.map(([opts]) => new URL(opts.url).pathname)).toEqual([
      '/api/sync/memories', '/api/memories/sync-page',
    ]);
    expect(fileManager.trashFile).not.toHaveBeenCalled();
  });

  it.each(['changed', 'removed'] as const)(
    'preserves a note whose local identity is %s while its absence GET is pending', async (change) => {
      const path = 'MemVault/local-identity.md';
      const { app, fileManager, metadataCache } = fakeApp([], [
        { path, id: 'mem_old', updatedAt: '' },
      ]);
      let announceGet!: () => void;
      const getStarted = new Promise<void>((resolve) => { announceGet = resolve; });
      let release404!: (response: { status: number }) => void;
      requestUrl
        .mockResolvedValueOnce(envelope(syncPage([])))
        .mockImplementationOnce(() => new Promise((resolve) => {
          release404 = resolve;
          announceGet();
        }));
      const plugin = makePlugin();
      plugin.app = app;
      plugin.settings.syncDeleteOrphans = true;
      const syncing = plugin.syncVaultFromServer();
      await getStarted;
      expect(requestUrl.mock.calls[1][0].url).toContain('/api/memories/mem_old');
      metadataCache.getFileCache.mockReturnValue({ frontmatter:
        change === 'changed' ? { memvault_id: 'mem_new' } : {},
      });
      release404({ status: 404 });
      await syncing;
      expect(fileManager.trashFile).not.toHaveBeenCalled();
      expect(Notice).not.toHaveBeenCalledWith(expect.stringContaining('removed'));
    },
  );

  it.each([200, 401, 403, 405, 500, 'network', 'envelope404'])(
    'preserves a snapshot orphan unless its GET confirms actual absence: %s', async (status) => {
      const { app, fileManager } = fakeApp([], [
        { path: 'MemVault/alive.md', id: 'mem_ /?&', updatedAt: '' },
      ]);
      requestUrl.mockResolvedValueOnce(envelope(syncPage([])));
      if (status === 'network') requestUrl.mockRejectedValueOnce(new Error('offline'));
      else if (status === 'envelope404') requestUrl.mockResolvedValueOnce({ status: 200, json: { ok: false, error: '404 not found' } });
      else requestUrl.mockResolvedValueOnce(envelope(remoteMemory(), status as number));
      const plugin = makePlugin();
      plugin.app = app;
      plugin.settings.syncDeleteOrphans = true;
      await plugin.syncVaultFromServer();
      expect(fileManager.trashFile).not.toHaveBeenCalled();
      const opts = requestUrl.mock.calls[1][0];
      expect(opts.url).toContain('/api/memories/mem_%20%2F%3F%26');
      expect(opts.method).toBe('GET');
      expect(opts.throw).toBe(false);
      expect(opts.headers).toEqual({ 'X-MemVault-Api-Key': 'k3y' });
    },
  );

  it.each([404, 405])('supports an old server without orphan removal (%s)', async (status) => {
    const memory = remoteMemory({ id: 'mem_keep' });
    const { app, vault, fileManager } = fakeApp([memory], [
      { path: 'MemVault/old.md', id: memory.id, updatedAt: '2026-08-01T00:00:00Z' },
      { path: 'MemVault/not-in-capped-list.md', id: 'mem_live_elsewhere', updatedAt: '' },
    ]);
    requestUrl.mockResolvedValueOnce({ status }).mockResolvedValueOnce(envelope([memory]));
    const plugin = makePlugin();
    plugin.app = app;
    plugin.settings.syncDeleteOrphans = true;
    await plugin.syncVaultFromServer();
    expect(requestUrl).toHaveBeenCalledTimes(2);
    expect(requestUrl.mock.calls[1][0].url).toContain('/api/memories?limit=10000');
    expect(vault.modify).toHaveBeenCalledTimes(1);
    expect(fileManager.trashFile).not.toHaveBeenCalled();
    expect(Notice).toHaveBeenCalledWith(expect.stringContaining('partial sync, orphan removal skipped'));
  });

  it.each([404, 405])('retains legacy sync coverage past 1,000 records without orphan removal (%s)', async (status) => {
    const memories = Array.from({ length: 1001 }, (_, i) => remoteMemory({
      id: `mem_${String(i).padStart(5, '0')}`, content: `legacy fixture ${i}`,
    }));
    const { app, vault, fileManager } = fakeApp(memories, [
      { path: 'MemVault/late.md', id: memories[1000].id, updatedAt: '2026-08-01T00:00:00Z' },
      { path: 'MemVault/outside-bounded-list.md', id: 'mem_still_live', updatedAt: '' },
    ]);
    requestUrl.mockImplementation(async (opts) => {
      const url = new URL(opts.url);
      if (url.pathname === '/api/sync/memories') {
        expect(url.searchParams.get('limit')).toBe('1000');
        return { status };
      }
      expect(url.pathname).toBe('/api/memories');
      const limit = Number(url.searchParams.get('limit'));
      expect(limit).toBe(10000);
      return envelope(memories.slice(0, limit));
    });
    const plugin = makePlugin();
    plugin.app = app;
    plugin.settings.syncDeleteOrphans = true;
    await plugin.syncVaultFromServer();
    expect(requestUrl).toHaveBeenCalledTimes(2);
    expect(vault.create).toHaveBeenCalledTimes(1000);
    expect(vault.modify).toHaveBeenCalledTimes(1);
    expect(vault.modify.mock.calls[0][0]).toMatchObject({ path: 'MemVault/late.md' });
    expect(vault.modify.mock.calls[0][1]).toContain('legacy fixture 1000');
    expect(fileManager.trashFile).not.toHaveBeenCalled();
    expect(Notice).toHaveBeenCalledWith(expect.stringContaining('partial sync, orphan removal skipped'));
  });

  it('restarts from the beginning when a continuation cursor disappears (400)', async () => {
    requestUrl
      .mockResolvedValueOnce(envelope(syncPage([remoteMemory({ id: 'mem_a' })], {
        total_count: 2, has_more: true, next_cursor: 'mem_a',
      })))
      .mockResolvedValueOnce({ status: 400 })
      .mockResolvedValueOnce(envelope(syncPage([remoteMemory({ id: 'mem_c' })])));
    const plugin = makePlugin();
    const snapshot = await plugin.remoteSnapshot();
    expect(snapshot.complete).toBe(true);
    expect(snapshot.records.map((memory) => memory.id)).toEqual(['mem_c']);
    expect(requestUrl.mock.calls.map(([opts]) => new URL(opts.url).searchParams.get('cursor'))).toEqual([null, 'mem_a', null]);
  });

  it.each(['network', '401', '500', 'cursor', 'count', 'digest', 'deleted-cursor', 'no-evidence', 'legacy-array'])(
    'never trashes notes after an incomplete traversal: %s', async (failure) => {
      const memory = remoteMemory({ id: 'mem_a' });
      const { app, fileManager } = fakeApp([memory], [
        { path: 'MemVault/elsewhere.md', id: 'mem_still_live', updatedAt: '' },
      ]);
      let pageCalls = 0;
      requestUrl.mockImplementation(async (opts) => {
        pageCalls++;
        const cursor = new URL(opts.url).searchParams.get('cursor');
        if (failure === 'network') throw new Error('offline');
        if (failure === '401' || failure === '500') return { status: Number(failure) };
        if (failure === 'legacy-array') return envelope([memory]);
        if (failure === 'no-evidence') return envelope({ records: [memory], has_more: false });
        if (failure === 'count') return envelope(syncPage([memory], { total_count: 2 }));
        if (!cursor) return envelope(syncPage([memory], { total_count: 3, has_more: true, next_cursor: 'same' }));
        if (failure === 'deleted-cursor') return { status: 400 };
        return envelope(syncPage([remoteMemory({ id: 'mem_b' })], {
          total_count: 3, has_more: true, next_cursor: 'same',
          membership_digest: failure === 'digest' ? 'changed' : 'fixture-membership',
        }));
      });
      const plugin = makePlugin();
      plugin.app = app;
      plugin.settings.syncDeleteOrphans = true;
      await plugin.syncVaultFromServer();
      expect(fileManager.trashFile).not.toHaveBeenCalled();
      expect(pageCalls).toBeLessThanOrEqual(6);
      expect(requestUrl.mock.calls.every(([opts]) => opts.url.includes('/api/sync/memories?'))).toBe(true);
    },
  );

  it('does not even recheck orphans when the optional deletion setting is off', async () => {
    requestUrl.mockResolvedValue(envelope(syncPage([])));
    const { app, fileManager } = fakeApp([], [{ path: 'MemVault/gone.md', id: 'mem_gone', updatedAt: '' }]);
    const plugin = makePlugin();
    plugin.app = app;
    await plugin.syncVaultFromServer();
    expect(requestUrl).toHaveBeenCalledTimes(1);
    expect(fileManager.trashFile).not.toHaveBeenCalled();
  });

  it('rewrites a note written before status existed, once', async () => {
    // The upgrade path, end to end: main.ts has to actually read `status` out
    // of the frontmatter cache and hand it to decideAction, which sync.test.ts
    // cannot verify because it never touches main.ts. A note with no status is
    // stale by definition — it cannot be showing the memory's real state — so
    // it is rewritten on the first sync after upgrading and settles after that.
    const mems = [remoteMemory({ id: 'mem_legacy', updated_at: '2026-08-20T00:00:00Z' })];
    const existing = [
      { path: 'MemVault/legacy--x.md', id: 'mem_legacy', updatedAt: '2026-08-20T00:00:00Z', status: null },
    ];
    const { app, vault } = fakeApp(mems, existing);
    requestUrl.mockResolvedValue(envelope(mems));

    const plugin = makePlugin();
    plugin.app = app;
    await plugin.syncVaultFromServer();

    expect(vault.modify).toHaveBeenCalledTimes(1);
    // The rewritten note carries the status, so the next sync is a no-op.
    const written = vault.modify.mock.calls[0][1] as string;
    expect(written).toContain('status: active');
    expect(Notice).toHaveBeenCalledWith(expect.stringContaining('1 updated, 0 unchanged'));
  });

  it('does not upload edited note text or user-declared fact properties', async () => {
    const mems = [remoteMemory({
      id: 'mem_managed',
      fact_assertion: null,
      fact_management: {
        record_id: 'mem_managed',
        producer_id: 'verified-producer',
        namespace: 'global',
        source_namespace: 'project:example',
        key: 'coding.language',
        source_order: 7,
        state: 'suspended',
      },
    })];
    const path = 'MemVault/managed.md';
    const { app, vault, metadataCache } = fakeApp(mems, [{
      path, id: 'mem_managed', updatedAt: '2026-08-20T00:00:00Z',
    }]);
    Object.assign(vault.getAbstractFileByPath(path)!, { content: 'User now prefers Go' });
    metadataCache.getFileCache.mockReturnValue({ frontmatter: {
      memvault_id: 'mem_managed',
      memvault_updated_at: '2026-08-20T00:00:00Z',
      status: 'active',
      fact_assertion: {
        key: 'coding.language', value: 'Go', source_namespace: 'project:example',
        source_order: 8, cardinality: 'single',
      },
      fact_management: { ...mems[0].fact_management, state: 'active' },
      identity_verified: true,
    } });
    requestUrl.mockResolvedValue(envelope(mems));
    const plugin = makePlugin();
    plugin.app = app;
    await plugin.syncVaultFromServer();

    // The existing adapter is one-way. A local edit cannot become a PATCH,
    // resolved-fact claim, or source of verification evidence on the server.
    expect(requestUrl).toHaveBeenCalledTimes(1);
    expect(requestUrl.mock.calls[0][0].method).toBe('GET');
    expect(requestUrl.mock.calls[0][0].body).toBeUndefined();
    expect(vault.create).not.toHaveBeenCalled();
    expect(vault.modify).not.toHaveBeenCalled();
    expect(mems[0].fact_assertion).toBeNull();
    expect(mems[0].fact_management?.state).toBe('suspended');
  });

  it('writes the status and its callout for a memory that is not in effect', async () => {
    const mems = [
      remoteMemory({
        id: 'mem_expired',
        updated_at: '2026-08-20T00:00:00Z',
        expires_at: '2026-08-25T00:00:00Z',
      }),
    ];
    const { app, vault } = fakeApp(mems, []);
    requestUrl.mockResolvedValue(envelope(mems));

    const plugin = makePlugin();
    plugin.app = app;
    await plugin.syncVaultFromServer();

    const written = vault.create.mock.calls[0][1] as string;
    expect(written).toContain('status: expired');
    expect(written).toContain('expires_at: 2026-08-25T00:00:00Z');
    expect(written).toContain('[!warning] Expired');
  });
});

// ─── Settings tab (declarative, Obsidian 1.13+) ──────────────────
//
// `control.key` is typed as a plain string, so a typo'd key silently binds
// nothing and tsc cannot catch it. Pin the binding set here, and pin the one
// row that legitimately isn't a `control` (text field + Detect button).
describe('MemVaultSettingTab.getSettingDefinitions()', () => {
  function collect(defs: unknown[]): { controlKeys: string[]; names: string[]; groups: string[] } {
    const controlKeys: string[] = [];
    const names: string[] = [];
    const groups: string[] = [];
    const walk = (items: unknown[]) => {
      for (const raw of items) {
        const item = raw as Record<string, unknown>;
        if (typeof item.name === 'string') names.push(item.name);
        const control = item.control as { key?: string } | undefined;
        if (control?.key) controlKeys.push(control.key);
        if (Array.isArray(item.items)) {
          if (typeof item.heading === 'string') groups.push(item.heading);
          walk(item.items);
        }
      }
    };
    walk(defs);
    return { controlKeys, names, groups };
  }

  it('binds every non-render setting to a real settings key', () => {
    const tab = new MemVaultSettingTab({} as never, makePlugin() as never);
    const { controlKeys, names } = collect(tab.getSettingDefinitions() as unknown[]);

    expect([...controlKeys].sort()).toEqual(
      ['apiKey', 'refreshInterval', 'syncDeleteOrphans', 'syncFolder'],
    );
    for (const key of controlKeys) {
      expect(Object.keys(makePlugin().settings)).toContain(key);
    }
    // Server URL is the one imperative row (text + Detect button).
    expect(names).toContain('Server URL');
  });

  it('keeps the vault-sync rows grouped', () => {
    const tab = new MemVaultSettingTab({} as never, makePlugin() as never);
    const { groups, names } = collect(tab.getSettingDefinitions() as unknown[]);
    expect(groups).toEqual(['Vault sync']);
    expect(names).toContain('Sync folder');
    expect(names).toContain('Delete orphaned notes on sync');
  });
});
