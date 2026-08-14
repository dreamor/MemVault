import * as vscode from 'vscode';
import * as http from 'http';
import * as https from 'https';
import { Memory, priorityIcon, treeItemLabel, treeItemDescription, treeItemTooltipLines, formatMemoryDetail } from './format';

// ─── API Client ──────────────────────────────────────────────────

function getServerUrl(): string {
  return vscode.workspace.getConfiguration('memvault').get('serverUrl', 'http://127.0.0.1:8080');
}

function getApiKey(): string | undefined {
  return vscode.workspace.getConfiguration('memvault').get('apiKey', '') || undefined;
}

/// Every REST response is wrapped as `{ ok, data, error }` — unwrap `data` here
/// so every caller below just gets the real payload, and throw on `ok: false`
/// so callers can rely on try/catch instead of checking `ok` themselves.
async function apiRequest(method: string, path: string, body?: any): Promise<any> {
  const url = `${getServerUrl()}${path}`;
  const parsed = new URL(url);
  const lib = parsed.protocol === 'https:' ? https : http;

  const headers: Record<string, string> = {};
  if (body) headers['Content-Type'] = 'application/json';
  const apiKey = getApiKey();
  if (apiKey) headers['X-MemVault-Api-Key'] = apiKey;

  const responseText: string = await new Promise((resolve, reject) => {
    const options = {
      hostname: parsed.hostname,
      port: parsed.port,
      path: parsed.pathname + parsed.search,
      method,
      headers,
    };

    const req = lib.request(options, (res) => {
      let data = '';
      res.on('data', (chunk) => data += chunk);
      res.on('end', () => resolve(data));
    });
    req.on('error', reject);
    if (body) req.write(JSON.stringify(body));
    req.end();
  });

  let parsed_body: any;
  try {
    parsed_body = JSON.parse(responseText);
  } catch {
    return responseText;
  }
  if (parsed_body && typeof parsed_body === 'object' && 'ok' in parsed_body) {
    if (!parsed_body.ok) {
      throw new Error(parsed_body.error || 'MemVault API error');
    }
    return parsed_body.data;
  }
  return parsed_body;
}

// ─── Types ───────────────────────────────────────────────────────

interface SearchResult {
  memory: Memory;
  score: number;
}

// ─── Tree Provider ───────────────────────────────────────────────

class MemoryTreeProvider implements vscode.TreeDataProvider<Memory> {
  private _onDidChange = new vscode.EventEmitter<Memory | undefined>();
  readonly onDidChangeTreeData = this._onDidChange.event;
  private mode: 'all' | 'inbox';

  constructor(mode: 'all' | 'inbox') {
    this.mode = mode;
  }

  refresh() { this._onDidChange.fire(undefined); }

  async getChildren(): Promise<Memory[]> {
    try {
      if (this.mode === 'inbox') {
        const inbox: { memories: Memory[]; total: number } = await apiRequest('GET', '/api/inbox');
        return inbox.memories;
      }
      return await apiRequest('GET', '/api/memories?limit=100');
    } catch {
      return [];
    }
  }

  getTreeItem(mem: Memory): vscode.TreeItem {
    const ti = new vscode.TreeItem(treeItemLabel(mem), vscode.TreeItemCollapsibleState.None);
    ti.description = treeItemDescription(mem);
    ti.tooltip = new vscode.MarkdownString(treeItemTooltipLines(mem).map(l => `- ${l}`).join('\n'));
    ti.contextValue = mem.human_reviewed ? 'reviewed' : 'pending';
    return ti;
  }
}

// ─── Search Quick Pick ───────────────────────────────────────────

class MemoryQuickPick {
  static async search(insertMode: boolean) {
    const query = await vscode.window.showInputBox({
      prompt: 'Search memories',
      placeHolder: 'e.g. Python preference',
    });
    if (!query) return;

    try {
      const results: SearchResult[] = await apiRequest('POST', '/api/search', { query, top_k: 20 });
      if (!results.length) {
        vscode.window.showInformationMessage('No memories found.');
        return;
      }

      const items = results.map(r => ({
        label: `${priorityIcon(r.memory.priority)} [${r.memory.layer}] ${r.memory.content.slice(0, 70)}`,
        description: `score: ${r.score.toFixed(2)} · ${r.memory.tags.join(', ')}`,
        detail: r.memory.instruction || undefined,
        memory: r.memory,
      }));

      const selected = await vscode.window.showQuickPick(items, {
        placeHolder: insertMode ? 'Select memory to insert' : 'Select memory to view',
        matchOnDescription: true,
        matchOnDetail: true,
      });

      if (!selected) return;

      if (insertMode) {
        const editor = vscode.window.activeTextEditor;
        if (editor) {
          const text = selected.memory.instruction || selected.memory.content;
          editor.edit(b => b.replace(editor.selection, text));
        }
      } else {
        const doc = await vscode.workspace.openTextDocument({
          content: formatMemoryDetail(selected.memory),
          language: 'yaml',
        });
        await vscode.window.showTextDocument(doc, { preview: true });
      }
    } catch (e: any) {
      vscode.window.showErrorMessage(`MemVault search: ${e.message}`);
    }
  }
}

