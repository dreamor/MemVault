import {
  App,
  Plugin,
  PluginSettingTab,
  Setting,
  ItemView,
  WorkspaceLeaf,
  Notice,
  SuggestModal,
  MarkdownView,
  requestUrl,
} from 'obsidian';

const VIEW_TYPE = 'memvault-panel';

interface MemVaultSettings {
  serverUrl: string;
  refreshInterval: number;
}

const DEFAULT_SETTINGS: MemVaultSettings = {
  serverUrl: 'http://127.0.0.1:8080',
  refreshInterval: 10,
};

interface Memory {
  id: string;
  memory_type: string;
  content: string;
  instruction: string | null;
  priority: string;
  namespace: string;
  tags: string[];
  layer: string;
  skill_meta: SkillMeta | null;
  access_count: number;
  human_reviewed: boolean;
  decay_score: number;
  created_at: string;
}

interface SkillMeta {
  trigger: string | null;
  steps: string[];
  verification: string | null;
  version: number;
}

interface SearchResult {
  memory: Memory;
  score: number;
}

export default class MemVaultPlugin extends Plugin {
  settings: MemVaultSettings = DEFAULT_SETTINGS;
  private refreshTimer: number | null = null;

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
      callback: () => new MemVaultSearchModal(this.app, this).open(),
    });

    this.addCommand({
      id: 'search-insert',
      name: 'Search and Insert Memory',
      editorCallback: (editor) => {
        new MemVaultInsertModal(this.app, this, editor).open();
      },
    });

    this.addCommand({
      id: 'save-selection',
      name: 'Save Selection as Memory',
      editorCallback: async (editor) => {
        const text = editor.getSelection();
        if (!text) { new Notice('No text selected'); return; }
        await this.saveMemory(text, 'REFERENCE', 'fact');
      },
    });

    this.addCommand({
      id: 'save-selection-must',
      name: 'Save Selection as MUST Rule',
      editorCallback: async (editor) => {
        const text = editor.getSelection();
        if (!text) { new Notice('No text selected'); return; }
        await this.saveMemory(text, 'MUST', 'preference');
      },
    });

    this.addCommand({
      id: 'review-inbox',
      name: 'Review Inbox',
      callback: () => this.activateView('inbox'),
    });

    this.addSettingTab(new MemVaultSettingTab(this.app, this));
  }

  onunload() {
    if (this.refreshTimer) {
      window.clearInterval(this.refreshTimer);
    }
  }

  async api(method: string, path: string, body?: any): Promise<any> {
    const url = `${this.settings.serverUrl}${path}`;
    const options: any = { url, method };
    if (body) {
      options.body = JSON.stringify(body);
      options.headers = { 'Content-Type': 'application/json' };
    }
    const resp = await requestUrl(options);
    return resp.json;
  }

  async listMemories(limit = 50): Promise<Memory[]> {
    return await this.api('GET', `/api/memories?limit=${limit}`);
  }

  async searchMemories(query: string, topK = 10): Promise<SearchResult[]> {
    return await this.api('POST', '/api/search', { query, top_k: topK });
  }

  async saveMemory(content: string, priority: string, type: string): Promise<void> {
    try {
      const result = await this.api('POST', '/api/memories', {
        content,
        priority,
        type,
        agent_id: 'obsidian',
        agent_type: 'note-editor',
        namespace: 'global',
      });
      new Notice(`Saved: ${result.id}`);
    } catch (e: any) {
      new Notice(`Save error: ${e.message}`);
    }
  }

  async getInbox(): Promise<Memory[]> {
    return await this.api('GET', '/api/inbox');
  }

  async approveMemory(id: string): Promise<void> {
    await this.api('POST', `/api/inbox/${id}/approve`);
  }

  async rejectMemory(id: string): Promise<void> {
    await this.api('POST', `/api/inbox/${id}/reject`);
  }

  async deleteMemory(id: string): Promise<void> {
    await this.api('DELETE', `/api/memories/${id}`);
  }

  async activateView(tab?: string) {
    const existing = this.app.workspace.getLeavesOfType(VIEW_TYPE);
    if (existing.length) {
      const view = existing[0].view as MemVaultView;
      if (tab) view.switchTab(tab);
      this.app.workspace.revealLeaf(existing[0]);
      return;
    }
    const leaf = this.app.workspace.getRightLeaf(false);
    if (leaf) {
      await leaf.setViewState({ type: VIEW_TYPE, active: true });
      this.app.workspace.revealLeaf(leaf);
      if (tab) {
        const view = leaf.view as MemVaultView;
        setTimeout(() => view.switchTab(tab), 100);
      }
    }
  }

  async loadSettings() {
    this.settings = Object.assign({}, DEFAULT_SETTINGS, await this.loadData());
  }

  async saveSettings() {
    await this.saveData(this.settings);
  }
}

// ─── Search Modal (Obsidian native SuggestModal) ─────────────────

class MemVaultSearchModal extends SuggestModal<SearchResult> {
  plugin: MemVaultPlugin;
  private results: SearchResult[] = [];

