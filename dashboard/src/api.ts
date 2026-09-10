/**
 * MemVault Web Dashboard — REST API client.
 *
 * Single data layer for the dashboard. Talks to the `memvault-mcp` REST server
 * (default: same-origin, e.g. `memvault-mcp --transport http --serve-web dist`).
 * `API_BASE` can be overridden via `VITE_MEMVAULT_API_BASE` for CDN / dev hosting.
 *
 * Every REST response is wrapped as `{ ok, data, error }` — the envelope is
 * unwrapped here so callers just get `data` and rely on thrown errors.
 */

const API_BASE = import.meta.env.VITE_MEMVAULT_API_BASE ?? "";
const LS_API_KEY = "memvault.apiKey";
const LS_AGENT_ID = "memvault.agentId";

export interface MemoryView {
  id: string;
  memory_type: string;
  content: string;
  instruction: string | null;
  priority: string;
  namespace: string;
  tags: string[];
  source_agent_id: string;
  confidence: number;
  human_reviewed: boolean;
  decay_score: number;
  access_count: number;
  layer: string;
  skill_meta: SkillMetaView | null;
  /** "scoped" (this agent only) or "shared" (team-visible pool). */
  visibility: string;
  superseded_by: string | null;
  created_at: string;
  updated_at: string;
  /** Friction-signal summary attached when a Stop-hook extract passed the
   * friction gate (see memvault-core::friction) — absent for memories not
   * produced that way. */
  friction_evidence?: string | null;
}

export interface SkillMetaView {
  trigger: string | null;
  steps: string[];
  verification: string | null;
  version: number;
}

export interface RelationView {
  subject_id: string;
  predicate: string;
  object_id: string | null;
  object_text: string | null;
  line: string;
}

export interface SearchResultView {
  memory: MemoryView;
  score: number;
  /** Actual retrieval mode used by the backend (e.g. degraded to keyword). */
  searchMode?: string;
  /** Recall provenance tags, e.g. ["kw#2", "vec#1"]. */
  hitSources?: string[];
  /** One-hop relation neighborhood, present only when `expandRelations` was requested. */
  relations?: RelationView[];
}

export interface StatsView {
  total: number;
  must_count: number;
  reference_count: number;
  reviewed_count: number;
  agents: string[];
  namespaces: string[];
  layers: { l0: number; l1: number; l2: number; l3: number };
  skills: number;
}

export interface ComplianceReport {
  inject_session_id: string;
  agent_id: string;
  total_injected: number;
  must_followed: number;
  must_violated: number;
  ref_followed: number;
  ref_violated: number;
  pending: number;
  compliance_rate: number;
}

export interface ComplianceSummary {
  total_sessions: number;
  overall_rate: number;
  must_rate: number;
  recent_sessions: ComplianceReport[];
}

interface Envelope<T> {
  ok: boolean;
  data?: T | null;
  error?: string | null;
}

// ── Credentials ─────────────────────────────────────────────────────────

export function getApiKey(): string {
  return localStorage.getItem(LS_API_KEY) ?? "";
}

export function setApiKey(key: string): void {
  if (key) localStorage.setItem(LS_API_KEY, key);
  else localStorage.removeItem(LS_API_KEY);
}

export function getAgentId(): string {
  return localStorage.getItem(LS_AGENT_ID) ?? "admin";
}

export function setAgentId(id: string): void {
  if (id) localStorage.setItem(LS_AGENT_ID, id);
  else localStorage.removeItem(LS_AGENT_ID);
}

// ── Low-level helpers ───────────────────────────────────────────────────

/** Field names differ between the REST JSON and the legacy desktop MemoryView. */
function toMemoryView(j: any): MemoryView {
  const { type, source_agent, ...rest } = j;
  return {
    ...rest,
    memory_type: type,
    source_agent_id: source_agent,
  } as MemoryView;
}

