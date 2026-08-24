import { describe, it, expect, vi, beforeAll, afterAll, beforeEach } from 'vitest';
import * as http from 'http';
import type { AddressInfo } from 'net';
import { Memory } from './format';

// ---- vscode stub -----------------------------------------------------------
const state = vi.hoisted(() => {
  const config = (() => {
    const vals: Record<string, unknown> = {
      serverUrl: 'http://127.0.0.1:8080',
      refreshInterval: 0,
      apiKey: '',
    };
    return {
      get(key: string, def?: unknown) {
        return (key in vals ? vals[key] : def) as unknown;
      },
      set(key: string, v: unknown) {
        vals[key] = v;
      },
    };
  })();
  return {
    config,
    commands: new Map<string, (...a: any[]) => any>(),
    providers: new Map<string, any>(),
    infos: [] as string[],
    errors: [] as string[],
    warnings: [] as string[],
    inputs: [] as (string | undefined)[],
    picks: [] as (string | undefined)[],
  };
});

vi.mock('vscode', () => {
  class EventEmitter<T> {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    private fns: ((e: T) => any)[] = [];
    event = (fn: (e: T) => any) => { this.fns.push(fn); return { dispose: () => {} }; };
    fire(e: T) { this.fns.forEach((f) => f(e)); }
  }
  class TreeItem {
    label: string;
    description?: string;
    tooltip?: unknown;
    contextValue?: string;
    collapsibleState: number;
    constructor(label: string, collapsibleState: number) { this.label = label; this.collapsibleState = collapsibleState; }
  }
  class MarkdownString {
    value: string;
    constructor(value: string) { this.value = value; }
  }
  return {
    workspace: {
      getConfiguration: (section: string) => {
        if (section !== 'memvault') throw new Error('unexpected config section');
        return state.config;
      },
      openTextDocument: vi.fn(),
    },
    window: {
      showInputBox: vi.fn(async () => state.inputs.shift()),
      showQuickPick: vi.fn(async () => state.picks.shift()),
      showInformationMessage: vi.fn((m: string) => { state.infos.push(String(m)); }),
      showErrorMessage: vi.fn((m: string) => { state.errors.push(String(m)); }),
      showWarningMessage: vi.fn(async () => state.picks.shift()),
      registerTreeDataProvider: (id: string, p: unknown) => { state.providers.set(id, p); },
      activeTextEditor: undefined,
    },
    commands: {
      registerCommand: (name: string, cb: (...a: any[]) => any) => {
        state.commands.set(name, cb);
        return { dispose: () => {} };
      },
    },
    EventEmitter,
    TreeItem,
    TreeItemCollapsibleState: { None: 0, Collapsed: 1, Expanded: 2 },
    MarkdownString,
  };
});

import { activate, deactivate } from './extension';

// ---- real local REST server ------------------------------------------------
let server: http.Server;
let origin = '';

function envelope(data: unknown, error?: string) {
  return { ok: error ? false : true, data, ...(error ? { error } : {}) };
}

const memory: Memory = {
  id: 'mem_1',
  memory_type: 'preference',
  content: 'User prefers Python',
  instruction: 'Never suggest Java',
  priority: 'MUST',
  namespace: 'global',
  tags: ['python'],
  layer: 'L3',
  skill_meta: null,
  access_count: 2,
  human_reviewed: true,
  created_at: '2026-08-01T00:00:00Z',
  updated_at: '2026-08-01T00:00:00Z',
};

beforeAll(async () => {
  server = http.createServer((req, res) => {
    const chunks: Buffer[] = [];
    req.on('data', (c) => chunks.push(c as Buffer));
    req.on('end', () => {
      const body = Buffer.concat(chunks).toString();
      const send = (payload: unknown, status = 200) => {
        res.writeHead(status, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify(payload));
      };
      const url = req.url ?? '';
      if (url.startsWith('/api/search')) {
        const parsed = JSON.parse(body || '{}');
        if (parsed.query === 'fail') return send(envelope(null, 'search exploded'));
        return send(envelope([{ memory, score: 0.9 }]));
      }
      if (url.startsWith('/api/memories?') && req.method === 'GET') return send(envelope([memory]));
      if (url.startsWith('/api/memories') && req.method === 'POST') return send(envelope({ id: 'mem_new' }));
      if (url.startsWith('/api/memories/') && req.method === 'PUT') return send(envelope({}));
      if (url.startsWith('/api/memories/') && req.method === 'DELETE') return send(envelope({}));
      if (url.startsWith('/api/inbox/') && req.method === 'POST') return send(envelope({}));
      if (url.startsWith('/api/inbox')) return send(envelope({ memories: [memory], total: 1 }));
      if (url.startsWith('/api/dedup')) return send(envelope({ unique: 1, duplicates: 2 }));
      if (url.startsWith('/api/decay')) return send(envelope({ updated: 1, archived: 0 }));
      if (url.startsWith('/api/promote')) return send(envelope({ promoted_to_l2: 1, promoted_to_l3: 0 }));
      return send(envelope(null, `no route ${url}`), 404);
    });
  });
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  const addr = server.address() as AddressInfo;
  origin = `http://127.0.0.1:${addr.port}`;
});