  constructor(app: App, plugin: MemVaultPlugin) {
    super(app);
    this.plugin = plugin;
    this.setPlaceholder('Search memories...');
  }

  async getSuggestions(query: string): Promise<SearchResult[]> {
    if (query.length < 2) return [];
    try {
      this.results = await this.plugin.searchMemories(query);
      return this.results;
    } catch {
      return [];
    }
  }

  renderSuggestion(result: SearchResult, el: HTMLElement) {
    const mem = result.memory;
    const priority = mem.priority === 'MUST' ? '🔴' : mem.priority === 'REFERENCE' ? '🔵' : '⚪';
    el.createEl('div', {
      text: `${priority} [${mem.layer}] ${mem.content.slice(0, 80)}`,
      cls: 'memvault-suggestion-title',
    });
    el.createEl('small', {
      text: `${mem.memory_type} · ${mem.tags.join(', ')} · score: ${result.score.toFixed(2)}`,
      cls: 'memvault-suggestion-meta',
    });
  }

  onChooseSuggestion(result: SearchResult) {
    const mem = result.memory;
    const content = mem.instruction || mem.content;
    navigator.clipboard.writeText(content);
    new Notice('Copied to clipboard');
  }
}

// ─── Insert Modal (search + insert into editor) ──────────────────

class MemVaultInsertModal extends SuggestModal<SearchResult> {
  plugin: MemVaultPlugin;
  editor: any;

  constructor(app: App, plugin: MemVaultPlugin, editor: any) {
    super(app);
    this.plugin = plugin;
    this.editor = editor;
    this.setPlaceholder('Search and insert memory...');
  }

  async getSuggestions(query: string): Promise<SearchResult[]> {
    if (query.length < 2) return [];
    try {
      return await this.plugin.searchMemories(query);
    } catch {
      return [];
    }
  }

  renderSuggestion(result: SearchResult, el: HTMLElement) {
    const mem = result.memory;
    const priority = mem.priority === 'MUST' ? '🔴' : '🔵';
    el.createEl('div', { text: `${priority} ${mem.content.slice(0, 80)}` });
  }

  onChooseSuggestion(result: SearchResult) {
    const mem = result.memory;
    const text = mem.instruction || mem.content;
    this.editor.replaceSelection(text);
  }
}

// ─── Sidebar View ────────────────────────────────────────────────

class MemVaultView extends ItemView {
  plugin: MemVaultPlugin;
  private currentTab: string = 'memories';
  private refreshTimer: number | null = null;

  constructor(leaf: WorkspaceLeaf, plugin: MemVaultPlugin) {
    super(leaf);
    this.plugin = plugin;
  }

  getViewType() { return VIEW_TYPE; }
  getDisplayText() { return 'MemVault'; }
  getIcon() { return 'database'; }

  async onOpen() {
    this.render();
    this.startAutoRefresh();
  }

  onClose() {
    if (this.refreshTimer) {
      window.clearInterval(this.refreshTimer);
      this.refreshTimer = null;
    }
    return Promise.resolve();
  }

  switchTab(tab: string) {
    this.currentTab = tab;
    this.render();
  }

  private startAutoRefresh() {
    const interval = this.plugin.settings.refreshInterval * 1000;
    if (interval > 0) {
      this.refreshTimer = window.setInterval(() => this.render(), interval);
    }
  }

  private async render() {
    const container = this.containerEl.children[1] as HTMLElement;
    container.empty();

    // Tab bar
    const tabs = container.createEl('div', { cls: 'memvault-tabs' });
    tabs.style.display = 'flex';
    tabs.style.gap = '4px';
    tabs.style.marginBottom = '8px';

    const memTab = tabs.createEl('button', { text: 'Memories' });
    const inboxTab = tabs.createEl('button', { text: 'Inbox' });
    memTab.style.flex = '1';
    inboxTab.style.flex = '1';

    memTab.toggleClass('mod-cta', this.currentTab === 'memories');
    inboxTab.toggleClass('mod-cta', this.currentTab === 'inbox');

    memTab.onclick = () => { this.currentTab = 'memories'; this.render(); };
    inboxTab.onclick = () => { this.currentTab = 'inbox'; this.render(); };

    // Content
    if (this.currentTab === 'memories') {
      await this.renderMemories(container);
    } else {
      await this.renderInbox(container);
    }
  }

  private async renderMemories(container: HTMLElement) {
    try {
      const memories = await this.plugin.listMemories();
      if (!memories.length) {
        container.createEl('p', { text: 'No memories stored.', cls: 'mod-muted' });
        return;
      }

      const list = container.createEl('div', { cls: 'memvault-list' });
      for (const mem of memories) {
        this.renderMemoryItem(list, mem, false);
      }

      container.createEl('small', {
        text: `${memories.length} memories · auto-refresh ${this.plugin.settings.refreshInterval}s`,
        cls: 'mod-muted',
      });
    } catch (e: any) {
      container.createEl('p', {
        text: `Connection error: ${e.message}\nEnsure MemVault server is running on ${this.plugin.settings.serverUrl}`,
        cls: 'mod-warning',
      });
    }
  }