async function request<T>(method: string, path: string, body?: unknown): Promise<T> {
  const headers: Record<string, string> = {
    "X-MemVault-Agent-Id": getAgentId(),
  };
  if (body !== undefined) headers["Content-Type"] = "application/json";
  const apiKey = getApiKey();
  if (apiKey) headers["X-MemVault-Api-Key"] = apiKey;

  const resp = await fetch(`${API_BASE}${path}`, {
    method,
    headers,
    body: body === undefined ? undefined : JSON.stringify(body),
  });

  const json: Envelope<T> = await resp.json().catch(() => ({ ok: false, error: `HTTP ${resp.status}` }));
  if (!resp.ok || !json.ok || json.data === null || json.data === undefined) {
    throw new Error(json.error || `Request failed: ${resp.status}`);
  }
  return json.data;
}

/** /health is a plain-text `ok` (no envelope). */
export async function health(): Promise<boolean> {
  const resp = await fetch(`${API_BASE}/health`);
  return resp.ok;
}

// ── Dashboard API ───────────────────────────────────────────────────────

export async function listMemories(p: {
  namespace?: string;
  limit: number;
  offset: number;
}): Promise<MemoryView[]> {
  const qs = new URLSearchParams();
  if (p.namespace) qs.set("namespace", p.namespace);
  qs.set("limit", String(p.limit));
  qs.set("offset", String(p.offset));
  const data = await request<MemoryView[]>("GET", `/api/memories?${qs.toString()}`);
  return data.map(toMemoryView);
}

export async function getStats(): Promise<StatsView> {
  return await request<StatsView>("GET", "/api/stats");
}

/**
 * Full pending-review list, independent of the paginated/namespace-filtered
 * Memories tab — the Review tab must see every pending memory, not just
 * whatever page happens to be loaded.
 */
export async function getInbox(limit = 1000): Promise<MemoryView[]> {
  const data = await request<{ memories: any[]; total: number }>(
    "GET",
    `/api/inbox?limit=${limit}`,
  );
  return data.memories.map(toMemoryView);
}

export async function searchMemories(p: {
  query: string;
  topK?: number;
  mode?: string;
  namespace?: string;
  expandRelations?: boolean;
}): Promise<SearchResultView[]> {
  const data = await request<any[]>("POST", "/api/search", {
    query: p.query,
    top_k: p.topK ?? 20,
    mode: p.mode ?? "keyword",
    namespace: p.namespace,
    expand_relations: p.expandRelations ?? false,
  });
  return data.map((r) => ({
    memory: toMemoryView(r.memory),
    score: r.score,
    searchMode: r.search_mode,
    hitSources: r.hit_sources,
    relations: r.relations,
  }));
}

export interface CreateMemoryInput {
  content: string;
  instruction?: string | null;
  priority: string;
  memory_type: string;
  namespace: string;
  tags: string[];
  /** "scoped" (default, this agent only) or "shared" (team-visible pool). */
  visibility?: string | null;
  skill_trigger?: string | null;
  skill_steps?: string[] | null;
  skill_verification?: string | null;
}

export async function createMemory(v: CreateMemoryInput): Promise<{ id: string; embedded: boolean }> {
  const { memory_type, skill_steps, ...rest } = v;
  return await request<{ id: string; embedded: boolean }>("POST", "/api/memories", {
    ...rest,
    type: memory_type,
    // Backend field is `Vec<String>` with a default, not `Option` — send [] rather than null/undefined.
    skill_steps: skill_steps ?? [],
    agent_id: "dashboard",
    agent_type: "web-dashboard",
    // Manual human save — goes straight to the list, not the review queue.
    human_reviewed: true,
    ai_generated: false,
  });
}

export interface UpdateMemoryInput {
  content: string;
  instruction?: string | null;
  priority: string;
  memory_type: string;
  namespace: string;
  tags: string[];
  skill_trigger?: string | null;
  skill_steps?: string[] | null;
  skill_verification?: string | null;
  /** "scoped" or "shared"; omit to leave the memory's visibility untouched. */
  visibility?: string | null;
}

export async function updateMemory(id: string, patch: UpdateMemoryInput): Promise<MemoryView> {
  const { memory_type, ...rest } = patch;
  const data = await request<any>("PUT", `/api/memories/${id}`, {
    ...rest,
    type: memory_type,
    skill_trigger: rest.skill_trigger ?? null,
    skill_steps: rest.skill_steps ?? null,
    skill_verification: rest.skill_verification ?? null,
  });
  return toMemoryView(data);
}

