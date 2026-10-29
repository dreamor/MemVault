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
    serverUrl: 'http://127.0.0.1:3777',
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
    const fileManager = { trashFile: vi.fn(async () => {}) };
    return { app: { vault, fileManager, metadataCache } as any, vault, fileManager, metadataCache };
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
    const { app, vault, fileManager } = fakeApp(mems, existing);
    requestUrl.mockResolvedValue(envelope(mems));

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
});
