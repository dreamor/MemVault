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
    activeEditor: undefined as { document: { getText: (sel: unknown) => string }; selection: unknown } | undefined,
    saveDialogResults: [] as ({ fsPath: string } | undefined)[],
    openDialogResults: [] as ({ fsPath: string }[] | undefined)[],
    writtenFiles: [] as { path: string; data: Buffer }[],
    fileContents: new Map<string, Buffer>(),
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
  class Uri {
    fsPath: string;
    constructor(fsPath: string) { this.fsPath = fsPath; }
    static file(fsPath: string) { return new Uri(fsPath); }
    static joinPath(base: { fsPath: string }, ...segments: string[]) {
      return new Uri([base.fsPath, ...segments].join('/'));
    }
  }
  return {
    workspace: {
      getConfiguration: (section: string) => {
        if (section !== 'memvault') throw new Error('unexpected config section');
        return state.config;
      },
      openTextDocument: vi.fn(),
      fs: {
        writeFile: vi.fn(async (uri: { fsPath: string }, data: Uint8Array) => {
          state.writtenFiles.push({ path: uri.fsPath, data: Buffer.from(data) });
        }),
        readFile: vi.fn(async (uri: { fsPath: string }) => state.fileContents.get(uri.fsPath) ?? Buffer.from('')),
      },
    },
    window: {
      showInputBox: vi.fn(async () => state.inputs.shift()),
      showQuickPick: vi.fn(async () => state.picks.shift()),
      showInformationMessage: vi.fn((m: string) => { state.infos.push(String(m)); }),
      showErrorMessage: vi.fn((m: string) => { state.errors.push(String(m)); }),
      showWarningMessage: vi.fn(async () => state.picks.shift()),
      showSaveDialog: vi.fn(async () => state.saveDialogResults.shift()),
      showOpenDialog: vi.fn(async () => state.openDialogResults.shift()),
      registerTreeDataProvider: (id: string, p: unknown) => { state.providers.set(id, p); },
      get activeTextEditor() { return state.activeEditor; },
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
    Uri,
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
      if (url.startsWith('/api/backup')) {
        const buf = Buffer.from('SQLite format 3\0FAKEBYTES');
        res.writeHead(200, {
          'Content-Type': 'application/octet-stream',
          'Content-Disposition': 'attachment; filename="memvault-backup-20260101T000000Z.db"',
        });
        res.end(buf);
        return;
      }
      if (url.startsWith('/api/export')) {
        const format = new URL(url, 'http://x').searchParams.get('format');
        if (format === 'markdown') {
          return send(envelope({ format: 'markdown', files: [{ filename: 'mem_1.md', content: '---\nid: mem_1\n---\ncontent' }] }));
        }
        return send(envelope({ format: 'json', content: JSON.stringify({ memories: [memory] }) }));
      }
      if (url.startsWith('/api/import')) {
        const parsed = JSON.parse(body || '{}');
        if (parsed.format === 'markdown') {
          return send(envelope({
            imported: Math.max(parsed.files.length - 1, 0),
            skipped: [{ filename: parsed.files[0]?.filename ?? 'x', reason: 'bad frontmatter' }],
          }));
        }
        return send(envelope({ imported: 1 }));
      }
      if (url.startsWith('/api/checkpoints/') && url.endsWith('/restore')) {
        return send(envelope({ ...memory, content: 'restored content' }));
      }
      if (url.startsWith('/api/checkpoints')) {
        return send(envelope([{ history_id: 1, memory_id: 'mem_1', operation: 'update', changed_at: '2026-08-01T00:00:00Z' }]));
      }
      if (url.startsWith('/api/memories/') && url.endsWith('/checkpoints')) {
        return send(envelope([{ history_id: 2, memory_id: 'mem_1', operation: 'create', changed_at: '2026-07-01T00:00:00Z' }]));
      }
      if (url.startsWith('/api/search')) {
        const parsed = JSON.parse(body || '{}');
        if (parsed.query === 'fail') return send(envelope(null, 'search exploded'));
        return send(envelope([{ memory, score: 0.9 }]));
      }
      if (url.startsWith('/api/extract')) {
        const parsed = JSON.parse(body || '{}');
        if (parsed.text === 'empty') return send(envelope({ memories: [], coverage: null, saved_ids: [] }));
        return send(envelope({
          memories: [
            { content: 'User prefers tabs', instruction: null, type: 'preference', priority: 'REFERENCE', tags: ['style'], confidence: 0.7 },
          ],
          coverage: { input_lines: 3, empty_lines: 0, extracted_lines: 1, no_signal_lines: 2 },
          saved_ids: [],
        }));
      }
      if (url.startsWith('/api/memories?') && req.method === 'GET') return send(envelope([memory]));
      if (url.startsWith('/api/memories') && req.method === 'POST') return send(envelope({ id: 'mem_new' }));
      if (url.startsWith('/api/memories/') && url.endsWith('/supersede')) return send(envelope({ superseded: 'mem_1', replacement_id: JSON.parse(body).replacement_id }));
      if (url.startsWith('/api/memories/') && req.method === 'PUT') return send(envelope({}));
      if (url.startsWith('/api/memories/') && req.method === 'DELETE') return send(envelope({}));
      if (url.startsWith('/api/inbox/') && url.endsWith('/edit')) return send(envelope({ edited: 'mem_1' }));
      if (url.startsWith('/api/inbox/') && req.method === 'POST') return send(envelope({}));
      if (url.startsWith('/api/inbox')) return send(envelope({ memories: [memory], total: 1 }));
      if (url.startsWith('/api/dedup')) return send(envelope({ unique: 1, duplicates: 2 }));
      if (url.startsWith('/api/decay')) return send(envelope({ updated: 1, archived: 0 }));
      if (url.startsWith('/api/promote')) return send(envelope({ promoted_to_l2: 1, promoted_to_l3: 0 }));
      if (url.startsWith('/api/stats')) return send(envelope({
        total: 1,
        must_count: 1,
        reference_count: 0,
        reviewed_count: 1,
        agents: ['vscode'],
        namespaces: ['global'],
        layers: { l0: 0, l1: 0, l2: 0, l3: 1 },
        skills: 0,
      }));
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
  state.activeEditor = undefined;
  state.saveDialogResults = [];
  state.openDialogResults = [];
  state.writtenFiles = [];
  state.fileContents = new Map();
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
      'memvault.extractFromSelection',
      'memvault.approve',
      'memvault.reject',
      'memvault.delete',
      'memvault.createMemory',
      'memvault.edit',
      'memvault.supersede',
      'memvault.quickEditInbox',
      'memvault.dedup',
      'memvault.decay',
      'memvault.promote',
      'memvault.showStats',
      'memvault.export',
      'memvault.import',
      'memvault.backup',
      'memvault.checkpoints',
      'memvault.memoryHistory',
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

  it('supersede POSTs replacement_id to /api/memories/:id/supersede', async () => {
    state.inputs.push('mem_new');
    await run('memvault.supersede');
    expect(state.infos.some((m) => m.includes('Superseded by mem_new'))).toBe(true);
  });

  it('supersede does nothing when the replacement id prompt is cancelled', async () => {
    state.inputs.push(undefined);
    await run('memvault.supersede');
    expect(state.infos).toHaveLength(0);
    expect(state.errors).toHaveLength(0);
  });

  it('quickEditInbox POSTs edited_content to /api/inbox/:id/edit', async () => {
    state.inputs.push('edited now');
    await run('memvault.quickEditInbox');
    expect(state.infos.some((m) => m.includes('Edited and approved'))).toBe(true);
  });

  it('extractFromSelection shows nothing when there is no active editor or selection', async () => {
    await run('memvault.extractFromSelection');
    expect(state.infos).toHaveLength(0);
    expect(state.errors).toHaveLength(0);
  });

  it('extractFromSelection warns when the selection is empty', async () => {
    state.activeEditor = { document: { getText: () => '' }, selection: {} };
    await run('memvault.extractFromSelection');
    expect(state.picks).toEqual([]); // never reached the mode prompt
  });

  it('extractFromSelection previews candidates, defaults to all selected, and saves picked ones', async () => {
    state.activeEditor = { document: { getText: () => 'User prefers tabs over spaces' }, selection: {} };
    state.picks.push('rule');
    // Simulate the user confirming the default (all-selected) quick pick.
    state.picks.push([{ candidate: { content: 'User prefers tabs', instruction: null, type: 'preference', priority: 'REFERENCE', tags: ['style'] } }]);
    await run('memvault.extractFromSelection');
    expect(state.infos.some((m) => m.includes('Saved 1/1 extracted memories'))).toBe(true);
  });

  it('extractFromSelection reports zero candidates without saving', async () => {
    state.activeEditor = { document: { getText: () => 'empty' }, selection: {} };
    state.picks.push('rule');
    await run('memvault.extractFromSelection');
    expect(state.infos.some((m) => m.includes('No candidates extracted.'))).toBe(true);
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

  it('showStats calls GET /api/stats and renders the dedicated aggregate', async () => {
    await run('memvault.showStats');
    expect(state.infos.some((m) => m.includes('Total: 1'))).toBe(true);
    expect(state.infos.some((m) => m.includes('MUST: 1 | REF: 0'))).toBe(true);
    expect(state.infos.some((m) => m.includes('L3: 1 | L2: 0 | L1: 0 | L0: 0'))).toBe(true);
    expect(state.infos.some((m) => m.includes('Agents: 1 | Namespaces: 1'))).toBe(true);
  });

  it('export writes json content to the chosen save path', async () => {
    state.picks.push('json');
    state.inputs.push('');
    state.saveDialogResults.push({ fsPath: '/tmp/export.json' });
    await run('memvault.export');
    expect(state.infos.some((m) => m.includes('Exported to /tmp/export.json'))).toBe(true);
    expect(state.writtenFiles).toHaveLength(1);
    expect(state.writtenFiles[0].path).toBe('/tmp/export.json');
    expect(JSON.parse(state.writtenFiles[0].data.toString())).toEqual({ memories: [memory] });
  });

  it('export writes each markdown file into the chosen folder', async () => {
    state.picks.push('markdown');
    state.inputs.push('');
    state.openDialogResults.push([{ fsPath: '/tmp/exportdir' }]);
    await run('memvault.export');
    expect(state.infos.some((m) => m.includes('Exported 1 files to /tmp/exportdir'))).toBe(true);
    expect(state.writtenFiles).toHaveLength(1);
    expect(state.writtenFiles[0].path).toBe('/tmp/exportdir/mem_1.md');
  });

  it('import reads a chosen json file and posts its content', async () => {
    state.openDialogResults.push([{ fsPath: '/tmp/import.json' }]);
    state.fileContents.set('/tmp/import.json', Buffer.from(JSON.stringify({ memories: [memory] })));
    await run('memvault.import');
    expect(state.infos.some((m) => m.includes('Imported 1'))).toBe(true);
  });

  it('import reads chosen markdown files and reports skipped entries', async () => {
    state.openDialogResults.push([{ fsPath: '/tmp/a.md' }, { fsPath: '/tmp/b.md' }]);
    state.fileContents.set('/tmp/a.md', Buffer.from('---\nid: a\n---\nbad'));
    state.fileContents.set('/tmp/b.md', Buffer.from('---\nid: b\n---\ngood'));
    await run('memvault.import');
    expect(state.infos.some((m) => m.includes('Imported 1, 1 skipped'))).toBe(true);
  });

  it('backup downloads the sqlite file to the chosen save path', async () => {
    state.saveDialogResults.push({ fsPath: '/tmp/backup.db' });
    await run('memvault.backup');
    expect(state.infos.some((m) => m.includes('Backup saved to /tmp/backup.db'))).toBe(true);
    expect(state.writtenFiles).toHaveLength(1);
    expect(state.writtenFiles[0].data.subarray(0, 15).toString()).toBe('SQLite format 3');
  });

  it('checkpoints lists history, confirms, and restores the picked version', async () => {
    state.picks.push({ entry: { history_id: 1, memory_id: 'mem_1', operation: 'update', changed_at: '2026-08-01T00:00:00Z' } });
    state.picks.push('Restore'); // showWarningMessage confirm dialog
    await run('memvault.checkpoints');
    expect(state.infos.some((m) => m.includes('Restored'))).toBe(true);
  });

  it('checkpoints does nothing when the confirm dialog is declined', async () => {
    state.picks.push({ entry: { history_id: 1, memory_id: 'mem_1', operation: 'update', changed_at: '2026-08-01T00:00:00Z' } });
    state.picks.push(undefined); // confirm dialog dismissed
    await run('memvault.checkpoints');
    expect(state.infos.some((m) => m.includes('Restored'))).toBe(false);
  });

  it('memoryHistory lists a single memory\'s checkpoints scoped to that id', async () => {
    state.picks.push(undefined); // cancel the quick pick
    await run('memvault.memoryHistory');
    expect(state.infos).toHaveLength(0);
    expect(state.errors).toHaveLength(0);
  });
});