export async function approveMemory(id: string): Promise<void> {
  await request("POST", `/api/inbox/${id}/approve`, {});
}

/** Hard-delete a memory outright (Memories tab / detail panel "Delete"). */
export async function rejectMemory(id: string): Promise<void> {
  await request("DELETE", `/api/memories/${id}`);
}

/** Reject a pending-review memory via the dedicated inbox endpoint (Review tab). */
export async function rejectPendingMemory(id: string): Promise<void> {
  await request("POST", `/api/inbox/${id}/reject`, {});
}

/** Quick-edit a pending-review memory's content/instruction; marks it reviewed. */
export async function editPendingMemory(
  id: string,
  patch: { editedContent?: string; editedInstruction?: string },
): Promise<void> {
  await request("POST", `/api/inbox/${id}/edit`, {
    edited_content: patch.editedContent,
    edited_instruction: patch.editedInstruction,
  });
}

/** Mark a memory as superseded by another (archives the old one, doesn't delete it). */
export async function supersedeMemory(id: string, replacementId: string): Promise<void> {
  await request("POST", `/api/memories/${id}/supersede`, { replacement_id: replacementId });
}

export async function runPromote(): Promise<{ promoted_to_l2: number; promoted_to_l3: number }> {
  return await request("POST", "/api/promote");
}

export async function runDecay(): Promise<{ updated: number; archived: number }> {
  return await request("POST", "/api/decay");
}

export async function runDedup(): Promise<{ unique_count: number; duplicate_count: number }> {
  const data = await request<{ unique: number; duplicates: number }>("POST", "/api/dedup");
  return { unique_count: data.unique, duplicate_count: data.duplicates };
}

export async function getComplianceSummary(limit = 10): Promise<ComplianceSummary> {
  return await request<ComplianceSummary>("GET", `/api/compliance/summary?limit=${limit}`);
}

export async function getComplianceSession(sessionId: string): Promise<ComplianceReport> {
  return await request<ComplianceReport>(
    "GET",
    `/api/compliance/session?session_id=${encodeURIComponent(sessionId)}`,
  );
}

// ── Confirm read (bump access_count; decay weighs access recency) ──────

/** Mark memories as read — refreshes their access recency so decay is
 * less likely to archive them while they are still actively used. */
export async function confirmRead(memoryIds: string[]): Promise<{ confirmed: number }> {
  return await request<{ confirmed: number }>("POST", "/api/confirm-read", {
    memory_ids: memoryIds,
  });
}

// ── Effectiveness (auto-judged injection quality) ──────────────────────

/** Automatic effectiveness verdicts for injected memories: useful / neutral /
 * harmful / insufficient_context rates, judged server-side from
 * `record_outcome` pairing — independent of manual `report_compliance`. */
export interface EffectivenessSummary {
  useful: number;
  neutral: number;
  harmful: number;
  insufficient: number;
  unjudged: number;
  usefulness_rate: number;
  harmful_rate: number;
  coverage: number;
}

export async function getEffectivenessReport(p: {
  agentId?: string;
  limit?: number;
}): Promise<EffectivenessSummary> {
  const qs = new URLSearchParams();
  if (p.agentId) qs.set("agent_id", p.agentId);
  qs.set("limit", String(p.limit ?? 200));
  return await request<EffectivenessSummary>("GET", `/api/effectiveness?${qs.toString()}`);
}

// ── Session preview (what an agent would receive on connect) ───────────

/** Same pipeline as the MCP `session_start` tool: MUST/REF instructions,
 * semantic candidates, and explainable drop reasons (`skipped`), so an
 * admin can see exactly what an agent gets — without connecting as it. */
export interface SessionPreview {
  results: SearchResultView[];
  count: number;
  format: string;
  agentProfile: string;
  skipped: { id: string; reason: string }[];
  skippedChannel: string | null;
  note: string | null;
  injectSessionId: string | null;
}