// ─── Create / Edit form (sequential prompts) ──────────────────────

interface MemoryFormResult {
  content: string;
  instruction?: string;
  priority: string;
  type: string;
  namespace: string;
  tags: string[];
}

/** Walks the user through content/priority/type/namespace/tags via sequential
 * QuickPick/InputBox prompts. Returns undefined if cancelled at any step. */
async function promptMemoryForm(initial?: Memory): Promise<MemoryFormResult | undefined> {
  const content = await vscode.window.showInputBox({
    prompt: 'Memory content',
    value: initial?.content ?? '',
    ignoreFocusOut: true,
  });
  if (!content) return undefined;

  const priority = await vscode.window.showQuickPick(['MUST', 'REFERENCE', 'BACKGROUND'], {
    placeHolder: 'Priority',
  });
  if (!priority) return undefined;

  const type = await vscode.window.showQuickPick(
    ['preference', 'fact', 'episode', 'entity', 'skill'],
    { placeHolder: 'Memory type' },
  );
  if (!type) return undefined;

  const instruction = await vscode.window.showInputBox({
    prompt: 'Instruction (optional)',
    value: initial?.instruction ?? '',
    ignoreFocusOut: true,
  });

  const namespace = await vscode.window.showInputBox({
    prompt: 'Namespace',
    value: initial?.namespace ?? 'global',
    ignoreFocusOut: true,
  });
  if (namespace === undefined) return undefined;

  const tagsInput = await vscode.window.showInputBox({
    prompt: 'Tags (comma-separated)',
    value: initial?.tags.join(', ') ?? '',
    ignoreFocusOut: true,
  });
  if (tagsInput === undefined) return undefined;

  return {
    content,
    instruction: instruction || undefined,
    priority,
    type,
    namespace,
    tags: tagsInput.split(',').map(t => t.trim()).filter(Boolean),
  };
}

// ─── Activate ────────────────────────────────────────────────────