afterAll(async () => {
  await new Promise<void>((resolve) => server.close(() => resolve()));
});

beforeEach(() => {
  state.commands.clear();
  state.providers.clear();
  state.infos = [];
  state.errors = [];
  state.warnings = [];
  state.inputs = [];
  state.picks = [];
  state.config.set('serverUrl', origin);
  state.config.set('apiKey', 'k');
  state.config.set('refreshInterval', 0);
  activate({ subscriptions: [] } as any);
});

deactivate();

describe('activate registration', () => {
  it('registers all commands and both tree providers', () => {
    for (const name of [
      'memvault.refresh',
      'memvault.search',
      'memvault.searchInsert',
      'memvault.saveSelection',
      'memvault.approve',
      'memvault.reject',
      'memvault.delete',
      'memvault.createMemory',
      'memvault.edit',
      'memvault.dedup',
      'memvault.decay',
      'memvault.promote',
      'memvault.showStats',
    ]) {
      expect(state.commands.has(name), `missing ${name}`).toBe(true);
    }
    expect(state.providers.has('memvault.memories')).toBe(true);
    expect(state.providers.has('memvault.inbox')).toBe(true);
  });
});

describe('tree provider', () => {
  it('lists all memories via GET /api/memories', async () => {
    const provider = state.providers.get('memvault.memories');
    const items = await provider.getChildren();
    expect(items).toHaveLength(1);
    expect(items[0].content).toBe('User prefers Python');
  });

  it('lists inbox via GET /api/inbox', async () => {
    const provider = state.providers.get('memvault.inbox');
    const items = await provider.getChildren();
    expect(items).toHaveLength(1);
  });

  it('builds a TreeItem with label/description/contextValue', () => {
    const provider = state.providers.get('memvault.memories');
    const item = provider.getTreeItem(memory);
    expect(item.label).toContain('User prefers Python');
    expect(item.contextValue).toBe('reviewed');
    expect(item.tooltip).toBeDefined();
  });
});

describe('commands', () => {
  async function run(name: string) {
    await state.commands.get(name)(memory);
  }

  it('search shows results or an error message', async () => {
    state.inputs.push('python');
    await run('memvault.search');
    // No QuickPick selection -> silently returns; no error message.
    expect(state.errors).toHaveLength(0);

    state.inputs.push('fail');
    await run('memvault.search');
    expect(state.errors.some((e) => e.includes('search exploded'))).toBe(true);
  });

  it('dedup / decay / promote hit maintenance endpoints and surface results', async () => {
    await run('memvault.dedup');
    expect(state.infos.some((m) => m.includes('Dedup: 1 unique, 2 duplicates'))).toBe(true);

    await run('memvault.decay');
    expect(state.infos.some((m) => m.includes('Decay: 1 updated, 0 archived'))).toBe(true);

    await run('memvault.promote');
    expect(state.infos.some((m) => m.includes('Promote: 1 → L2, 0 → L3'))).toBe(true);
  });

  it('approve / reject / delete call the right endpoints', async () => {
    await run('memvault.approve');
    expect(state.infos.some((m) => m.includes('Approved'))).toBe(true);

    await run('memvault.reject');
    expect(state.infos.some((m) => m.includes('Rejected'))).toBe(true);

    state.picks.push('Delete'); // confirm dialog
    await run('memvault.delete');
    expect(state.infos.some((m) => m.includes('Deleted'))).toBe(true);
  });

  it('createMemory walks the form and POSTs /api/memories', async () => {
    state.inputs.push('prefers tabs over spaces');
    state.picks.push('MUST', 'preference');
    state.inputs.push('Always use tabs', 'global', 'tabs, style');
    await run('memvault.createMemory');
    expect(state.infos.some((m) => m.includes('Created: mem_new'))).toBe(true);
  });

  it('editMemory PUTs the updated form to /api/memories/:id', async () => {
    state.inputs.push('updated content');
    state.picks.push('REFERENCE', 'fact');
    state.inputs.push('', 'global', '');
    await run('memvault.edit');
    expect(state.infos.some((m) => m.includes('Updated'))).toBe(true);
  });

  it('showStats aggregates counts from the full memory list', async () => {
    await run('memvault.showStats');
    expect(state.infos.some((m) => m.includes('Total: 1'))).toBe(true);
    expect(state.infos.some((m) => m.includes('MUST: 1'))).toBe(true);
  });
});