export async function previewSession(p: {
  agentId: string;
  contextHint?: string;
  format?: string;
}): Promise<SessionPreview> {
  const data = await request<any>("POST", "/api/session", {
    agent_id: p.agentId,
    context_hint: p.contextHint || null,
    format: p.format || null,
  });
  return {
    results: (data.results ?? []).map((r: any) => ({
      memory: toMemoryView(r.memory),
      score: r.score,
      searchMode: r.search_mode,
      hitSources: r.hit_sources,
    })),
    count: data.count,
    format: data.format,
    agentProfile: data.agent_profile,
    skipped: data.skipped ?? [],
    skippedChannel: data.skipped_channel ?? null,
    note: data.note ?? null,
    injectSessionId: data.inject_session_id ?? null,
  };
}

// ── System / diagnostics (Doctor, Capabilities, Metrics) ───────────────

export interface FindingItem {
  id: string;
  detail: string;
}

export interface DoctorFinding {
  check: string;
  severity: "warn" | "info";
  count: number;
  items: FindingItem[];
}

export interface DoctorReport {
  total_memories: number;
  findings: DoctorFinding[];
}

/** Full read-only hygiene scan — O(n) over the store, so only call this on
 * an explicit user action, never on tab load / polling. */
export async function runDoctor(): Promise<DoctorReport> {
  return await request<DoctorReport>("GET", "/api/doctor");
}

export interface CapabilityStatus {
  name: string;
  available: boolean;
  note: string;
}

export async function getCapabilities(): Promise<CapabilityStatus[]> {
  return await request<CapabilityStatus[]>("GET", "/api/capabilities");
}

export interface MetricSample {
  name: string;
  labels: Record<string, string>;
  value: number;
}

/** Minimal Prometheus text-exposition parser — good enough for the flat
 * counters MemVault exports (no histograms/summaries to unpack). */
function parsePrometheusText(text: string): MetricSample[] {
  const samples: MetricSample[] = [];
  for (const rawLine of text.split("\n")) {
    const line = rawLine.trim();
    if (!line || line.startsWith("#")) continue;
    const match = line.match(/^([a-zA-Z_:][a-zA-Z0-9_:]*)(\{[^}]*\})?\s+(-?[0-9.eE+-]+)$/);
    if (!match) continue;
    const [, name, labelStr, valueStr] = match;
    const labels: Record<string, string> = {};
    if (labelStr) {
      for (const pair of labelStr.slice(1, -1).split(",")) {
        const eq = pair.indexOf("=");
        if (eq === -1) continue;
        const key = pair.slice(0, eq).trim();
        let val = pair.slice(eq + 1).trim();
        if (val.startsWith('"') && val.endsWith('"')) val = val.slice(1, -1);
        labels[key] = val;
      }
    }
    const value = Number(valueStr);
    if (!Number.isNaN(value)) samples.push({ name, labels, value });
  }
  return samples;
}

/** `/metrics` is plain Prometheus text (no envelope) — same endpoint `sf`/scrapers use. */
export async function getMetrics(): Promise<MetricSample[]> {
  const resp = await fetch(`${API_BASE}/metrics`);
  if (!resp.ok) throw new Error(`Request failed: ${resp.status}`);
  return parsePrometheusText(await resp.text());
}

// ── Extraction (paste text → candidate memories) ───────────────────────

export interface ExtractedCandidate {
  content: string;
  instruction: string | null;
  type: string;
  priority: string;
  tags: string[];
  confidence: number;
}

export interface ExtractionCoverage {
  input_lines: number;
  empty_lines: number;
  extracted_lines: number;
  no_signal_lines: number;
}

export interface ExtractResult {
  memories: ExtractedCandidate[];
  /** Only present for mode="rule" — llm mode has no line-coverage concept. */
  coverage: ExtractionCoverage | null;
  savedIds: string[];
}

export async function extractMemories(p: {
  text: string;
  mode?: "rule" | "llm";
  assistantText?: string;
  autoSave?: boolean;
}): Promise<ExtractResult> {
  const data = await request<any>("POST", "/api/extract", {
    text: p.text,
    mode: p.mode ?? "rule",
    assistant_text: p.assistantText || undefined,
    auto_save: p.autoSave ?? false,
  });
  return {
    memories: data.memories,
    coverage: data.coverage ?? null,
    savedIds: data.saved_ids ?? [],
  };
}