export function activate(context: vscode.ExtensionContext) {
  const memProvider = new MemoryTreeProvider('all');
  const inboxProvider = new MemoryTreeProvider('inbox');

  vscode.window.registerTreeDataProvider('memvault.memories', memProvider);
  vscode.window.registerTreeDataProvider('memvault.inbox', inboxProvider);

  // Auto-refresh
  const interval = vscode.workspace.getConfiguration('memvault').get('refreshInterval', 15) as number;
  if (interval > 0) {
    const timer = setInterval(() => {
      memProvider.refresh();
      inboxProvider.refresh();
    }, interval * 1000);
    context.subscriptions.push({ dispose: () => clearInterval(timer) });
  }

  context.subscriptions.push(
    vscode.commands.registerCommand('memvault.refresh', () => {
      memProvider.refresh();
      inboxProvider.refresh();
    }),

    vscode.commands.registerCommand('memvault.search', () => MemoryQuickPick.search(false)),
    vscode.commands.registerCommand('memvault.searchInsert', () => MemoryQuickPick.search(true)),

    vscode.commands.registerCommand('memvault.saveSelection', async () => {
      const editor = vscode.window.activeTextEditor;
      if (!editor) return;
      const selection = editor.document.getText(editor.selection);
      if (!selection) { vscode.window.showWarningMessage('No text selected'); return; }

      const priority = await vscode.window.showQuickPick(
        ['MUST', 'REFERENCE', 'BACKGROUND'],
        { placeHolder: 'Priority' }
      );
      if (!priority) return;

      const type = await vscode.window.showQuickPick(
        ['preference', 'fact', 'episode', 'skill'],
        { placeHolder: 'Memory type' }
      );
      if (!type) return;

      try {
        const result = await apiRequest('POST', '/api/memories', {
          content: selection,
          priority,
          type,
          agent_id: 'vscode',
          agent_type: 'ide-editor',
          namespace: 'global',
        });
        vscode.window.showInformationMessage(`Saved: ${result.id}`);
        memProvider.refresh();
      } catch (e: any) {
        vscode.window.showErrorMessage(`Save failed: ${e.message}`);
      }
    }),

    vscode.commands.registerCommand('memvault.approve', async (mem: Memory) => {
      try {
        await apiRequest('POST', `/api/inbox/${mem.id}/approve`);
        vscode.window.showInformationMessage('Approved');
        inboxProvider.refresh();
        memProvider.refresh();
      } catch (e: any) {
        vscode.window.showErrorMessage(`Approve failed: ${e.message}`);
      }
    }),

    vscode.commands.registerCommand('memvault.reject', async (mem: Memory) => {
      try {
        await apiRequest('POST', `/api/inbox/${mem.id}/reject`);
        vscode.window.showInformationMessage('Rejected');
        inboxProvider.refresh();
      } catch (e: any) {
        vscode.window.showErrorMessage(`Reject failed: ${e.message}`);
      }
    }),

    vscode.commands.registerCommand('memvault.delete', async (mem: Memory) => {
      const confirm = await vscode.window.showWarningMessage(
        `Delete memory "${mem.content.slice(0, 40)}..."?`,
        { modal: true }, 'Delete'
      );
      if (confirm !== 'Delete') return;
      try {
        await apiRequest('DELETE', `/api/memories/${mem.id}`);
        vscode.window.showInformationMessage('Deleted');
        memProvider.refresh();
      } catch (e: any) {
        vscode.window.showErrorMessage(`Delete failed: ${e.message}`);
      }
    }),

    vscode.commands.registerCommand('memvault.createMemory', async () => {
      const form = await promptMemoryForm();
      if (!form) return;
      try {
        const result = await apiRequest('POST', '/api/memories', {
          ...form,
          agent_id: 'vscode',
          agent_type: 'ide-editor',
        });
        vscode.window.showInformationMessage(`Created: ${result.id}`);
        memProvider.refresh();
      } catch (e: any) {
        vscode.window.showErrorMessage(`Create failed: ${e.message}`);
      }
    }),

    vscode.commands.registerCommand('memvault.edit', async (mem: Memory) => {
      const form = await promptMemoryForm(mem);
      if (!form) return;
      try {
        await apiRequest('PUT', `/api/memories/${mem.id}`, form);
        vscode.window.showInformationMessage('Updated');
        memProvider.refresh();
        inboxProvider.refresh();
      } catch (e: any) {
        vscode.window.showErrorMessage(`Edit failed: ${e.message}`);
      }
    }),

    vscode.commands.registerCommand('memvault.dedup', async () => {
      try {
        const r = await apiRequest('POST', '/api/dedup');
        vscode.window.showInformationMessage(`Dedup: ${r.unique} unique, ${r.duplicates} duplicates found`);
        memProvider.refresh();
      } catch (e: any) {
        vscode.window.showErrorMessage(`Dedup failed: ${e.message}`);
      }
    }),

    vscode.commands.registerCommand('memvault.decay', async () => {
      try {
        const r = await apiRequest('POST', '/api/decay');
        vscode.window.showInformationMessage(`Decay: ${r.updated} updated, ${r.archived} archived`);
        memProvider.refresh();
      } catch (e: any) {
        vscode.window.showErrorMessage(`Decay failed: ${e.message}`);
      }
    }),

    vscode.commands.registerCommand('memvault.promote', async () => {
      try {
        const r = await apiRequest('POST', '/api/promote', {});
        vscode.window.showInformationMessage(`Promote: ${r.promoted_to_l2} → L2, ${r.promoted_to_l3} → L3`);
        memProvider.refresh();
      } catch (e: any) {
        vscode.window.showErrorMessage(`Promote failed: ${e.message}`);
      }
    }),

    vscode.commands.registerCommand('memvault.showStats', async () => {
      try {
        const memories: Memory[] = await apiRequest('GET', '/api/memories?limit=10000');
        const must = memories.filter(m => m.priority === 'MUST').length;
        const ref_ = memories.filter(m => m.priority === 'REFERENCE').length;
        const l3 = memories.filter(m => m.layer === 'L3').length;
        const l2 = memories.filter(m => m.layer === 'L2').length;
        const l1 = memories.filter(m => m.layer === 'L1').length;
        const reviewed = memories.filter(m => m.human_reviewed).length;
        const skills = memories.filter(m => m.skill_meta !== null).length;

        const msg = [
          `Total: ${memories.length}`,
          `MUST: ${must} | REF: ${ref_}`,
          `L3: ${l3} | L2: ${l2} | L1: ${l1}`,
          `Skills: ${skills} | Reviewed: ${reviewed}`,
        ].join(' · ');
        vscode.window.showInformationMessage(msg);
      } catch (e: any) {
        vscode.window.showErrorMessage(`Stats: ${e.message}`);
      }
    }),
  );
}

export function deactivate() {}
