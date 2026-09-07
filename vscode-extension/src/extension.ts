import * as vscode from 'vscode';
import * as http from 'http';
import * as https from 'https';
import {
  Memory,
  DashboardStats,
  ExtractedCandidate,
  ExtractCoverage,
  CheckpointEntry,
  priorityIcon,
  treeItemLabel,
  treeItemDescription,
  treeItemTooltipLines,
  formatMemoryDetail,
  formatStatsMessage,
  extractedCandidateLabel,
  formatCoverageMessage,
  checkpointLabel,
} from './format';

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

/// Parallel to `apiRequest` but for endpoints that return a raw file stream
/// (e.g. `/api/backup`) instead of the `{ ok, data, error }` JSON envelope —
/// buffers response bytes instead of decoding them as a UTF-8 string.
async function apiRequestBinary(method: string, path: string): Promise<{ buffer: Buffer; headers: http.IncomingHttpHeaders }> {
  const url = `${getServerUrl()}${path}`;
  const parsed = new URL(url);
  const lib = parsed.protocol === 'https:' ? https : http;

  const headers: Record<string, string> = {};
  const apiKey = getApiKey();
  if (apiKey) headers['X-MemVault-Api-Key'] = apiKey;

  return new Promise((resolve, reject) => {
    const options = {
      hostname: parsed.hostname,
      port: parsed.port,
      path: parsed.pathname + parsed.search,
      method,
      headers,
    };

    const req = lib.request(options, (res) => {
      const chunks: Buffer[] = [];
      res.on('data', (chunk) => chunks.push(chunk as Buffer));
      res.on('end', () => {
        const buffer = Buffer.concat(chunks);
        if ((res.statusCode ?? 200) >= 400) {
          try {
            const errorBody = JSON.parse(buffer.toString('utf8'));
            reject(new Error(errorBody.error || 'MemVault API error'));
          } catch {
            reject(new Error(`MemVault API error (status ${res.statusCode})`));
          }
          return;
        }
        resolve({ buffer, headers: res.headers });
      });
    });
    req.on('error', reject);
    req.end();
  });
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

/** Shared by the global "Checkpoints" command and the per-item "History"
 * menu entry: lists checkpoint entries, lets the user pick one, confirms
 * (the only destructive-with-confirmation flow in the data batch, mirroring
 * the Web Dashboard's checkpoint restore), then restores it. */
async function pickAndRestoreCheckpoint(entries: CheckpointEntry[], refresh: () => void): Promise<void> {
  if (!entries.length) {
    vscode.window.showInformationMessage('No checkpoint history.');
    return;
  }
  const items = entries.map((entry) => ({
    label: checkpointLabel(entry),
    description: entry.memory_id,
    entry,
  }));
  const picked = await vscode.window.showQuickPick(items, { placeHolder: 'Select a version to restore' });
  if (!picked) return;

  const confirm = await vscode.window.showWarningMessage(
    'Restore this version? The current content will be replaced (and itself saved to history).',
    { modal: true },
    'Restore',
  );
  if (confirm !== 'Restore') return;

  try {
    await apiRequest('POST', `/api/checkpoints/${picked.entry.history_id}/restore`);
    vscode.window.showInformationMessage('Restored');
    refresh();
  } catch (e: any) {
    vscode.window.showErrorMessage(`Restore failed: ${e.message}`);
  }
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

    vscode.commands.registerCommand('memvault.extractFromSelection', async () => {
      const editor = vscode.window.activeTextEditor;
      if (!editor) return;
      const text = editor.document.getText(editor.selection);
      if (!text) { vscode.window.showWarningMessage('No text selected'); return; }

      const mode = await vscode.window.showQuickPick(['rule', 'llm'], { placeHolder: 'Extraction mode' });
      if (!mode) return;

      try {
        const result: { memories: ExtractedCandidate[]; coverage: ExtractCoverage | null; saved_ids: string[] } =
          await apiRequest('POST', '/api/extract', { text, mode, auto_save: false });

        if (!result.memories.length) {
          vscode.window.showInformationMessage('No candidates extracted.');
          return;
        }

        const items = result.memories.map((c) => ({
          label: extractedCandidateLabel(c),
          description: `confidence: ${c.confidence.toFixed(2)} · ${c.tags.join(', ')}`,
          detail: c.instruction || undefined,
          picked: true,
          candidate: c,
        }));

        const selected = await vscode.window.showQuickPick(items, {
          canPickMany: true,
          placeHolder: result.coverage ? formatCoverageMessage(result.coverage) : 'Select candidates to save',
        });
        if (!selected || !selected.length) return;

        let saved = 0;
        for (const item of selected) {
          try {
            await apiRequest('POST', '/api/memories', {
              content: item.candidate.content,
              instruction: item.candidate.instruction,
              priority: item.candidate.priority,
              type: item.candidate.type,
              tags: item.candidate.tags,
              agent_id: 'vscode',
              agent_type: 'ide-editor',
              namespace: 'global',
            });
            saved++;
          } catch {
            // keep saving the rest even if one candidate fails
          }
        }
        vscode.window.showInformationMessage(`Saved ${saved}/${selected.length} extracted memories`);
        memProvider.refresh();
      } catch (e: any) {
        vscode.window.showErrorMessage(`Extract failed: ${e.message}`);
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

    vscode.commands.registerCommand('memvault.supersede', async (mem: Memory) => {
      const replacementId = await vscode.window.showInputBox({
        prompt: `Replacement memory id for "${mem.content.slice(0, 40)}..."`,
        placeHolder: 'mem_...',
        ignoreFocusOut: true,
      });
      if (!replacementId) return;
      try {
        await apiRequest('POST', `/api/memories/${mem.id}/supersede`, { replacement_id: replacementId });
        vscode.window.showInformationMessage(`Superseded by ${replacementId}`);
        memProvider.refresh();
      } catch (e: any) {
        vscode.window.showErrorMessage(`Supersede failed: ${e.message}`);
      }
    }),

    vscode.commands.registerCommand('memvault.quickEditInbox', async (mem: Memory) => {
      const editedContent = await vscode.window.showInputBox({
        prompt: 'Edit content (approves and leaves the review inbox)',
        value: mem.content,
        ignoreFocusOut: true,
      });
      if (editedContent === undefined) return;
      try {
        await apiRequest('POST', `/api/inbox/${mem.id}/edit`, { edited_content: editedContent });
        vscode.window.showInformationMessage('Edited and approved');
        inboxProvider.refresh();
        memProvider.refresh();
      } catch (e: any) {
        vscode.window.showErrorMessage(`Quick edit failed: ${e.message}`);
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
        const stats: DashboardStats = await apiRequest('GET', '/api/stats');
        vscode.window.showInformationMessage(formatStatsMessage(stats));
      } catch (e: any) {
        vscode.window.showErrorMessage(`Stats: ${e.message}`);
      }
    }),

    vscode.commands.registerCommand('memvault.export', async () => {
      const format = await vscode.window.showQuickPick(['json', 'markdown'], { placeHolder: 'Export format' });
      if (!format) return;
      const namespace = await vscode.window.showInputBox({
        prompt: 'Namespace filter (optional, leave empty for all)',
        ignoreFocusOut: true,
      });
      if (namespace === undefined) return;

      try {
        const query = `?format=${format}${namespace ? `&namespace=${encodeURIComponent(namespace)}` : ''}`;
        const result = await apiRequest('GET', `/api/export${query}`);
        if (result.format === 'json') {
          const uri = await vscode.window.showSaveDialog({
            defaultUri: vscode.Uri.file(`memvault-export-${namespace || 'all'}.json`),
            filters: { JSON: ['json'] },
          });
          if (!uri) return;
          await vscode.workspace.fs.writeFile(uri, Buffer.from(result.content, 'utf8'));
          vscode.window.showInformationMessage(`Exported to ${uri.fsPath}`);
        } else {
          const folders = await vscode.window.showOpenDialog({
            canSelectFolders: true,
            canSelectFiles: false,
            canSelectMany: false,
            openLabel: 'Export here',
          });
          if (!folders || !folders.length) return;
          const files: { filename: string; content: string }[] = result.files;
          for (const file of files) {
            await vscode.workspace.fs.writeFile(
              vscode.Uri.joinPath(folders[0], file.filename),
              Buffer.from(file.content, 'utf8'),
            );
          }
          vscode.window.showInformationMessage(`Exported ${files.length} files to ${folders[0].fsPath}`);
        }
      } catch (e: any) {
        vscode.window.showErrorMessage(`Export failed: ${e.message}`);
      }
    }),

    vscode.commands.registerCommand('memvault.import', async () => {
      const uris = await vscode.window.showOpenDialog({
        canSelectMany: true,
        filters: { 'MemVault export': ['json', 'md'] },
      });
      if (!uris || !uris.length) return;

      try {
        const isJson = uris[0].fsPath.toLowerCase().endsWith('.json');
        let result: { imported: number; skipped?: { filename: string; reason: string }[] };
        if (isJson) {
          const bytes = await vscode.workspace.fs.readFile(uris[0]);
          result = await apiRequest('POST', '/api/import', {
            format: 'json',
            content: Buffer.from(bytes).toString('utf8'),
          });
        } else {
          const files = [];
          for (const uri of uris) {
            const bytes = await vscode.workspace.fs.readFile(uri);
            files.push({
              filename: uri.fsPath.split(/[\\/]/).pop() ?? uri.fsPath,
              content: Buffer.from(bytes).toString('utf8'),
            });
          }
          result = await apiRequest('POST', '/api/import', { format: 'markdown', files });
        }
        const skippedMsg = result.skipped?.length ? `, ${result.skipped.length} skipped` : '';
        vscode.window.showInformationMessage(`Imported ${result.imported}${skippedMsg}`);
        memProvider.refresh();
        inboxProvider.refresh();
      } catch (e: any) {
        vscode.window.showErrorMessage(`Import failed: ${e.message}`);
      }
    }),

    vscode.commands.registerCommand('memvault.backup', async () => {
      try {
        const { buffer, headers } = await apiRequestBinary('POST', '/api/backup');
        const disposition = String(headers['content-disposition'] ?? '');
        const match = disposition.match(/filename="([^"]+)"/);
        const filename = match ? match[1] : 'memvault-backup.db';
        const uri = await vscode.window.showSaveDialog({ defaultUri: vscode.Uri.file(filename) });
        if (!uri) return;
        await vscode.workspace.fs.writeFile(uri, buffer);
        vscode.window.showInformationMessage(`Backup saved to ${uri.fsPath}`);
      } catch (e: any) {
        vscode.window.showErrorMessage(`Backup failed: ${e.message}`);
      }
    }),

    vscode.commands.registerCommand('memvault.checkpoints', async () => {
      try {
        const entries: CheckpointEntry[] = await apiRequest('GET', '/api/checkpoints');
        await pickAndRestoreCheckpoint(entries, () => memProvider.refresh());
      } catch (e: any) {
        vscode.window.showErrorMessage(`Checkpoints failed: ${e.message}`);
      }
    }),

    vscode.commands.registerCommand('memvault.memoryHistory', async (mem: Memory) => {
      try {
        const entries: CheckpointEntry[] = await apiRequest('GET', `/api/memories/${mem.id}/checkpoints`);
        await pickAndRestoreCheckpoint(entries, () => memProvider.refresh());
      } catch (e: any) {
        vscode.window.showErrorMessage(`History failed: ${e.message}`);
      }
    }),
  );
}

export function deactivate() {}
