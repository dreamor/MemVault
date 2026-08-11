import * as vscode from 'vscode';
import * as http from 'http';
import * as https from 'https';

// ─── API Client ──────────────────────────────────────────────────

function getServerUrl(): string {
  return vscode.workspace.getConfiguration('memvault').get('serverUrl', 'http://127.0.0.1:8080');
}

async function apiRequest(method: string, path: string, body?: any): Promise<any> {
  const url = `${getServerUrl()}${path}`;
  const parsed = new URL(url);
  const lib = parsed.protocol === 'https:' ? https : http;

  return new Promise((resolve, reject) => {
    const options = {
      hostname: parsed.hostname,
      port: parsed.port,
      path: parsed.pathname + parsed.search,
      method,
      headers: body ? { 'Content-Type': 'application/json' } : {},
    };

    const req = lib.request(options, (res) => {
      let data = '';
      res.on('data', (chunk) => data += chunk);
      res.on('end', () => {
        try {
          resolve(JSON.parse(data));
        } catch {
          resolve(data);
        }
      });
    });
    req.on('error', reject);
    if (body) req.write(JSON.stringify(body));
    req.end();
  });
}

// ─── Types ───────────────────────────────────────────────────────

interface Memory {
  id: string;
  memory_type: string;
  content: string;
  instruction: string | null;
  priority: string;
  namespace: string;
  tags: string[];
  layer: string;
  skill_meta: { trigger: string | null; steps: string[]; verification: string | null; version: number } | null;
  access_count: number;
  human_reviewed: boolean;
  created_at: string;
}

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
        return await apiRequest('GET', '/api/inbox');
      }
      return await apiRequest('GET', '/api/memories?limit=100');
    } catch {
      return [];
    }
  }

  getTreeItem(mem: Memory): vscode.TreeItem {
    const icon = mem.priority === 'MUST' ? '🔴' : mem.priority === 'REFERENCE' ? '🔵' : '⚪';
    const label = `${icon} ${mem.content.slice(0, 60)}`;
    const ti = new vscode.TreeItem(label, vscode.TreeItemCollapsibleState.None);
    ti.description = `[${mem.layer}] ${mem.memory_type}`;
    
    const lines = [
      `ID: ${mem.id}`,
      `Priority: ${mem.priority} | Layer: ${mem.layer}`,
      `Type: ${mem.memory_type}`,
      `Tags: ${mem.tags.join(', ') || 'none'}`,
      `Namespace: ${mem.namespace}`,
      `Access: ${mem.access_count} | Reviewed: ${mem.human_reviewed}`,
    ];
    if (mem.instruction) lines.push(`Instruction: ${mem.instruction}`);
    if (mem.skill_meta) {
      lines.push(`Skill trigger: ${mem.skill_meta.trigger || 'none'}`);
      lines.push(`Skill steps: ${mem.skill_meta.steps.join(' → ') || 'none'}`);
      if (mem.skill_meta.verification) lines.push(`Verification: ${mem.skill_meta.verification}`);
    }
    ti.tooltip = new vscode.MarkdownString(lines.map(l => `- ${l}`).join('\n'));
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
        label: `${r.memory.priority === 'MUST' ? '🔴' : '🔵'} [${r.memory.layer}] ${r.memory.content.slice(0, 70)}`,
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

function formatMemoryDetail(mem: Memory): string {
  let text = `# Memory: ${mem.id}\n\n`;
  text += `priority: ${mem.priority}\n`;
  text += `layer: ${mem.layer}\n`;
  text += `type: ${mem.memory_type}\n`;
  text += `namespace: ${mem.namespace}\n`;
  text += `tags: [${mem.tags.join(', ')}]\n`;
  text += `reviewed: ${mem.human_reviewed}\n`;
  text += `access_count: ${mem.access_count}\n`;
  text += `created_at: ${mem.created_at}\n\n`;
  text += `## Content\n${mem.content}\n`;
  if (mem.instruction) text += `\n## Instruction\n${mem.instruction}\n`;
  if (mem.skill_meta) {
    text += `\n## Skill Meta\n`;
    text += `trigger: ${mem.skill_meta.trigger || 'none'}\n`;
    text += `steps:\n${mem.skill_meta.steps.map((s, i) => `  ${i + 1}. ${s}`).join('\n')}\n`;
    if (mem.skill_meta.verification) text += `verification: ${mem.skill_meta.verification}\n`;
    text += `version: ${mem.skill_meta.version}\n`;
  }
  return text;
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
