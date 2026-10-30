import {
  App,
  Editor,
  Plugin,
  PluginSettingTab,
  RequestUrlParam,
  Setting,
  ItemView,
  WorkspaceLeaf,
  Notice,
  Modal,
  SuggestModal,
  TFile,
  requestUrl,
  SettingDefinitionItem,
  TextComponent,
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
const DEFAULT_SERVER_URL = 'http://127.0.0.1:3777';
const LEGACY_SERVER_URL = 'http://127.0.0.1:8080';
const SERVER_PROBE_URLS = [DEFAULT_SERVER_URL, LEGACY_SERVER_URL] as const;

interface MemVaultSettings {
  serverUrl: string;
  refreshInterval: number;
  apiKey: string;
  syncFolder: string;
  syncDeleteOrphans: boolean;
}

const DEFAULT_SETTINGS: MemVaultSettings = {
  serverUrl: DEFAULT_SERVER_URL,
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

/** Wire format returned by the server: the memory kind is named 'type'. */
interface ServerMemory extends Omit<Memory, 'memory_type'> {
  type?: string;
  memory_type?: string;
}

interface ServerSearchResult {
  memory: ServerMemory;
  score: number;
}

interface HealthResponse {
  status?: string;
  service?: string;
  version?: string;
}

function asMessage(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
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
      name: 'Open memory panel',
      callback: () => this.activateView(),
    });

    this.addCommand({
      id: 'detect-server',
      name: 'Detect server',
      callback: async () => {
        await this.detectAndSaveServer();
      },
    });

    this.addCommand({
      id: 'search',
      name: 'Search memories',
      callback: () => new MemVaultSearchModal(this.app, this).open(),
    });

    this.addCommand({
      id: 'search-insert',
      name: 'Search and insert memory',
      editorCallback: (editor) => {
        new MemVaultInsertModal(this.app, this, editor).open();
      },
    });

    this.addCommand({
      id: 'save-selection',
      name: 'Save selection as memory',
      editorCallback: async (editor) => {
        const text = editor.getSelection();
        if (!text) { new Notice('No text selected'); return; }
        await this.saveMemory(text, 'REFERENCE', 'fact');
      },
    });

    this.addCommand({
      id: 'save-selection-must',
      name: 'Save selection as MUST rule',
      editorCallback: async (editor) => {
        const text = editor.getSelection();
        if (!text) { new Notice('No text selected'); return; }
        await this.saveMemory(text, 'MUST', 'preference');
      },
    });

    this.addCommand({
      id: 'extract-selection',
      name: 'Extract memories from selection',
      editorCallback: async (editor) => {
        const text = editor.getSelection();
        if (!text) { new Notice('No text selected'); return; }
        try {
          const result = await this.api<{ memories: unknown[]; saved_ids?: string[] }>('POST', '/api/extract', {
            text,
            mode: 'rule',
            auto_save: true,
          });
          const candidates = result.memories?.length ?? 0;
          if (candidates === 0) { new Notice('No extractable memories found in selection'); return; }
          const saved = result.saved_ids?.length ?? 0;
          new Notice(
            saved > 0
              ? `${candidates} candidate(s) extracted — ${saved} saved to review inbox`
              : `${candidates} candidate(s) extracted`,
          );
        } catch (e) {
          new Notice(`Extraction failed: ${asMessage(e)}`);
        }
      },
    });

    this.addCommand({
      id: 'mark-read',
      name: 'Mark memory as read',
      callback: () => {
        new SimplePromptModal(this.app, {
          title: 'Mark as read (refresh access recency — decay weighs it)',
          placeholder: 'mem_...',
          submitLabel: 'Mark as read',
          onSubmit: async (id) => {
            const trimmed = id.trim();
            if (!trimmed) { new Notice('Memory ID required'); return; }
            try {
              await this.api('POST', '/api/confirm-read', { memory_ids: [trimmed] });
              new Notice('Marked as read (access_count bumped)');
            } catch (e) {
              new Notice(`Mark as read failed: ${asMessage(e)}`);
            }
          },
        });
      },
    });

    this.addCommand({
      id: 'sync-vault',
      name: 'Sync memories to vault',
      callback: async () => {
        try {
          await this.syncVaultFromServer();
        } catch (e) {
          new Notice(`Sync failed: ${asMessage(e)}`);
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
  private async probeServer(baseUrl: string): Promise<boolean> {
    try {
      const resp = await requestUrl({
        url: `${baseUrl.replace(/\/+$/, '')}/api/health`,
        method: 'GET',
      });
      const health = resp.json as HealthResponse;
      return health?.status === 'ok' && health?.service === 'memvault-mcp';
    } catch {
      return false;
    }
  }

  /** Probe the installed default first, then the legacy manual-install port. */
  async detectServerUrl(): Promise<string | null> {
    for (const candidate of SERVER_PROBE_URLS) {
      if (await this.probeServer(candidate)) return candidate;
    }
    return null;
  }

  /** Explicit user action only — loading settings never rewrites an old URL. */
  async detectAndSaveServer(): Promise<boolean> {
    const detected = await this.detectServerUrl();
    if (!detected) {
      new Notice(
        'No MemVault server found on 127.0.0.1:3777 or :8080. Run: memvault-cli dashboard',
      );
      return false;
    }
    this.settings.serverUrl = detected;
    await this.saveSettings();
    new Notice(`Detected MemVault server at ${detected}`);
    this.refreshOpenViews();
    return true;
  }

  async api<T = unknown>(method: string, path: string, body?: unknown): Promise<T> {
    const baseUrl = this.settings.serverUrl.replace(/\/+$/, '');
    const url = `${baseUrl}${path}`;
    const options: RequestUrlParam = { url, method };
    const headers: Record<string, string> = {};
    if (body !== undefined) {
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
    const parsed = resp.json as { ok?: boolean; error?: string; data?: T };
    if (parsed && typeof parsed === 'object' && parsed.ok !== undefined) {
      if (!parsed.ok) {
        throw new Error(parsed.error || 'MemVault API error');
      }
      return parsed.data as T;
    }
    return parsed as T;
  }

  async listMemories(limit = 50): Promise<Memory[]> {
    // Server API returns the memory kind as 'type'; map to the plugin's
    // 'memory_type' field so folderFor/buildNoteContent stay field-aligned.
    const raw = await this.api<ServerMemory[]>("GET", `/api/memories?limit=${limit}`);
    return raw.map(({ type, ...rest }) => ({ ...rest, memory_type: type ?? rest.memory_type ?? '' }));
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
    const raw = await this.api<ServerSearchResult[]>("POST", "/api/search", { query, top_k: topK });
    return raw.map(({ memory, score }) => {
      const { type, ...rest } = memory;
      return { memory: { ...rest, memory_type: type ?? rest.memory_type ?? '' }, score };
    });
  }

  async saveMemory(content: string, priority: string, type: string): Promise<void> {
    try {
      const result = await this.api<{ id: string }>('POST', '/api/memories', {
        content,
        priority,
        type,
        agent_id: 'obsidian',
        agent_type: 'note-editor',
        namespace: 'global',
      });
      new Notice(`Saved: ${result.id}`);
    } catch (e) {
      new Notice(`Save error: ${asMessage(e)}`);
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
      const memvaultId: unknown = fm?.memvault_id;
      if (typeof memvaultId === 'string' && memvaultId) {
        const updatedAt: unknown = fm?.memvault_updated_at;
        indexEntries.push({
          path: file.path,
          memvaultId,
          updatedAt: typeof updatedAt === 'string' ? updatedAt : '',
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
          await this.app.fileManager.trashFile(file);
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
      await this.app.workspace.revealLeaf(existing[0]);
      return;
    }
    const leaf = this.app.workspace.getRightLeaf(false);
    if (leaf) {
      await leaf.setViewState({ type: VIEW_TYPE, active: true });
      await this.app.workspace.revealLeaf(leaf);
    }
  }

  async loadSettings() {
    const data = (await this.loadData()) as Partial<MemVaultSettings> | null;
    this.settings = { ...DEFAULT_SETTINGS, ...data };
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
    el.createDiv({
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
    navigator.clipboard
      .writeText(content)
      .then(() => new Notice('Copied to clipboard'))
      .catch(() => new Notice('Copy failed — clipboard unavailable'));
  }
}

// ─── Insert Modal (search + insert into editor) ──────────────────

class MemVaultInsertModal extends SuggestModal<SearchResult> {
  plugin: MemVaultPlugin;
  editor: Editor;

  constructor(app: App, plugin: MemVaultPlugin, editor: Editor) {
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
    el.createDiv({
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
          } catch (e) {
            new Notice(`Failed: ${asMessage(e)}`);
          }
        }),
    );
  }

  onClose() {
    this.contentEl.empty();
  }
}

// ─── Sidebar View ────────────────────────────────────────────────

class MemVaultView extends ItemView {
  plugin: MemVaultPlugin;
  private refreshTimer: number | null = null;
  private appliedInterval = 0;
  private renderSeq = 0;
  private lastSignature: string | null = null;

  constructor(leaf: WorkspaceLeaf, plugin: MemVaultPlugin) {
    super(leaf);
    this.plugin = plugin;
  }

  getViewType() { return VIEW_TYPE; }
  getDisplayText() { return 'MemVault'; }
  getIcon() { return 'database'; }

  async onOpen() {
    await this.render();
    this.startAutoRefresh();
  }

  onClose() {
    this.stopAutoRefresh();
    return Promise.resolve();
  }

  refresh(): void {
    void this.render(true);
  }

  private stopAutoRefresh() {
    if (this.refreshTimer !== null) {
      window.clearInterval(this.refreshTimer);
      this.refreshTimer = null;
    }
  }

  private startAutoRefresh() {
    this.stopAutoRefresh();
    const interval = this.plugin.settings.refreshInterval * 1000;
    this.appliedInterval = interval;
    if (interval > 0) {
      this.refreshTimer = window.setInterval(() => void this.autoRefreshTick(), interval);
    }
  }

  /**
   * Re-arm the timer before rendering when the interval changed: declarative
   * settings write straight to plugin.settings with no save callback, so a
   * running timer would otherwise keep the old cadence (including 0 = off).
   */
  private async autoRefreshTick() {
    const desired = this.plugin.settings.refreshInterval * 1000;
    if (desired !== this.appliedInterval) {
      this.startAutoRefresh();
      if (desired <= 0) return;
    }
    await this.render();
  }

  /**
   * Fetch first, then swap the panel in one synchronous step. The previous
   * implementation cleared the panel before awaiting the request and rebuilt
   * every card on each tick, so the default 10s auto-refresh looked like the
   * whole page flashing. Skip the swap unless the payload actually changed,
   * and keep the user's scroll position when it does.
   */
  private async render(force = false) {
    const container = this.containerEl.children[1] as HTMLElement;
    const seq = ++this.renderSeq;

    let memories: Memory[] | null = null;
    let error: unknown = null;
    try {
      memories = await this.plugin.listMemories();
    } catch (e) {
      error = e;
    }
    // A newer render (manual refresh, delete) superseded this one.
    if (seq !== this.renderSeq) return;

    const signature = error
      ? `error:${asMessage(error)}`
      : `ok:${this.plugin.settings.refreshInterval}:${JSON.stringify(memories)}`;
    if (!force && signature === this.lastSignature) return;
    this.lastSignature = signature;

    const scrollTop = container.scrollTop;
    const next = createDiv();
    this.renderHeader(next);
    if (error) {
      this.renderConnectionError(next, error);
    } else {
      this.renderMemories(next, memories ?? []);
    }

    container.empty();
    container.appendChild(next);
    container.scrollTop = scrollTop;
  }

  private renderHeader(container: HTMLElement) {
    const header = container.createDiv({ cls: 'memvault-panel-header' });
    header.createSpan({ text: 'MemVault', cls: 'memvault-panel-title' });
    const syncBtn = header.createEl('button', { text: 'Sync', cls: 'mod-cta' });
    syncBtn.setAttr('aria-label', 'Sync memories to vault');
    syncBtn.onclick = async () => {
      try {
        await this.plugin.syncVaultFromServer();
      } catch (e) {
        new Notice(`Sync failed: ${asMessage(e)}`);
      }
    };
  }

  private renderMemories(container: HTMLElement, memories: Memory[]) {
    if (!memories.length) {
      container.createEl('p', { text: 'No memories stored.', cls: 'mod-muted' });
      return;
    }

    const list = container.createDiv({ cls: 'memvault-list' });
    for (const mem of memories) {
      this.renderMemoryItem(list, mem);
    }

    container.createEl('small', {
      text: `${memories.length} memories · auto-refresh ${this.plugin.settings.refreshInterval}s`,
      cls: 'mod-muted',
    });
  }

  private renderConnectionError(container: HTMLElement, error: unknown) {
    const errorBox = container.createDiv({ cls: 'memvault-connection-error' });
    errorBox.createEl('p', {
      text: `No usable MemVault server at ${this.plugin.settings.serverUrl}: ${asMessage(error)}`,
      cls: 'mod-warning',
    });
    errorBox.createEl('p', {
      text: 'Start the local backend, then retry:',
      cls: 'mod-muted',
    });
    errorBox.createEl('code', { text: 'memvault-cli dashboard' });
    errorBox.createEl('p', {
      text: 'Using a different port? Set Server URL in Settings and click Detect server.',
      cls: 'mod-muted',
    });
    const retry = errorBox.createEl('button', { text: 'Retry' });
    retry.onclick = () => void this.render(true);
  }

  private renderMemoryItem(container: HTMLElement, mem: Memory) {
    const item = container.createDiv({ cls: 'memvault-item' });
    item.setAttr('data-priority', mem.priority);

    // Header: priority + layer + type
    const header = item.createDiv({ cls: 'memvault-item-header' });

    const priority = (mem.priority || 'BACKGROUND').toUpperCase();
    header.createSpan({
      text: priority,
      cls: `memvault-priority memvault-priority-${priority.toLowerCase()}`,
    });
    header.createSpan({ text: mem.layer, cls: 'memvault-layer' });
    header.createSpan({ text: mem.memory_type, cls: 'memvault-type' });

    // Content
    item.createDiv({
      text: mem.content.slice(0, 120) + (mem.content.length > 120 ? '...' : ''),
      cls: 'memvault-item-content',
    });

    // Instruction (if different from content)
    if (mem.instruction && mem.instruction !== mem.content) {
      item.createDiv({
        text: `→ ${mem.instruction.slice(0, 100)}`,
        cls: 'memvault-instruction',
      });
    }

    // Skill meta
    if (mem.skill_meta) {
      const skill = item.createDiv({ cls: 'memvault-skill' });
      if (mem.skill_meta.trigger) {
        skill.createSpan({ text: mem.skill_meta.trigger });
      }
      if (mem.skill_meta.steps.length) {
        skill.createSpan({ text: ` · ${mem.skill_meta.steps.length} steps` });
      }
    }

    // Tags
    if (mem.tags.length) {
      const tags = item.createDiv({ cls: 'memvault-tags' });
      for (const tag of mem.tags) {
        tags.createSpan({ text: tag, cls: 'memvault-tag' });
      }
    }

    // Actions (management lives in the Web Dashboard / CLI — only Delete stays inline)
    const actions = item.createDiv({ cls: 'memvault-actions' });

    const deleteBtn = actions.createEl('button', { text: 'Delete', cls: 'memvault-delete' });
    deleteBtn.onclick = () => {
      new ConfirmModal(this.app, 'Delete this memory? This cannot be undone.', async () => {
        try {
          await this.plugin.deleteMemory(mem.id);
          new Notice('Deleted');
          void this.render(true);
        } catch (e) {
          new Notice(`Delete failed: ${asMessage(e)}`);
        }
      }).open();
    };
  }
}

// ─── Confirm Modal (delete flow — no native confirm()) ──────────

class ConfirmModal extends Modal {
  private message: string;
  private onConfirm: () => Promise<void>;

  constructor(app: App, message: string, onConfirm: () => Promise<void>) {
    super(app);
    this.message = message;
    this.onConfirm = onConfirm;
  }

  onOpen() {
    this.contentEl.createDiv({ text: this.message, cls: 'mod-warning' });
    new Setting(this.contentEl).addButton((b) =>
      b
        .setButtonText('Delete')
        // setWarning() is deprecated in 1.13.0 in favour of setDestructive(),
        // which does NOT exist at minAppVersion 1.7.2. The community review's
        // obsidianmd/no-unsupported-api rule is static — a runtime typeof guard
        // does not satisfy it — so the compatible call wins until
        // minAppVersion reaches 1.13.0.
        .setWarning()
        .onClick(async () => {
          await this.onConfirm();
          this.close();
        }),
    );
  }

  onClose() {
    this.contentEl.empty();
  }
}

// ─── Settings ────────────────────────────────────────────────────

// Dual support: getSettingDefinitions() (declarative settings, Obsidian 1.13.0+)
// is the current API and the only way settings show up in Obsidian's settings
// search; display() below stays as the fallback for older apps, which is the
// pattern the Obsidian docs prescribe for plugins with minAppVersion < 1.13.0.
// The framework renders declarations *instead of* display() whenever this
// returns a non-empty array, so the two paths never overlap — but they must
// list the same settings, and `control` bindings write straight to
// this.plugin.settings (no onChange/saveSettings plumbing needed).
export class MemVaultSettingTab extends PluginSettingTab {
  plugin: MemVaultPlugin;

  constructor(app: App, plugin: MemVaultPlugin) {
    super(app, plugin);
    this.plugin = plugin;
  }

  getSettingDefinitions(): SettingDefinitionItem[] {
    return [
      {
        name: 'Server URL',
        desc: 'MemVault REST backend. Start it with `memvault-cli dashboard` (default http://127.0.0.1:3777).',
        // Text field + button in one row has no declarative equivalent, so this
        // row stays imperative. `render` does not auto-save: write explicitly.
        render: (setting) => {
          let urlText: TextComponent | undefined;
          setting
            .addText((text) => {
              urlText = text;
              text
                .setPlaceholder(DEFAULT_SERVER_URL)
                .setValue(this.plugin.settings.serverUrl)
                .onChange(async (value) => {
                  this.plugin.settings.serverUrl = value;
                  await this.plugin.saveSettings();
                });
            })
            .addButton((button) =>
              button.setButtonText('Detect server').onClick(async () => {
                if (await this.plugin.detectAndSaveServer()) {
                  urlText?.setValue(this.plugin.settings.serverUrl);
                }
              }),
            );
        },
      },
      {
        name: 'Auto-refresh interval',
        desc: 'How often to refresh the sidebar (seconds, 0 to disable).',
        // Stored as a number; the old text field ran the value through
        // `parseInt(v) || 10`, which silently turned the documented 0 into 10.
        control: {
          type: 'number',
          key: 'refreshInterval',
          min: 0,
          placeholder: '10',
          defaultValue: DEFAULT_SETTINGS.refreshInterval,
        },
      },
      {
        name: 'API key',
        desc: 'Sent as X-MemVault-Api-Key for admin-protected REST routes (leave empty if the server has no admin key configured).',
        control: { type: 'text', key: 'apiKey', placeholder: '' },
      },
      {
        type: 'group',
        heading: 'Vault sync',
        items: [
          {
            name: 'Sync folder',
            desc: 'Vault folder that "Sync Memories to Vault" writes notes into.',
            control: {
              type: 'text',
              key: 'syncFolder',
              placeholder: 'MemVault',
              defaultValue: DEFAULT_SETTINGS.syncFolder,
              validate: (value) => (value.trim() ? undefined : 'Enter a folder name.'),
            },
          },
          {
            name: 'Delete orphaned notes on sync',
            desc: "If a synced note's memory no longer exists on the server, delete the local note too. Off by default so manual edits are never silently lost.",
            control: { type: 'toggle', key: 'syncDeleteOrphans' },
          },
        ],
      },
    ];
  }

  display() {
    const { containerEl } = this;
    containerEl.empty();

    new Setting(containerEl)
      .setName('Server URL')
      .setDesc('MemVault REST backend. Start it with `memvault-cli dashboard` (default http://127.0.0.1:3777).')
      .addText(text => text
        .setPlaceholder(DEFAULT_SERVER_URL)
        .setValue(this.plugin.settings.serverUrl)
        .onChange(async (value) => {
          this.plugin.settings.serverUrl = value;
          await this.plugin.saveSettings();
        }))
      .addButton(button => button
        .setButtonText('Detect server')
        .onClick(async () => {
          if (await this.plugin.detectAndSaveServer()) this.display();
        }));

    new Setting(containerEl)
      .setName('Auto-refresh interval')
      .setDesc('How often to refresh the sidebar (seconds, 0 to disable)')
      .addText(text => text
        .setPlaceholder('10')
        .setValue(String(this.plugin.settings.refreshInterval))
        .onChange(async (value) => {
          // 0 is a real value here (0 = disable), so don't fall back on it.
          const parsed = Number.parseInt(value, 10);
          this.plugin.settings.refreshInterval = Number.isFinite(parsed)
            ? Math.max(0, parsed)
            : DEFAULT_SETTINGS.refreshInterval;
          await this.plugin.saveSettings();
        }));

    new Setting(containerEl)
      .setName('API key')
      .setDesc('Sent as X-MemVault-Api-Key for admin-protected REST routes (leave empty if the server has no admin key configured)')
      .addText(text => text
        .setPlaceholder('')
        .setValue(this.plugin.settings.apiKey)
        .onChange(async (value) => {
          this.plugin.settings.apiKey = value;
          await this.plugin.saveSettings();
        }));

    new Setting(containerEl).setName('Vault sync').setHeading();

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
      .setDesc("If a synced note's memory no longer exists on the server, delete the local note too. Off by default so manual edits are never silently lost.")
      .addToggle(toggle => toggle
        .setValue(this.plugin.settings.syncDeleteOrphans)
        .onChange(async (value) => {
          this.plugin.settings.syncDeleteOrphans = value;
          await this.plugin.saveSettings();
        }));
  }
}
