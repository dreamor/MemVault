/**
 * One-way DB → vault Markdown sync. Pure functions only — no Obsidian API
 * calls here, so this module is unit-testable without mocking `obsidian`.
 * The `main.ts` MemVaultPlugin does the actual vault I/O and calls into this
 * module for the decision logic and content generation.
 */

/** Nested `skill_meta` payload; mirrors the Rust `SkillMeta`. */
export interface SkillMeta {
  trigger: string | null;
  steps: string[];
  verification: string | null;
  version: number;
}

/**
 * The complete REST memory shape, field-for-field with `memory_to_json` in
 * crates/memvault-mcp/src/rest_api.rs, plus the one rename the wire adapter in
 * `main.ts` performs (`type` → `memory_type`, so `folderFor` and the note body
 * stay aligned with the rest of the plugin).
 *
 * Declared in full rather than as the subset the plugin happens to read today.
 * `contracts/memory-json-keys.json` is checked against BOTH this interface and
 * the Rust serialiser, so a renamed or removed field fails CI instead of
 * arriving here as `undefined` and being written into frontmatter as an empty
 * value. Keep the two in step through that file, not by eyeballing.
 */
export interface RemoteMemory {
  id: string;
  content: string;
  instruction: string | null;
  priority: string;
  /** Wire name is `type`. */
  memory_type: string;
  namespace: string;
  tags: string[];
  layer: string;
  human_reviewed: boolean;
  ai_generated: boolean;
  confidence: number;
  access_count: number;
  decay_score: number;
  created_at: string;
  updated_at: string;
  /** Event time — when it happened, as opposed to when it was recorded. */
  occurred_at: string | null;
  /** Once past, the memory is out of effect (§9.6). */
  expires_at: string | null;
  archived_at: string | null;
  source_agent: string;
  visibility: string;
  superseded_by: string | null;
  proposes_supersede_of: string | null;
  skill_meta: SkillMeta | null;
  friction_evidence: string | null;
}

/**
 * Whether a memory is in effect, derived from the status-bearing fields rather
 * than stored anywhere. This is the single answer to "is this note live?",
 * which the mirror previously could not express at all: an expired, archived,
 * superseded or still-proposed memory rendered identically to a current one.
 */
export type MemoryStatus = 'active' | 'expired' | 'archived' | 'superseded' | 'proposal';

export interface FrontmatterIndexEntry {
  path: string;
  memvaultId: string;
  updatedAt: string;
  /**
   * The `status` this note was written with. Absent for notes produced before
   * status existed, which is what makes the first post-upgrade sync rewrite
   * them once and then converge.
   */
  status?: string;
}

export type SyncAction = 'create' | 'update' | 'skip';

function parseTime(value: string | null | undefined): number | null {
  if (!value) return null;
  const t = Date.parse(value);
  return Number.isNaN(t) ? null : t;
}

/**
 * Derive a memory's status. Precedence is deliberate:
 *
 * - `proposal` first, because it is the only state whose resolution has a side
 *   effect on a *different* memory — approving it archives its target.
 * - `superseded` before `archived`, because supersession archives its target,
 *   so a superseded memory usually carries both and `superseded` is the more
 *   specific of the two.
 * - `expired` last: it is the only one computed by comparing a timestamp
 *   against the clock rather than by reading a field that is either set or not.
 *
 * An unparseable `expires_at` is treated as not-expired. Guessing "expired"
 * from corrupt data would hide a live memory, which is the worse error.
 */
export function memoryStatus(memory: RemoteMemory, now = Date.now()): MemoryStatus {
  if (memory.proposes_supersede_of) return 'proposal';
  if (memory.superseded_by) return 'superseded';
  if (memory.archived_at) return 'archived';
  const expiry = parseTime(memory.expires_at);
  if (expiry !== null && expiry <= now) return 'expired';
  return 'active';
}

