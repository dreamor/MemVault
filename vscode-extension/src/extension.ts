import * as vscode from 'vscode';
import { execFile } from 'child_process';
import { promisify } from 'util';

const exec = promisify(execFile);

function getCliPath(): string {
  return vscode.workspace.getConfiguration('memvault').get('cliPath', 'memvault-cli');
}

function getDbPath(): string {
  const home = process.env.HOME || '~';
  return vscode.workspace.getConfiguration('memvault').get('dbPath', `${home}/.memvault/data.db`);
}

async function runCli(...args: string[]): Promise<string> {
  const { stdout } = await exec(getCliPath(), ['--db', getDbPath(), ...args]);
  return stdout;
}

interface MemoryItem {
  id: string;
  priority: string;
  content: string;
  reviewed: boolean;
}

function parseListOutput(output: string): MemoryItem[] {
  const items: MemoryItem[] = [];
  for (const line of output.split('\n')) {
    const match = line.match(/^\[(\w+)\]\s+(mem_\w+)\s+—\s+(.+?)(\s+✓)?$/);
    if (match) {
      items.push({
        priority: match[1],
        id: match[2],
        content: match[3],
        reviewed: !!match[4],
      });
    }
  }
  return items;
}

class MemoryTreeProvider implements vscode.TreeDataProvider<MemoryItem> {
  private _onDidChange = new vscode.EventEmitter<MemoryItem | undefined>();
  readonly onDidChangeTreeData = this._onDidChange.event;
  private reviewOnly: boolean;

  constructor(reviewOnly: boolean) {
    this.reviewOnly = reviewOnly;
  }

  refresh() { this._onDidChange.fire(undefined); }

  async getChildren(): Promise<MemoryItem[]> {
    try {
      const output = await runCli('list', '--limit', '100');
      let items = parseListOutput(output);
      if (this.reviewOnly) {
        items = items.filter(m => !m.reviewed);
      }
      return items;
    } catch {
      return [];
    }
  }

  getTreeItem(item: MemoryItem): vscode.TreeItem {
    const ti = new vscode.TreeItem(item.content, vscode.TreeItemCollapsibleState.None);
    ti.description = `[${item.priority}]`;
    ti.tooltip = `${item.id}\nPriority: ${item.priority}\nReviewed: ${item.reviewed}`;
    ti.contextValue = item.reviewed ? 'reviewed' : 'pending';
    return ti;
  }
}

export function activate(context: vscode.ExtensionContext) {
  const memProvider = new MemoryTreeProvider(false);
  const reviewProvider = new MemoryTreeProvider(true);

  vscode.window.registerTreeDataProvider('memvault.memories', memProvider);
  vscode.window.registerTreeDataProvider('memvault.review', reviewProvider);

  context.subscriptions.push(
    vscode.commands.registerCommand('memvault.listMemories', async () => {
      try {
        const output = await runCli('list', '--limit', '50');
        const doc = await vscode.workspace.openTextDocument({ content: output, language: 'text' });
        await vscode.window.showTextDocument(doc);
      } catch (e: any) {
        vscode.window.showErrorMessage(`MemVault: ${e.message}`);
      }
    }),

    vscode.commands.registerCommand('memvault.searchMemories', async () => {
      const query = await vscode.window.showInputBox({ prompt: 'Search memories', placeHolder: 'e.g. Python preference' });
      if (!query) return;
      try {
        const output = await runCli('search', '--query', query);
        const doc = await vscode.workspace.openTextDocument({ content: output, language: 'text' });
        await vscode.window.showTextDocument(doc);
      } catch (e: any) {
        vscode.window.showErrorMessage(`MemVault: ${e.message}`);
      }
    }),

    vscode.commands.registerCommand('memvault.saveSelection', async () => {
      const editor = vscode.window.activeTextEditor;
      if (!editor) return;

      const selection = editor.document.getText(editor.selection);
      if (!selection) return;

      const priority = await vscode.window.showQuickPick(['REFERENCE', 'MUST', 'BACKGROUND'], { placeHolder: 'Priority' });
      if (!priority) return;

      const type = await vscode.window.showQuickPick(['preference', 'fact', 'episode', 'skill'], { placeHolder: 'Type' });
      if (!type) return;

      try {
        const output = await runCli('save', '--content', selection, '--priority', priority, '--type', type, '--agent-id', 'vscode');
        vscode.window.showInformationMessage(output.trim());
        memProvider.refresh();
        reviewProvider.refresh();
      } catch (e: any) {
        vscode.window.showErrorMessage(`MemVault: ${e.message}`);
      }
    }),

    vscode.commands.registerCommand('memvault.showStats', async () => {
      try {
        const output = await runCli('list', '--limit', '10000');
        const items = parseListOutput(output);
        const must = items.filter(m => m.priority === 'Must').length;
        const ref_ = items.filter(m => m.priority === 'Reference').length;
        const reviewed = items.filter(m => m.reviewed).length;
        const msg = `Total: ${items.length} | MUST: ${must} | REF: ${ref_} | Reviewed: ${reviewed}`;
        vscode.window.showInformationMessage(msg);
      } catch (e: any) {
        vscode.window.showErrorMessage(`MemVault: ${e.message}`);
      }
    }),
  );
}

export function deactivate() {}
