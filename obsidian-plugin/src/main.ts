import { App, Plugin, PluginSettingTab, Setting, ItemView, WorkspaceLeaf, Notice } from 'obsidian';
import { execFile } from 'child_process';
import { promisify } from 'util';

const exec = promisify(execFile);
const VIEW_TYPE = 'memvault-panel';

interface MemVaultSettings {
  cliPath: string;
  dbPath: string;
}

const DEFAULT_SETTINGS: MemVaultSettings = {
  cliPath: 'memvault-cli',
  dbPath: '~/.memvault/data.db',
};

export default class MemVaultPlugin extends Plugin {
  settings: MemVaultSettings = DEFAULT_SETTINGS;

  async onload() {
    await this.loadSettings();

    this.registerView(VIEW_TYPE, (leaf) => new MemVaultView(leaf, this));

    this.addRibbonIcon('database', 'MemVault', () => this.activateView());

    this.addCommand({
      id: 'open-panel',
      name: 'Open Memory Panel',
      callback: () => this.activateView(),
    });

    this.addCommand({
      id: 'search',
      name: 'Search Memories',
      callback: () => this.searchDialog(),
    });

    this.addCommand({
      id: 'save-selection',
      name: 'Save Selection as Memory',
      editorCallback: async (editor) => {
        const text = editor.getSelection();
        if (!text) { new Notice('No text selected'); return; }
        try {
          const output = await this.runCli('save', '--content', text, '--priority', 'REFERENCE', '--type', 'fact', '--agent-id', 'obsidian');
          new Notice(output.trim());
        } catch (e: any) {
          new Notice(`Error: ${e.message}`);
        }
      },
    });

    this.addCommand({
      id: 'export-markdown',
      name: 'Export Memories to Vault',
      callback: async () => {
        const vaultPath = (this.app.vault.adapter as any).basePath;
        const dir = `${vaultPath}/MemVault`;
        try {
          const output = await this.runCli('export', '--format', 'markdown', '--output', dir);
          new Notice(output.trim());
        } catch (e: any) {
          new Notice(`Export error: ${e.message}`);
        }
      },
    });

    this.addCommand({
      id: 'import-markdown',
      name: 'Import Memories from Vault',
      callback: async () => {
        const vaultPath = (this.app.vault.adapter as any).basePath;
        const dir = `${vaultPath}/MemVault`;
        try {
          const output = await this.runCli('import', '--format', 'markdown', '--input', dir);
          new Notice(output.trim());
        } catch (e: any) {
          new Notice(`Import error: ${e.message}`);
        }
      },
    });

    this.addSettingTab(new MemVaultSettingTab(this.app, this));
  }

  async runCli(...args: string[]): Promise<string> {
    const { stdout } = await exec(this.settings.cliPath, ['--db', this.settings.dbPath, ...args]);
    return stdout;
  }

  async searchDialog() {
    const query = await new Promise<string | null>((resolve) => {
      const modal = new SearchModal(this.app, resolve);
      modal.open();
    });
    if (!query) return;

    try {
      const output = await this.runCli('search', '--query', query);
      const leaf = this.app.workspace.getLeaf(true);
      await leaf.setViewState({ type: 'markdown', state: {} });
      const file = await this.app.vault.create(
        `MemVault/search-${Date.now()}.md`,
        `# MemVault Search: ${query}\n\n\`\`\`\n${output}\n\`\`\``
      );
      await leaf.openFile(file);
    } catch (e: any) {
      new Notice(`Search error: ${e.message}`);
    }
  }

  async activateView() {
    const existing = this.app.workspace.getLeavesOfType(VIEW_TYPE);
    if (existing.length) {
      this.app.workspace.revealLeaf(existing[0]);
      return;
    }
    const leaf = this.app.workspace.getRightLeaf(false);
    if (leaf) {
      await leaf.setViewState({ type: VIEW_TYPE, active: true });
      this.app.workspace.revealLeaf(leaf);
    }
  }

  async loadSettings() {
    this.settings = Object.assign({}, DEFAULT_SETTINGS, await this.loadData());
  }

  async saveSettings() {
    await this.saveData(this.settings);
  }
}

class SearchModal {
  app: App;
  resolve: (value: string | null) => void;

  constructor(app: App, resolve: (value: string | null) => void) {
    this.app = app;
    this.resolve = resolve;
  }

  open() {
    const query = prompt('Search memories:');
    this.resolve(query);
  }
}

class MemVaultView extends ItemView {
  plugin: MemVaultPlugin;

  constructor(leaf: WorkspaceLeaf, plugin: MemVaultPlugin) {
    super(leaf);
    this.plugin = plugin;
  }

  getViewType() { return VIEW_TYPE; }
  getDisplayText() { return 'MemVault'; }
  getIcon() { return 'database'; }

  async onOpen() {
    const container = this.containerEl.children[1];
    container.empty();
    container.createEl('h3', { text: 'MemVault Memories' });

    const refreshBtn = container.createEl('button', { text: 'Refresh' });
    refreshBtn.onclick = () => this.loadMemories(container);

    await this.loadMemories(container);
  }

  async loadMemories(container: Element) {
    const list = container.querySelector('.memory-list');
    if (list) list.remove();

    const div = container.createEl('div', { cls: 'memory-list' });

    try {
      const output = await this.plugin.runCli('list', '--limit', '50');

      for (const line of output.split('\n')) {
        if (line.startsWith('[')) {
          const el = div.createEl('div', { cls: 'memory-item', text: line });
          el.style.padding = '4px 0';
          el.style.borderBottom = '1px solid var(--background-modifier-border)';
          el.style.fontSize = '12px';
          el.style.cursor = 'pointer';
        }
      }

      if (!div.childElementCount) {
        div.createEl('p', { text: 'No memories stored.', cls: 'mod-muted' });
      }
    } catch (e: any) {
      div.createEl('p', { text: `Error: ${e.message}`, cls: 'mod-warning' });
    }
  }
}

class MemVaultSettingTab extends PluginSettingTab {
  plugin: MemVaultPlugin;

  constructor(app: App, plugin: MemVaultPlugin) {
    super(app, plugin);
    this.plugin = plugin;
  }

  display() {
    const { containerEl } = this;
    containerEl.empty();
    containerEl.createEl('h2', { text: 'MemVault Settings' });

    new Setting(containerEl)
      .setName('CLI Path')
      .setDesc('Path to memvault-cli binary')
      .addText(text => text
        .setPlaceholder('memvault-cli')
        .setValue(this.plugin.settings.cliPath)
        .onChange(async (value) => {
          this.plugin.settings.cliPath = value;
          await this.plugin.saveSettings();
        }));

    new Setting(containerEl)
      .setName('Database Path')
      .setDesc('Path to MemVault database')
      .addText(text => text
        .setPlaceholder('~/.memvault/data.db')
        .setValue(this.plugin.settings.dbPath)
        .onChange(async (value) => {
          this.plugin.settings.dbPath = value;
          await this.plugin.saveSettings();
        }));
  }
}
