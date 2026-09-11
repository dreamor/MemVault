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
  folderFor,
} from './sync';

/**
 * MemVault Obsidian plugin — converged scope (2026-09-10).
 *
 * Kept: vault bidirectional sync (the core differentiator), browse/search,
 * selection capture (save / extract), mark-read, delete.
 * Management surfaces (Review Inbox / approve / reject / supersede / edit /
 * stats / export / import / backup / checkpoints / dedup / decay / promote)
 * are handled by the Web Dashboard and the CLI instead.
 */

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
      id: 'extract-selection',
      name: 'Extract Memories from Selection',
      editorCallback: async (editor) => {
        const text = editor.getSelection();
        if (!text) { new Notice('No text selected'); return; }
        try {
          const result = (await this.api('POST', '/api/extract', {
            text,
            mode: 'rule',
            auto_save: true,
          })) as { memories: unknown[]; saved_ids?: string[] };
          const candidates = result.memories?.length ?? 0;
          if (candidates === 0) { new Notice('No extractable memories found in selection'); return; }
          const saved = result.saved_ids?.length ?? 0;
          new Notice(
            saved > 0
              ? `${candidates} candidate(s) extracted — ${saved} saved to review inbox`
              : `${candidates} candidate(s) extracted`,
          );
        } catch (e) {
          new Notice(`Extraction failed: ${e instanceof Error ? e.message : String(e)}`);
        }
      },
    });

    this.addCommand({
      id: 'mark-read',
      name: 'Mark Memory as Read',
      callback: () => {
        new SimplePromptModal(this.app, {
          title: 'Mark as read (refresh access recency — decay weighs it)',
          placeholder: 'mem_...',
          submitLabel: 'Mark as Read',
          onSubmit: async (id) => {
            const trimmed = id.trim();
            if (!trimmed) { new Notice('Memory ID required'); return; }
            try {
              await this.api('POST', '/api/confirm-read', { memory_ids: [trimmed] });
              new Notice('Marked as read (access_count bumped)');
            } catch (e) {
              new Notice(`Mark as read failed: ${e instanceof Error ? e.message : String(e)}`);
            }
          },
        });
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
    // Server API returns the memory kind as 'type'; map to the plugin's
    // 'memory_type' field so folderFor/buildNoteContent stay field-aligned.
    const raw = (await this.api("GET", `/api/memories?limit=${limit}`)) as any[];
    return raw.map((m) => ({ ...m, memory_type: m.type ?? m.memory_type }));
  }



  private async writeVaultFile(path: string, content: string): Promise<void> {
    const existing = this.app.vault.getAbstractFileByPath(path);
    if (existing instanceof TFile) {
      await this.app.vault.modify(existing, content);
    } else {
      await this.app.vault.create(path, content);
    }
  }

  async searchMemories(query: string, topK = 10): Promise<SearchResult[]> {
    const raw = (await this.api("POST", "/api/search", { query, top_k: topK })) as any[];
    return raw.map((r) => ({ ...r, memory: { ...r.memory, memory_type: r.memory.type ?? r.memory.memory_type } }));
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

  async deleteMemory(id: string): Promise<void> {
    await this.api('DELETE', `/api/memories/${id}`);
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
        const sub = folderFor(mem);
        await this.ensureFolder(`${folder}/${sub}`);
        const path = `${folder}/${sub}/${fileNameFor(mem)}`;
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
    el.createEl('div', {
      text: `[${mem.layer}] ${mem.content.slice(0, 80)}`,
      cls: 'memvault-suggestion-title',
    });
    el.createEl('small', {
      text: `${mem.priority} · ${mem.memory_type} · ${mem.tags.join(', ')} · score: ${result.score.toFixed(2)}`,
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
    el.createEl('div', {
      text: `${mem.content.slice(0, 80)}`,
      cls: 'memvault-suggestion-title',
    });
    el.createEl('small', {
      text: `${mem.priority} · ${mem.memory_type} · ${mem.layer}`,
      cls: 'memvault-suggestion-meta',
    });
  }

  onChooseSuggestion(result: SearchResult) {
    const mem = result.memory;
    const text = mem.instruction || mem.content;
    this.editor.replaceSelection(text);
  }
}

// ─── Stats Modal ───────────────────────────────────────────────────

// ─── Simple single-field prompt Modal (Mark as read) ──────────────

class SimplePromptModal extends Modal {
  private value: string;
  private title: string;
  private placeholder: string;
  private multiline: boolean;
  private submitLabel: string;
  private onSubmit: (value: string) => Promise<void>;

  constructor(
    app: App,
    opts: {
      title: string;
      initialValue?: string;
      placeholder?: string;
      multiline?: boolean;
      submitLabel?: string;
      onSubmit: (value: string) => Promise<void>;
    },
  ) {
    super(app);
    this.title = opts.title;
    this.value = opts.initialValue ?? '';
    this.placeholder = opts.placeholder ?? '';
    this.multiline = opts.multiline ?? false;
    this.submitLabel = opts.submitLabel ?? 'Submit';
    this.onSubmit = opts.onSubmit;
  }

  onOpen() {
    const { contentEl } = this;
    contentEl.empty();
    contentEl.createEl('h2', { text: this.title });

    const setting = new Setting(contentEl);
    if (this.multiline) {
      setting.addTextArea((t) => t.setPlaceholder(this.placeholder).setValue(this.value).onChange((v) => (this.value = v)));
    } else {
      setting.addText((t) => t.setPlaceholder(this.placeholder).setValue(this.value).onChange((v) => (this.value = v)));
    }

    new Setting(contentEl).addButton((b) =>
      b
        .setButtonText(this.submitLabel)
        .setCta()
        .onClick(async () => {
          try {
            await this.onSubmit(this.value);
            this.close();
          } catch (e: any) {
            new Notice(`Failed: ${e.message}`);
          }
        }),
    );
  }

  onClose() {
    this.contentEl.empty();
  }
}

// ─── Extract from Text Modal (preview → select → save) ────────────

// ─── Sidebar View ────────────────────────────────────────────────

class MemVaultView extends ItemView {
  plugin: MemVaultPlugin;
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

    const header = container.createEl('div', { cls: 'memvault-panel-header' });
    header.createEl('span', { text: 'MemVault', cls: 'memvault-panel-title' });
    const syncBtn = header.createEl('button', { text: 'Sync', cls: 'mod-cta' });
    syncBtn.setAttr('aria-label', 'Sync memories to vault');
    syncBtn.onclick = async () => {
      try {
        await this.plugin.syncVaultFromServer();
      } catch (e: any) {
        new Notice(`Sync failed: ${e.message}`);
      }
    };

    await this.renderMemories(container);
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
        this.renderMemoryItem(list, mem);
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

  private renderMemoryItem(container: HTMLElement, mem: Memory) {
    const item = container.createEl('div', { cls: 'memvault-item' });
    item.setAttr('data-priority', mem.priority);

    // Header: priority + layer + type
    const header = item.createEl('div', { cls: 'memvault-item-header' });

    const priority = (mem.priority || 'BACKGROUND').toUpperCase();
    header.createEl('span', {
      text: priority,
      cls: `memvault-priority memvault-priority-${priority.toLowerCase()}`,
    });
    header.createEl('span', { text: mem.layer, cls: 'memvault-layer' });
    header.createEl('span', { text: mem.memory_type, cls: 'memvault-type' });

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
        skill.createEl('span', { text: mem.skill_meta.trigger });
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

    // Actions (management lives in the Web Dashboard / CLI — only Delete stays inline)
    const actions = item.createEl('div', { cls: 'memvault-actions' });

    const deleteBtn = actions.createEl('button', { text: 'Delete', cls: 'memvault-delete' });
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
      .setDesc('MemVault MCP server HTTP address (requires --transport http / REST mode)')
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
