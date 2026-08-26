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
  created_at: string;
  updated_at: string;
}

export interface SkillMetaView {
  trigger: string | null;
  steps: string[];
  verification: string | null;
  version: number;
}

export interface SearchResultView {
  memory: MemoryView;
  score: number;
  /** Actual retrieval mode used by the backend (e.g. degraded to keyword). */
  searchMode?: string;
  /** Recall provenance tags, e.g. ["kw#2", "vec#1"]. */
  hitSources?: string[];
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
}): Promise<SearchResultView[]> {
  const data = await request<any[]>("POST", "/api/search", {
    query: p.query,
    top_k: p.topK ?? 20,
    mode: p.mode ?? "keyword",
    namespace: p.namespace,
  });
  return data.map((r) => ({
    memory: toMemoryView(r.memory),
    score: r.score,
    searchMode: r.search_mode,
    hitSources: r.hit_sources,
  }));
}

export interface CreateMemoryInput {
  content: string;
  instruction?: string | null;
  priority: string;
  memory_type: string;
  namespace: string;
  tags: string[];
}

export async function createMemory(v: CreateMemoryInput): Promise<{ id: string; embedded: boolean }> {
  const { memory_type, ...rest } = v;
  return await request<{ id: string; embedded: boolean }>("POST", "/api/memories", {
    ...rest,
    type: memory_type,
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

export async function rejectMemory(id: string): Promise<void> {
  await request("DELETE", `/api/memories/${id}`);
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