  private async renderInbox(container: HTMLElement) {
    try {
      const inbox = await this.plugin.getInbox();
      if (!inbox.length) {
        container.createEl('p', { text: '✓ Inbox is empty — all memories reviewed.', cls: 'mod-muted' });
        return;
      }

      container.createEl('small', {
        text: `${inbox.length} pending review`,
        cls: 'mod-muted',
      });

      const list = container.createEl('div', { cls: 'memvault-list' });
      for (const mem of inbox) {
        this.renderMemoryItem(list, mem, true);
      }
    } catch (e: any) {
      container.createEl('p', { text: `Error: ${e.message}`, cls: 'mod-warning' });
    }
  }

  private renderMemoryItem(container: HTMLElement, mem: Memory, showActions: boolean) {
    const item = container.createEl('div', { cls: 'memvault-item' });
    item.style.padding = '6px 0';
    item.style.borderBottom = '1px solid var(--background-modifier-border)';

    // Header: priority + layer + type
    const header = item.createEl('div', { cls: 'memvault-item-header' });
    header.style.display = 'flex';
    header.style.gap = '4px';
    header.style.alignItems = 'center';
    header.style.marginBottom = '2px';

    const priorityIcon = mem.priority === 'MUST' ? '🔴' : mem.priority === 'REFERENCE' ? '🔵' : '⚪';
    header.createEl('span', { text: priorityIcon });
    header.createEl('span', {
      text: mem.layer,
      cls: 'memvault-badge',
    }).style.cssText = 'font-size:10px;padding:1px 4px;border-radius:3px;background:var(--background-modifier-border)';
    header.createEl('span', {
      text: mem.memory_type,
      cls: 'memvault-badge',
    }).style.cssText = 'font-size:10px;padding:1px 4px;border-radius:3px;background:var(--background-modifier-border)';

    // Content
    const content = item.createEl('div', {
      text: mem.content.slice(0, 120) + (mem.content.length > 120 ? '...' : ''),
    });
    content.style.fontSize = '12px';
    content.style.lineHeight = '1.4';

    // Instruction (if different from content)
    if (mem.instruction && mem.instruction !== mem.content) {
      const inst = item.createEl('div', {
        text: `→ ${mem.instruction.slice(0, 100)}`,
      });
      inst.style.fontSize = '11px';
      inst.style.color = 'var(--text-accent)';
      inst.style.marginTop = '2px';
    }

    // Skill meta
    if (mem.skill_meta) {
      const skill = item.createEl('div');
      skill.style.fontSize = '11px';
      skill.style.marginTop = '2px';
      skill.style.color = 'var(--text-faint)';
      if (mem.skill_meta.trigger) {
        skill.createEl('span', { text: `⚡ ${mem.skill_meta.trigger}` });
      }
      if (mem.skill_meta.steps.length) {
        skill.createEl('span', { text: ` · ${mem.skill_meta.steps.length} steps` });
      }
    }

    // Tags
    if (mem.tags.length) {
      const tags = item.createEl('div');
      tags.style.marginTop = '2px';
      for (const tag of mem.tags) {
        const badge = tags.createEl('span', { text: tag });
        badge.style.cssText = 'font-size:10px;margin-right:4px;padding:1px 4px;border-radius:3px;background:var(--background-modifier-border-hover)';
      }
    }

    // Action buttons
    if (showActions) {
      const actions = item.createEl('div');
      actions.style.marginTop = '4px';
      actions.style.display = 'flex';
      actions.style.gap = '4px';

      const approveBtn = actions.createEl('button', { text: '✓ Approve' });
      approveBtn.style.fontSize = '11px';
      approveBtn.onclick = async () => {
        await this.plugin.approveMemory(mem.id);
        new Notice('Approved');
        this.render();
      };

      const rejectBtn = actions.createEl('button', { text: '✗ Reject' });
      rejectBtn.style.fontSize = '11px';
      rejectBtn.onclick = async () => {
        await this.plugin.rejectMemory(mem.id);
        new Notice('Rejected');
        this.render();
      };
    }
  }
}

// ─── Settings ────────────────────────────────────────────────────

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
      .setName('Server URL')
      .setDesc('MemVault MCP server HTTP address (requires --transport sse or REST mode)')
      .addText(text => text
        .setPlaceholder('http://127.0.0.1:8080')
        .setValue(this.plugin.settings.serverUrl)
        .onChange(async (value) => {
          this.plugin.settings.serverUrl = value;
          await this.plugin.saveSettings();
        }));

    new Setting(containerEl)
      .setName('Auto-refresh interval')
      .setDesc('How often to refresh the sidebar (seconds, 0 to disable)')
      .addText(text => text
        .setPlaceholder('10')
        .setValue(String(this.plugin.settings.refreshInterval))
        .onChange(async (value) => {
          this.plugin.settings.refreshInterval = parseInt(value) || 10;
          await this.plugin.saveSettings();
        }));
  }
}