// ── Episodic memory (task outcomes & lessons) ──────────────────────────

export type OutcomeStatus = "success" | "failure" | "partial";

export interface EpisodeView {
  memory_id: string;
  task: string;
  task_type: string | null;
  status: OutcomeStatus;
  cause: string | null;
  lesson: string | null;
  lesson_memory_id: string | null;
  occurred_at: string;
}

export interface RecordOutcomeInput {
  task: string;
  status: OutcomeStatus;
  cause?: string;
  task_type?: string;
  namespace?: string;
  tags?: string[];
}

export interface RecordOutcomeResult {
  id: string;
  outcome: string;
  embedded: boolean;
  lesson: {
    lesson: string;
    source: string;
    memory_id: string;
    escalation_hint: string | null;
  } | null;
}

/** Report the outcome of an executed task. Failures/partials are reflected
 * into lessons server-side; the response carries the distilled lesson. */
export async function recordOutcome(input: RecordOutcomeInput): Promise<RecordOutcomeResult> {
  return await request<RecordOutcomeResult>("POST", "/api/outcome", {
    task: input.task,
    status: input.status,
    cause: input.cause || null,
    task_type: input.task_type || null,
    namespace: input.namespace || "global",
    tags: input.tags ?? [],
    agent_id: "dashboard",
    agent_type: "web-dashboard",
  });
}

export interface EpisodeFilter {
  task_type?: string;
  status?: OutcomeStatus;
  namespace?: string;
  limit?: number;
}

export async function listEpisodes(filter: EpisodeFilter = {}): Promise<EpisodeView[]> {
  const qs = new URLSearchParams();
  if (filter.task_type) qs.set("task_type", filter.task_type);
  if (filter.status) qs.set("status", filter.status);
  if (filter.namespace) qs.set("namespace", filter.namespace);
  qs.set("limit", String(filter.limit ?? 100));
  const data = await request<{ episodes: EpisodeView[]; count: number }>(
    "GET",
    `/api/episodes?${qs.toString()}`,
  );
  return data.episodes;
}

// ── Export / Import ────────────────────────────────────────────────────

export interface ExportFile {
  filename: string;
  content: string;
}

export interface ExportResult {
  format: "json" | "markdown";
  content?: string;
  files?: ExportFile[];
}

export async function exportMemories(p: {
  format?: "json" | "markdown";
  namespace?: string;
}): Promise<ExportResult> {
  const qs = new URLSearchParams();
  qs.set("format", p.format ?? "json");
  if (p.namespace) qs.set("namespace", p.namespace);
  return await request<ExportResult>("GET", `/api/export?${qs.toString()}`);
}

export interface ImportSkipped {
  filename: string;
  reason: string;
}

export interface ImportResult {
  imported: number;
  skipped?: ImportSkipped[];
}

export async function importMemories(p: {
  format: "json" | "markdown";
  content?: string;
  files?: ExportFile[];
}): Promise<ImportResult> {
  return await request<ImportResult>("POST", "/api/import", {
    format: p.format,
    content: p.content,
    files: p.files,
  });
}

// ── Backup (binary file download, not the JSON envelope) ───────────────

export async function createBackup(): Promise<{ blob: Blob; filename: string }> {
  const headers: Record<string, string> = { "X-MemVault-Agent-Id": getAgentId() };
  const apiKey = getApiKey();
  if (apiKey) headers["X-MemVault-Api-Key"] = apiKey;

  const resp = await fetch(`${API_BASE}/api/backup`, { method: "POST", headers });
  if (!resp.ok) {
    const err = await resp.json().catch(() => null);
    throw new Error(err?.error || `Request failed: ${resp.status}`);
  }
  const disposition = resp.headers.get("content-disposition") ?? "";
  const match = disposition.match(/filename="([^"]+)"/);
  const filename = match ? match[1] : "memvault-backup.db";
  const blob = await resp.blob();
  return { blob, filename };
}

// ── Checkpoints / Restore ───────────────────────────────────────────────

export interface CheckpointEntry {
  history_id: number;
  memory_id: string;
  operation: string;
  changed_at: string;
}