/** Decide whether a remote memory needs a new note, an overwrite, or nothing. */
export function decideAction(
  memory: RemoteMemory,
  existing: FrontmatterIndexEntry | undefined,
  now = Date.now(),
): SyncAction {
  if (!existing) return 'create';
  // A memory can leave `active` without `updated_at` moving at all: expiry is a
  // comparison against the clock, and archive/supersede are written by some
  // *other* memory's approval. Comparing the derived status against the one the
  // note was written with is what stops the mirror from showing a dead memory
  // as live forever — `updated_at` alone never notices.
  if (existing.status !== memoryStatus(memory, now)) return 'update';
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

/**
 * Typed sub-folder for a memory, per the three-memory taxonomy in
 * DESIGN.md §8: episodic → 10-Daily, semantic entities → 20-Entities,
 * long-term facts/preferences → 30-Memories, procedural skills → 40-Skills.
 */
export function folderFor(memory: RemoteMemory): string {
  switch (memory.memory_type.toLowerCase()) {
    case 'episode':
      return '10-Daily';
    case 'entity':
      return '20-Entities';
    case 'skill':
      return '40-Skills';
    default:
      return '30-Memories';
  }
}

function yamlList(items: string[]): string {
  return `[${items.map((t) => JSON.stringify(t)).join(', ')}]`;
}

/**
 * Status-bearing fields written to frontmatter only when set. Kept as data so
 * the order here is the order in the note, and so adding one cannot forget the
 * null guard — every entry is an `Option` on the Rust side and null is the
 * common case, not the exception.
 */
const STATUS_FIELDS = [
  'occurred_at',
  'expires_at',
  'archived_at',
  'superseded_by',
  'proposes_supersede_of',
] as const;

export function buildFrontmatter(memory: RemoteMemory, now = Date.now()): string {
  const lines = [
    '---',
    `memvault_id: ${memory.id}`,
    `memvault_updated_at: ${memory.updated_at}`,
    // Derived, not stored: `status` is the one property a Dataview query or a
    // search can filter on without re-implementing the precedence rules above.
    // The raw timestamps are emitted alongside it so a query can also recompute
    // liveness live — this frontmatter is a snapshot taken at sync time, and a
    // memory that expires later still reads `active` until the next sync.
    `status: ${memoryStatus(memory, now)}`,
    `priority: ${memory.priority}`,
    `type: ${memory.memory_type}`,
    `layer: ${memory.layer}`,
    `tags: ${yamlList(memory.tags)}`,
    `namespace: ${memory.namespace}`,
    `human_reviewed: ${memory.human_reviewed}`,
  ];
  for (const key of STATUS_FIELDS) {
    const value = memory[key];
    if (value) lines.push(`${key}: ${value}`);
  }
  lines.push('---');
  return lines.join('\n');
}

/**
 * Reading-mode banner for a memory that is not in effect. Frontmatter alone
 * does not carry the signal: Obsidian collapses properties by default and
 * hides them entirely in reading mode, which is exactly how an expired memory
 * came to look identical to a live one. `active` gets no banner, so the common
 * case stays as clean as it was before status existed.
 */
export function statusBanner(memory: RemoteMemory, now = Date.now()): string | null {
  switch (memoryStatus(memory, now)) {
    case 'active':
      return null;
    case 'expired':
      return (
        `> [!warning] Expired\n` +
        `> This memory expired at ${memory.expires_at} and is no longer in effect.`
      );
    case 'archived':
      return `> [!info] Archived\n> This memory was archived at ${memory.archived_at}.`;
    case 'superseded':
      return (
        `> [!warning] Superseded\n` +
        `> Replaced by \`${memory.superseded_by}\`. Below is the old wording.`
      );
    case 'proposal':
      return (
        `> [!todo] Pending proposal\n` +
        `> Proposes to supersede \`${memory.proposes_supersede_of}\`.` +
        ` Approving it archives that memory.`
      );
  }
}

export function buildNoteContent(memory: RemoteMemory, now = Date.now()): string {
  const banner = statusBanner(memory, now);
  let body = banner ? `${banner}\n\n${memory.content}` : memory.content;
  if (memory.instruction) {
    body += `\n\n## Instruction\n${memory.instruction}`;
  }
  return `${buildFrontmatter(memory, now)}\n\n${body}\n`;
}
