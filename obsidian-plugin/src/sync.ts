/**
 * One-way DB → vault Markdown sync. Pure functions only — no Obsidian API
 * calls here, so this module is unit-testable without mocking `obsidian`.
 * The `main.ts` MemVaultPlugin does the actual vault I/O and calls into this
 * module for the decision logic and content generation.
 */

export interface RemoteMemory {
  id: string;
  content: string;
  instruction: string | null;
  priority: string;
  memory_type: string;
  namespace: string;
  tags: string[];
  layer: string;
  human_reviewed: boolean;
  updated_at: string;
}

export interface FrontmatterIndexEntry {
  path: string;
  memvaultId: string;
  updatedAt: string;
}

export type SyncAction = 'create' | 'update' | 'skip';

/** Decide whether a remote memory needs a new note, an overwrite, or nothing. */
export function decideAction(
  memory: RemoteMemory,
  existing: FrontmatterIndexEntry | undefined,
): SyncAction {
  if (!existing) return 'create';
  const existingTime = Date.parse(existing.updatedAt);
  const remoteTime = Date.parse(memory.updated_at);
  // Either side failing to parse forces an update rather than a silent
  // skip — a malformed *remote* timestamp used to fall through to 'skip'
  // and never sync again, since `NaN > existingTime` is false.
  if (Number.isNaN(existingTime) || Number.isNaN(remoteTime) || remoteTime > existingTime) {
    return 'update';
  }
  return 'skip';
}

/** Local notes whose `memvault_id` no longer exists in the remote set. */
export function detectOrphans(
  indexEntries: FrontmatterIndexEntry[],
  remoteIds: Set<string>,
): FrontmatterIndexEntry[] {
  return indexEntries.filter((e) => !remoteIds.has(e.memvaultId));
}

export function buildIdIndex(
  entries: FrontmatterIndexEntry[],
): Map<string, FrontmatterIndexEntry> {
  const map = new Map<string, FrontmatterIndexEntry>();
  for (const e of entries) {
    map.set(e.memvaultId, e);
  }
  return map;
}

/** Filesystem-safe, human-scannable slug. `memvault_id` frontmatter remains the
 * authoritative match key, so renames of the generated file are safe. */
export function slugify(text: string, maxLen = 60): string {
  const base = text
    .toLowerCase()
    .trim()
    .replace(/[^a-z0-9一-龥]+/g, '-')
    .replace(/^-+|-+$/g, '');
  return (base || 'memory').slice(0, maxLen);
}

export function fileNameFor(memory: RemoteMemory): string {
  const shortId = memory.id.replace(/^mem_/, '').slice(0, 8);
  return `${slugify(memory.content)}--${shortId}.md`;
}

function yamlList(items: string[]): string {
  return `[${items.map((t) => JSON.stringify(t)).join(', ')}]`;
}

export function buildFrontmatter(memory: RemoteMemory): string {
  const lines = [
    '---',
    `memvault_id: ${memory.id}`,
    `memvault_updated_at: ${memory.updated_at}`,
    `priority: ${memory.priority}`,
    `type: ${memory.memory_type}`,
    `layer: ${memory.layer}`,
    `tags: ${yamlList(memory.tags)}`,
    `namespace: ${memory.namespace}`,
    `human_reviewed: ${memory.human_reviewed}`,
    '---',
  ];
  return lines.join('\n');
}

export function buildNoteContent(memory: RemoteMemory): string {
  let body = memory.content;
  if (memory.instruction) {
    body += `\n\n## Instruction\n${memory.instruction}`;
  }
  return `${buildFrontmatter(memory)}\n\n${body}\n`;
}