/** Omit `memoryId` for the global recent-changes list across all memories. */
export async function listCheckpoints(memoryId?: string, limit = 20): Promise<CheckpointEntry[]> {
  const path = memoryId ? `/api/memories/${memoryId}/checkpoints` : "/api/checkpoints";
  return await request<CheckpointEntry[]>("GET", `${path}?limit=${limit}`);
}

export async function restoreCheckpoint(historyId: number): Promise<MemoryView> {
  const data = await request<any>("POST", `/api/checkpoints/${historyId}/restore`, {});
  return toMemoryView(data);
}

// ── SOP skill import ─────────────────────────────────────────────────────

export interface ImportedSkill {
  title: string;
  id: string;
  steps: number;
}

export interface ImportSkillsResult {
  imported: ImportedSkill[];
  skipped_no_steps: number;
}

export async function importSkills(p: {
  markdown: string;
  fallbackTitle?: string;
  namespace?: string;
  approve?: boolean;
}): Promise<ImportSkillsResult> {
  return await request<ImportSkillsResult>("POST", "/api/skills/import", {
    markdown: p.markdown,
    fallback_title: p.fallbackTitle,
    namespace: p.namespace,
    approve: p.approve ?? false,
  });
}

// ── Cold-start cross-agent memory import ────────────────────────────────
//
// Wraps the server-local file detection the `memvault import-agent` CLI
// command uses — only meaningful when this dashboard talks to a
// `memvault-mcp` running on the same machine as the agent being imported
// from (same trust boundary as backup/export).

export interface AgentScanResult {
  agent_key: string;
  display_name: string;
  found: boolean;
  paths: string[];
}

export async function scanAgentImport(): Promise<AgentScanResult[]> {
  return await request<AgentScanResult[]>("GET", "/api/agents/import/scan");
}

export interface SkippedFile {
  path: string;
  reason: string;
}

export interface AgentImportCandidate {
  content: string;
  instruction: string | null;
  type: string;
  priority: string;
  tags: string[];
  confidence: number;
  namespace: string;
  raw_excerpt: string;
  parse_confidence: string;
  duplicate_of: string | null;
}

export interface AgentImportPreview {
  agent_key: string;
  display_name: string;
  files_scanned: number;
  files_skipped: SkippedFile[];
  candidates: AgentImportCandidate[];
}

export async function previewAgentImport(p: {
  agent: string;
  path?: string;
  namespace?: string;
}): Promise<AgentImportPreview> {
  return await request<AgentImportPreview>("POST", "/api/agents/import/preview", p);
}

export interface AgentImportRunResult {
  agent_key: string;
  display_name: string;
  files_scanned: number;
  files_skipped: SkippedFile[];
  imported: { id: string; content: string }[];
  duplicates_skipped: number;
}

/** Always saves unreviewed (`human_reviewed=false`) — no `approve` bypass
 * from the web, unlike the CLI's `--approve`. Results land in the Review inbox. */
export async function runAgentImport(p: {
  agent: string;
  path?: string;
  namespace?: string;
}): Promise<AgentImportRunResult> {
  return await request<AgentImportRunResult>("POST", "/api/agents/import/run", p);
}

// ── Agent registry (read-only) ──────────────────────────────────────────

export interface InjectRulesView {
  max_memories: number;
  token_budget: number;
  priority_order: string[];
  namespace_filter: string[];
  exclude_types: string[];
}

export interface AgentProfileView {
  id: string;
  agent_type: string;
  description: string;
  inject_rules: InjectRulesView;
  /** Never the actual key/hash — only whether one is configured. */
  has_api_key: boolean;
}

export async function getAgentProfiles(): Promise<AgentProfileView[]> {
  return await request<AgentProfileView[]>("GET", "/api/agents");
}
// ── Memory relations (review-path visibility, PLAN §10.4) ──────────────

/** Relation triples touching one memory — surfaced on the detail panel so
 * review-inbox contradicts links are visible outside the search UI. */
export async function getMemoryRelations(memoryId: string): Promise<RelationView[]> {
  return await request<RelationView[]>("GET", `/api/memories/${memoryId}/relations`);
}
