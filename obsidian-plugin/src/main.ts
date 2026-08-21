import {
  App,
  Plugin,
  PluginSettingTab,
  Setting,
  ItemView,
  WorkspaceLeaf,
  Notice,
  Modal,
  SuggestModal,
  MarkdownView,
  TFile,
  requestUrl,
} from 'obsidian';
import {
  RemoteMemory,
  FrontmatterIndexEntry,
  buildIdIndex,
  decideAction,
  detectOrphans,
  buildNoteContent,
  fileNameFor,
} from './sync';

const VIEW_TYPE = 'memvault-panel';
const PRIORITIES = ['MUST', 'REFERENCE', 'BACKGROUND'];
const MEMORY_TYPES = ['preference', 'fact', 'episode', 'entity', 'skill'];

interface MemVaultSettings {
  serverUrl: string;
  refreshInterval: number;
  apiKey: string;
  syncFolder: string;
  syncDeleteOrphans: boolean;
}

const DEFAULT_SETTINGS: MemVaultSettings = {
  serverUrl: 'http://127.0.0.1:8080',
  refreshInterval: 10,
  apiKey: '',
  syncFolder: 'MemVault',
  syncDeleteOrphans: false,
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
  updated_at: string;
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

    this.addCommand({
      id: 'create-memory',
      name: 'Create Memory',
      callback: () => {
        const modal = new MemVaultCreateModal(this.app, this);
        modal.onSaved = () => this.refreshOpenViews();
        modal.open();
      },
    });

    this.addCommand({
      id: 'sync-vault',
      name: 'Sync Memories to Vault',
      callback: async () => {
        try {
          await this.syncVaultFromServer();
        } catch (e: any) {
          new Notice(`Sync failed: ${e.message}`);
        }
      },
    });

    this.addCommand({
      id: 'run-dedup',
      name: 'Run Dedup',
      callback: async () => {
        try {
          const r = await this.runDedup();
          new Notice(`Dedup: ${r.unique} unique, ${r.duplicates} duplicates found`);
        } catch (e: any) {
          new Notice(`Dedup failed: ${e.message}`);
        }
      },
    });

    this.addCommand({
      id: 'run-decay',
      name: 'Run Decay',
      callback: async () => {
        try {
          const r = await this.runDecay();
          new Notice(`Decay: ${r.updated} updated, ${r.archived} archived`);
        } catch (e: any) {
          new Notice(`Decay failed: ${e.message}`);
        }
      },
    });

    this.addCommand({
      id: 'run-promote',
      name: 'Run Promote (L1→L2→L3)',
      callback: async () => {
        try {
          const r = await this.runPromote();
          new Notice(`Promote: ${r.promoted_to_l2} → L2, ${r.promoted_to_l3} → L3`);
        } catch (e: any) {
          new Notice(`Promote failed: ${e.message}`);
        }
      },
    });

    this.addSettingTab(new MemVaultSettingTab(this.app, this));
  }

  onunload() {
    if (this.refreshTimer) {
      window.clearInterval(this.refreshTimer);
    }
  }

  /// Every REST response is wrapped as `{ ok, data, error }` — unwrap `data`
  /// here so every caller below just gets the real payload, and throw on
  /// `ok: false` so callers can rely on try/catch instead of checking `ok`.
  async api(method: string, path: string, body?: any): Promise<any> {
    const url = `${this.settings.serverUrl}${path}`;
    const options: any = { url, method };
    const headers: Record<string, string> = {};
    if (body) {
      options.body = JSON.stringify(body);
      headers['Content-Type'] = 'application/json';
    }
    if (this.settings.apiKey) {
      headers['X-MemVault-Api-Key'] = this.settings.apiKey;
    }
    if (Object.keys(headers).length) {
      options.headers = headers;
    }
    const resp = await requestUrl(options);
    const parsed = resp.json;
    if (parsed && typeof parsed === 'object' && 'ok' in parsed) {
      if (!parsed.ok) {
        throw new Error(parsed.error || 'MemVault API error');
      }
      return parsed.data;
    }
    return parsed;
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
    const inbox: { memories: Memory[]; total: number } = await this.api('GET', '/api/inbox');
    return inbox.memories;
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

  async createMemoryFull(values: {
    content: string;
    instruction: string;
    priority: string;
    memoryType: string;
    namespace: string;
    tags: string[];
  }): Promise<void> {
    await this.api('POST', '/api/memories', {
      content: values.content,
      instruction: values.instruction || null,
      priority: values.priority,
      type: values.memoryType,
      namespace: values.namespace,
      tags: values.tags,
      agent_id: 'obsidian',
      agent_type: 'note-editor',
    });
  }

  async updateMemory(
    id: string,
    patch: {
      content: string;
      instruction: string;
      priority: string;
      memoryType: string;
      namespace: string;
      tags: string[];
    },
  ): Promise<void> {
    await this.api('PUT', `/api/memories/${id}`, {
      content: patch.content,
      instruction: patch.instruction || null,
      priority: patch.priority,
      type: patch.memoryType,
      namespace: patch.namespace,
      tags: patch.tags,
    });
  }

  async runDedup(): Promise<{ unique: number; duplicates: number }> {
    return await this.api('POST', '/api/dedup');
  }

  async runDecay(): Promise<{ updated: number; archived: number }> {
    return await this.api('POST', '/api/decay');
  }

  async runPromote(): Promise<{ promoted_to_l2: number; promoted_to_l3: number }> {
    return await this.api('POST', '/api/promote', {});
  }

  private async ensureFolder(path: string): Promise<void> {
    const existing = this.app.vault.getAbstractFileByPath(path);
    if (!existing) {
      await this.app.vault.createFolder(path).catch(() => {
        // race with a concurrent creator; re-check below is enough
      });
    }
  }

  /** One-way DB → vault sync (see docs/INSTALL.md and README for the frontmatter schema). */
  async syncVaultFromServer(): Promise<void> {
    const folder = this.settings.syncFolder || 'MemVault';
    await this.ensureFolder(folder);

    const memories: RemoteMemory[] = await this.listMemories(10000);

    const existingFiles = this.app.vault
      .getMarkdownFiles()
      .filter((f) => f.path === folder || f.path.startsWith(`${folder}/`));

    const indexEntries: FrontmatterIndexEntry[] = [];
    for (const file of existingFiles) {
      const fm = this.app.metadataCache.getFileCache(file)?.frontmatter;
      if (fm?.memvault_id) {
        indexEntries.push({
          path: file.path,
          memvaultId: fm.memvault_id,
          updatedAt: fm.memvault_updated_at ?? '',
        });
      }
    }
    const index = buildIdIndex(indexEntries);

    let created = 0;
    let updated = 0;
    let skipped = 0;

    for (const mem of memories) {
      const existing = index.get(mem.id);
      const action = decideAction(mem, existing);
      if (action === 'skip') {
        skipped++;
        continue;
      }
      const content = buildNoteContent(mem);
      if (action === 'create') {
        const path = `${folder}/${fileNameFor(mem)}`;
        await this.app.vault.create(path, content);
        created++;
      } else if (existing) {
        const file = this.app.vault.getAbstractFileByPath(existing.path);
        if (file instanceof TFile) {
          await this.app.vault.modify(file, content);
          updated++;
        }
      }
    }

    let archived = 0;
    if (this.settings.syncDeleteOrphans) {
      const remoteIds = new Set(memories.map((m) => m.id));
      const orphans = detectOrphans(indexEntries, remoteIds);
      for (const o of orphans) {
        const file = this.app.vault.getAbstractFileByPath(o.path);
        if (file instanceof TFile) {
          await this.app.vault.delete(file);
          archived++;
        }
      }
    }

    new Notice(
      `MemVault sync: ${created} created, ${updated} updated, ${skipped} unchanged` +
        (archived ? `, ${archived} removed` : ''),
    );
  }

  refreshOpenViews(): void {
    for (const leaf of this.app.workspace.getLeavesOfType(VIEW_TYPE)) {
      (leaf.view as MemVaultView).refresh();
    }
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

// ─── Create / Edit Modal ──────────────────────────────────────────

interface MemoryFormValues {
  content: string;
  instruction: string;
  priority: string;
  memoryType: string;
  namespace: string;
  tagsInput: string;
}

abstract class MemoryFormModal extends Modal {
  protected values: MemoryFormValues;
  /** Set by the caller to refresh a list view after a successful save. */
  onSaved?: () => void;

  constructor(app: App, initial: MemoryFormValues) {
    super(app);
    this.values = { ...initial };
  }

  abstract getTitle(): string;
  abstract getSubmitLabel(): string;
  abstract onFormSubmit(values: MemoryFormValues): Promise<void>;

  onOpen() {
    const { contentEl } = this;
    contentEl.empty();
    contentEl.createEl('h2', { text: this.getTitle() });

    new Setting(contentEl).setName('Content').addTextArea((t) =>
      t.setValue(this.values.content).onChange((v) => (this.values.content = v)),
    );
    new Setting(contentEl).setName('Instruction (optional)').addTextArea((t) =>
      t.setValue(this.values.instruction).onChange((v) => (this.values.instruction = v)),
    );
    new Setting(contentEl).setName('Priority').addDropdown((d) => {
      PRIORITIES.forEach((p) => d.addOption(p, p));
      d.setValue(this.values.priority).onChange((v) => (this.values.priority = v));
    });
    new Setting(contentEl).setName('Type').addDropdown((d) => {
      MEMORY_TYPES.forEach((t) => d.addOption(t, t));
      d.setValue(this.values.memoryType).onChange((v) => (this.values.memoryType = v));
    });
    new Setting(contentEl).setName('Namespace').addText((t) =>
      t.setValue(this.values.namespace).onChange((v) => (this.values.namespace = v)),
    );
    new Setting(contentEl).setName('Tags (comma-separated)').addText((t) =>
      t.setValue(this.values.tagsInput).onChange((v) => (this.values.tagsInput = v)),
    );

    new Setting(contentEl).addButton((b) =>
      b
        .setButtonText(this.getSubmitLabel())
        .setCta()
        .onClick(async () => {
          if (!this.values.content.trim()) {
            new Notice('Content is required');
            return;
          }
          try {
            await this.onFormSubmit(this.values);
            this.close();
            this.onSaved?.();
          } catch (e: any) {
            new Notice(`Save failed: ${e.message}`);
          }
        }),
    );
  }

  onClose() {
    this.contentEl.empty();
  }
}

class MemVaultCreateModal extends MemoryFormModal {
  plugin: MemVaultPlugin;

  constructor(app: App, plugin: MemVaultPlugin) {
    super(app, {
      content: '',
      instruction: '',
      priority: 'REFERENCE',
      memoryType: 'fact',
      namespace: 'global',
      tagsInput: '',
    });
    this.plugin = plugin;
  }

  getTitle() {
    return 'New Memory';
  }
  getSubmitLabel() {
    return 'Create';
  }

  async onFormSubmit(values: MemoryFormValues) {
    const tags = values.tagsInput.split(',').map((t) => t.trim()).filter(Boolean);
    await this.plugin.createMemoryFull({ ...values, tags });
    new Notice('Memory created');
  }
}

class MemVaultEditModal extends MemoryFormModal {
  plugin: MemVaultPlugin;
  memoryId: string;

  constructor(app: App, plugin: MemVaultPlugin, memory: Memory) {
    super(app, {
      content: memory.content,
      instruction: memory.instruction ?? '',
      priority: memory.priority.toUpperCase(),
      memoryType: memory.memory_type.toLowerCase(),
      namespace: memory.namespace,
      tagsInput: memory.tags.join(', '),
    });
    this.plugin = plugin;
    this.memoryId = memory.id;
  }

  getTitle() {
    return 'Edit Memory';
  }
  getSubmitLabel() {
    return 'Save Changes';
  }

  async onFormSubmit(values: MemoryFormValues) {
    const tags = values.tagsInput.split(',').map((t) => t.trim()).filter(Boolean);
    await this.plugin.updateMemory(this.memoryId, { ...values, tags });
    new Notice('Memory updated');
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

  refresh() {
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

    const memTab = tabs.createEl('button', { text: 'Memories' });
    const inboxTab = tabs.createEl('button', { text: 'Inbox' });

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
    item.setAttr('data-priority', mem.priority);

    // Header: priority + layer + type
    const header = item.createEl('div', { cls: 'memvault-item-header' });

    const priorityIcon = mem.priority === 'MUST' ? '🔴' : mem.priority === 'REFERENCE' ? '🔵' : '⚪';
    header.createEl('span', { text: priorityIcon });
    header.createEl('span', { text: mem.layer, cls: 'memvault-badge memvault-layer' });
    header.createEl('span', { text: mem.memory_type, cls: 'memvault-badge memvault-type' });

    // Content
    const content = item.createEl('div', {
      text: mem.content.slice(0, 120) + (mem.content.length > 120 ? '...' : ''),
      cls: 'memvault-item-content',
    });

    // Instruction (if different from content)
    if (mem.instruction && mem.instruction !== mem.content) {
      const inst = item.createEl('div', {
        text: `→ ${mem.instruction.slice(0, 100)}`,
        cls: 'memvault-instruction',
      });
    }

    // Skill meta
    if (mem.skill_meta) {
      const skill = item.createEl('div', { cls: 'memvault-skill' });
      if (mem.skill_meta.trigger) {
        skill.createEl('span', { text: `⚡ ${mem.skill_meta.trigger}` });
      }
      if (mem.skill_meta.steps.length) {
        skill.createEl('span', { text: ` · ${mem.skill_meta.steps.length} steps` });
      }
    }

    // Tags
    if (mem.tags.length) {
      const tags = item.createEl('div', { cls: 'memvault-tags' });
      for (const tag of mem.tags) {
        tags.createEl('span', { text: tag, cls: 'memvault-tag' });
      }
    }

    // Action buttons
    const actions = item.createEl('div', { cls: 'memvault-actions' });

    if (showActions) {
      const approveBtn = actions.createEl('button', {
        text: '✓ Approve',
        cls: 'memvault-approve',
      });
      approveBtn.onclick = async () => {
        try {
          await this.plugin.approveMemory(mem.id);
          new Notice('Approved');
          this.render();
        } catch (e) {
          new Notice(`Approve failed: ${e}`);
        }
      };

      const rejectBtn = actions.createEl('button', {
        text: '✗ Reject',
        cls: 'memvault-reject',
      });
      rejectBtn.onclick = async () => {
        try {
          await this.plugin.rejectMemory(mem.id);
          new Notice('Rejected');
          this.render();
        } catch (e) {
          new Notice(`Reject failed: ${e}`);
        }
      };
    }

    const editBtn = actions.createEl('button', { text: '✎ Edit' });
    editBtn.onclick = () => {
      const modal = new MemVaultEditModal(this.app, this.plugin, mem);
      modal.onSaved = () => this.render();
      modal.open();
    };

    const deleteBtn = actions.createEl('button', { text: '🗑 Delete' });
    deleteBtn.onclick = async () => {
      if (!confirm('Delete this memory? This cannot be undone.')) return;
      try {
        await this.plugin.deleteMemory(mem.id);
        new Notice('Deleted');
        this.render();
      } catch (e) {
        new Notice(`Delete failed: ${e}`);
      }
    };
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

    new Setting(containerEl)
      .setName('API Key')
      .setDesc('Sent as X-MemVault-Api-Key for admin-protected REST routes (leave empty if the server has no admin key configured)')
      .addText(text => text
        .setPlaceholder('')
        .setValue(this.plugin.settings.apiKey)
        .onChange(async (value) => {
          this.plugin.settings.apiKey = value;
          await this.plugin.saveSettings();
        }));

    containerEl.createEl('h3', { text: 'Vault Sync' });

    new Setting(containerEl)
      .setName('Sync folder')
      .setDesc('Vault folder that "Sync Memories to Vault" writes notes into')
      .addText(text => text
        .setPlaceholder('MemVault')
        .setValue(this.plugin.settings.syncFolder)
        .onChange(async (value) => {
          this.plugin.settings.syncFolder = value || 'MemVault';
          await this.plugin.saveSettings();
        }));

    new Setting(containerEl)
      .setName('Delete orphaned notes on sync')
      .setDesc('If a synced note\'s memory no longer exists on the server, delete the local note too. Off by default so manual edits are never silently lost.')
      .addToggle(toggle => toggle
        .setValue(this.plugin.settings.syncDeleteOrphans)
        .onChange(async (value) => {
          this.plugin.settings.syncDeleteOrphans = value;
          await this.plugin.saveSettings();
        }));
  }
}
