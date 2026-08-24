import { useState, useEffect, useRef } from "react";
import {
  MemoryView,
  SearchResultView,
  StatsView,
  ComplianceSummary,
  createMemory,
  updateMemory,
  searchMemories,
  listMemories,
  getInbox,
  getStats,
  approveMemory,
  rejectMemory,
  runPromote,
  runDecay,
  runDedup,
  getComplianceSummary,
  health,
  getApiKey,
  setApiKey,
} from "./api";
import "./App.css";

type Tab = "memories" | "search" | "review" | "stats" | "settings";

const PAGE_SIZE = 50;

/** How often the open dashboard polls the backend to keep
 * header badges (Review count, memories page) and the namespace list fresh
 * without a manual reload. */
const REFRESH_INTERVAL_MS = 30_000;

const PRIORITIES = ["MUST", "REFERENCE", "BACKGROUND"];
const MEMORY_TYPES = ["preference", "fact", "episode", "entity", "skill"];

interface MemoryFormValues {
  content: string;
  instruction: string;
  priority: string;
  memory_type: string;
  namespace: string;
  tagsInput: string;
  skillTrigger: string;
  skillStepsInput: string;
  skillVerification: string;
}

function emptyForm(): MemoryFormValues {
  return {
    content: "",
    instruction: "",
    priority: "REFERENCE",
    memory_type: "fact",
    namespace: "global",
    tagsInput: "",
    skillTrigger: "",
    skillStepsInput: "",
    skillVerification: "",
  };
}

function formFromMemory(m: MemoryView): MemoryFormValues {
  return {
    content: m.content,
    instruction: m.instruction ?? "",
    priority: m.priority.toUpperCase(),
    memory_type: m.memory_type.toLowerCase(),
    namespace: m.namespace,
    tagsInput: m.tags.join(", "),
    skillTrigger: m.skill_meta?.trigger ?? "",
    skillStepsInput: m.skill_meta?.steps.join(", ") ?? "",
    skillVerification: m.skill_meta?.verification ?? "",
  };
}

function App() {
  const [tab, setTab] = useState<Tab>("memories");
  const [memories, setMemories] = useState<MemoryView[]>([]);
  const [pendingReview, setPendingReview] = useState<MemoryView[]>([]);
  const memoriesRequestId = useRef(0);
  const pendingReviewRequestId = useRef(0);
  const [searchQuery, setSearchQuery] = useState("");
  const [searchResults, setSearchResults] = useState<SearchResultView[]>([]);
  const [stats, setStats] = useState<StatsView | null>(null);
  const [selected, setSelected] = useState<MemoryView | null>(null);
  const [namespaceFilter, setNamespaceFilter] = useState("");
  const [page, setPage] = useState(0);
  const [hasNextPage, setHasNextPage] = useState(false);
  const [formOpen, setFormOpen] = useState(false);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [connected, setConnected] = useState<boolean | null>(null);
  const [apiKeyInput, setApiKeyInput] = useState(getApiKey());
  const [apiKeySaved, setApiKeySaved] = useState(false);
  const [compliance, setCompliance] = useState<ComplianceSummary | null>(null);
  const [complianceError, setComplianceError] = useState<string | null>(null);

  useEffect(() => {
    if (tab === "memories") loadMemories();
    if (tab === "review") loadPendingReview();
    if (tab === "stats") {
      loadStats();
      loadCompliance();
    }
    if (tab === "settings") checkConnection();
  }, [tab, namespaceFilter, page]);

  // Namespace filter list and the Review tab badge count need to be
  // available even before the user has visited those tabs.
  useEffect(() => {
    loadStats();
    loadPendingReview();
  }, []);

  async function loadMemories() {
    const requestId = ++memoriesRequestId.current;
    try {
      const result = await listMemories({
        namespace: namespaceFilter || undefined,
        limit: PAGE_SIZE,
        offset: page * PAGE_SIZE,
      });
      // A faster, more recent request may have already resolved — don't
      // let a stale response overwrite it.
      if (requestId !== memoriesRequestId.current) return;
      setHasNextPage(result.length === PAGE_SIZE);
      setMemories(result);
    } catch (e) {
      console.error("Failed to load memories:", e);
    }
  }

  /**
   * Full pending-review list — independent of the Memories tab's pagination
   * and namespace filter, so items outside the current page are still
   * surfaced for review instead of being silently invisible.
   */
  async function loadPendingReview() {
    const requestId = ++pendingReviewRequestId.current;
    try {
      const result = await getInbox();
      if (requestId !== pendingReviewRequestId.current) return;
      setPendingReview(result);
    } catch (e) {
      console.error("Failed to load pending review:", e);
    }
  }

  async function loadStats() {
    try {
      const result = await getStats();
      setStats(result);
    } catch (e) {
      console.error("Failed to load stats:", e);
    }
  }

  async function loadCompliance() {
    try {
      const result = await getComplianceSummary(10);
      setCompliance(result);
      setComplianceError(null);
    } catch (e) {
      setCompliance(null);
      setComplianceError(String(e));
    }
  }
  /**
   * Header badges (Review count, Memories page count) and the namespace
   * dropdown only refreshed on mount or on tab switch. Keep them fresh while
   * the dashboard stays open: poll periodically, and also refresh whenever the
   * window regains focus (e.g. an agent wrote via CLI/MCP while this tab was
   * in the background). load*() calls are idempotent and stale-guarded.
   */
  useEffect(() => {
    function refresh() {
      loadStats();
      loadPendingReview();
      if (tab === "memories") loadMemories();
    }
    const id = setInterval(refresh, REFRESH_INTERVAL_MS);
    window.addEventListener("focus", refresh);
    return () => {
      clearInterval(id);
      window.removeEventListener("focus", refresh);
    };
  }, [tab, namespaceFilter, page]);


  async function checkConnection() {
    const ok = await health().catch(() => false);
    setConnected(ok);
  }

  function saveApiKeyInfo() {
    setApiKey(apiKeyInput.trim());
    setApiKeySaved(true);
    setTimeout(() => setApiKeySaved(false), 2000);
  }

  async function doSearch() {
    if (!searchQuery.trim()) return;
    try {
      const result = await searchMemories({
        query: searchQuery,
        topK: 20,
        mode: "keyword",
        namespace: namespaceFilter || undefined,
      });
      setSearchResults(result);
    } catch (e) {
      console.error("Search failed:", e);
    }
  }

  async function handleApprove(id: string) {
    try {
      await approveMemory(id);
      loadMemories();
      loadPendingReview();
      setSelected(null);
    } catch (e) {
      console.error("Approve failed:", e);
    }
  }

  async function handleReject(id: string) {
    if (!confirm("Delete this memory?")) return;
    try {
      await rejectMemory(id);
      loadMemories();
      loadPendingReview();
      setSelected(null);
    } catch (e) {
      console.error("Reject failed:", e);
    }
  }

  async function handlePromote() {
    try {
      const result = await runPromote();
      alert(`Promote: ${result.promoted_to_l2} → L2, ${result.promoted_to_l3} → L3`);
      loadMemories();
      loadStats();
    } catch (e) {
      console.error("Promote failed:", e);
    }
  }

  async function handleDecay() {
    try {
      const result = await runDecay();
      alert(`Decay: ${result.updated} updated, ${result.archived} archived`);
      loadMemories();
      loadStats();
    } catch (e) {
      console.error("Decay failed:", e);
    }
  }

  async function handleDedup() {
    try {
      const result = await runDedup();
      alert(`Dedup: ${result.unique_count} unique, ${result.duplicate_count} duplicates found`);
      loadMemories();
    } catch (e) {
      console.error("Dedup failed:", e);
    }
  }

  function openCreateForm() {
    setEditingId(null);
    setFormOpen(true);
  }

  function openEditForm(m: MemoryView) {
    setEditingId(m.id);
    setFormOpen(true);
  }

  async function handleFormSubmit(values: MemoryFormValues) {
    const tags = values.tagsInput
      .split(",")
      .map((t) => t.trim())
      .filter(Boolean);
    const skillSteps = values.skillStepsInput
      .split(",")
      .map((s) => s.trim())
      .filter(Boolean);
    const isSkill = values.memory_type === "skill";

    try {
      if (editingId) {
        await updateMemory(editingId, {
          content: values.content,
          instruction: values.instruction || null,
          priority: values.priority,
          memory_type: values.memory_type,
          tags,
          namespace: values.namespace,
          skill_trigger: isSkill ? values.skillTrigger || null : null,
          skill_steps: isSkill ? skillSteps : null,
          skill_verification: isSkill ? values.skillVerification || null : null,
        });
      } else {
        await createMemory({
          content: values.content,
          instruction: values.instruction || null,
          priority: values.priority,
          memory_type: values.memory_type,
          namespace: values.namespace,
          tags,
        });
      }
      setFormOpen(false);
      setEditingId(null);
      setSelected(null);
      loadMemories();
      loadStats();
    } catch (e) {
      alert(`Save failed: ${e}`);
    }
  }

  return (
    <div className="app">
      <header className="header">
        <h1>MemVault</h1>
        <nav className="tabs">
          {(["memories", "search", "review", "stats", "settings"] as Tab[]).map((t) => (
            <button
              key={t}
              className={tab === t ? "active" : ""}
              onClick={() => setTab(t)}
            >
              {t === "memories" && `Memories (${memories.length})`}
              {t === "search" && "Search"}
              {t === "review" && `Review (${pendingReview.length})`}
              {t === "stats" && "Stats"}
              {t === "settings" && "Settings"}
            </button>
          ))}
        </nav>
        {tab === "memories" && (
          <button className="new-memory-btn" onClick={openCreateForm}>
            + New Memory
          </button>
        )}
      </header>

      <main className="content">
        {tab === "memories" && (
          <>
            <div className="list-toolbar">
              <select
                value={namespaceFilter}
                onChange={(e) => {
                  setPage(0);
                  setNamespaceFilter(e.target.value);
                }}
              >
                <option value="">All namespaces</option>
                {(stats?.namespaces ?? []).map((ns) => (
                  <option key={ns} value={ns}>
                    {ns}
                  </option>
                ))}
              </select>
              <div className="pagination">
                <button disabled={page === 0} onClick={() => setPage((p) => Math.max(0, p - 1))}>
                  ← Prev
                </button>
                <span>Page {page + 1}</span>
                <button disabled={!hasNextPage} onClick={() => setPage((p) => p + 1)}>
                  Next →
                </button>
              </div>
            </div>
            <MemoryList
              memories={memories}
              onSelect={setSelected}
              selected={selected}
              onApprove={handleApprove}
              onReject={handleReject}
            />
          </>
        )}

        {tab === "search" && (
          <div className="search-panel">
            <div className="search-bar">
              <input
                value={searchQuery}
                onChange={(e) => setSearchQuery(e.target.value)}
                onKeyDown={(e) => e.key === "Enter" && doSearch()}
                placeholder="Search memories..."
              />
              <button onClick={doSearch}>Search</button>
            </div>
            <div className="results">
              {searchResults.map((r) => (
                <MemoryCard
                  key={r.memory.id}
                  memory={r.memory}
                  score={r.score}
                  onClick={() => setSelected(r.memory)}
                />
              ))}
              {searchResults.length === 0 && searchQuery && (
                <p className="empty">No results found.</p>
              )}
            </div>
          </div>
        )}

        {tab === "review" && (
          <div className="review-panel">
            <h2>Pending Review ({pendingReview.length})</h2>
            {pendingReview.length === 0 ? (
              <p className="empty">All memories have been reviewed.</p>
            ) : (
              pendingReview.map((m) => (
                <div key={m.id} className="review-card">
                  <MemoryCard memory={m} onClick={() => setSelected(m)} />
                  <div className="review-actions">
                    <button className="approve" onClick={() => handleApprove(m.id)}>
                      Approve
                    </button>
                    <button className="reject" onClick={() => handleReject(m.id)}>
                      Reject
                    </button>
                  </div>
                </div>
              ))
            )}
          </div>
        )}

        {tab === "stats" && stats && (
          <div className="stats-panel">
            <div className="stat-grid">
              <StatCard label="Total Memories" value={stats.total} />
              <StatCard label="MUST Rules" value={stats.must_count} />
              <StatCard label="References" value={stats.reference_count} />
              <StatCard label="Reviewed" value={stats.reviewed_count} />
              <StatCard label="L3 (Persona)" value={stats.layers.l3} />
              <StatCard label="L2 (Scenario)" value={stats.layers.l2} />
              <StatCard label="L1 (Atom)" value={stats.layers.l1} />
              <StatCard label="Skills" value={stats.skills} />
            </div>
            <div className="actions-section">
              <h3>Pipeline Actions</h3>
              <button onClick={handlePromote}>Run Promote (L1→L2→L3)</button>
              <button onClick={handleDecay}>Run Decay</button>
              <button onClick={handleDedup}>Run Dedup</button>
            </div>
            <div className="agents-section">
              <h3>Connected Agents</h3>
              {stats.agents.length === 0 ? (
                <p className="empty">No agents have written memories yet.</p>
              ) : (
                <ul>
                  {stats.agents.map((a) => (
                    <li key={a}>{a}</li>
                  ))}
                </ul>
              )}
            </div>
            <div className="compliance-section">
              <h3>Compliance (last {compliance?.recent_sessions.length ?? 0} sessions)</h3>
              {complianceError && (
                <p className="empty">Compliance tracking is not enabled on this database.</p>
              )}
              {compliance && (
                <>
                  <div className="stat-grid compliance-grid">
                    <StatCard label="Sessions" value={compliance.total_sessions} />
                    <StatCard label="Overall Rate" value={Math.round(compliance.overall_rate * 100)} suffix="%" />
                    <StatCard label="MUST Rate" value={Math.round(compliance.must_rate * 100)} suffix="%" />
                  </div>
                  {compliance.recent_sessions.length > 0 && (
                    <table className="compliance-table">
                      <thead>
                        <tr>
                          <th>Session</th>
                          <th>Agent</th>
                          <th>Injected</th>
                          <th>MUST ✓/✗</th>
                          <th>REF ✓/✗</th>
                          <th>Rate</th>
                        </tr>
                      </thead>
                      <tbody>
                        {compliance.recent_sessions.map((r) => (
                          <tr key={r.inject_session_id}>
                            <td>{r.inject_session_id}</td>
                            <td>{r.agent_id}</td>
                            <td>{r.total_injected}</td>
                            <td>{r.must_followed}/{r.must_violated}</td>
                            <td>{r.ref_followed}/{r.ref_violated}</td>
                            <td>{Math.round(r.compliance_rate * 100)}%</td>
                          </tr>
                        ))}
                      </tbody>
                    </table>
                  )}
                </>
              )}
            </div>
          </div>
        )}

        {tab === "settings" && (
          <div className="settings-panel">
            <h2>Settings</h2>
            <div className="settings-field">
              <label>Backend connection</label>
              <p className="settings-hint">
                {connected === null && <span>Checking…</span>}
                {connected === true && (
                  <span className="status-connected">● Connected</span>
                )}
                {connected === false && (
                  <span className="status-disconnected">
                    ● Unreachable — is <code>memvault-mcp</code> running with{" "}
                    <code>--transport http</code>?
                  </span>
                )}
              </p>
            </div>
            <div className="settings-field">
              <label>API key</label>
              <input
                type="password"
                value={apiKeyInput}
                onChange={(e) => {
                  setApiKeyInput(e.target.value);
                  setApiKeySaved(false);
                }}
              />
              <p className="settings-hint">
                Sent as <code>X-MemVault-Api-Key</code> for admin-protected REST routes
                (required when the server registers an admin key in{" "}
                <code>agents.yaml</code>). Leave empty if no key is configured.
              </p>
              <button onClick={saveApiKeyInfo}>Save</button>
              {apiKeySaved && <span className="settings-saved">Saved.</span>}
            </div>
            <div className="settings-field">
              <label>Agent ID</label>
              <p className="settings-hint">
                Sent as <code>X-MemVault-Agent-Id</code> (default <code>admin</code>), and
                used to label memories created here as{" "}
                <code>dashboard</code>. Override in the app config only if your server's
                registry uses a different admin agent.
              </p>
            </div>
            <div className="settings-field">
              <label>Access</label>
              <p className="settings-hint">
                This Dashboard fetches the REST API served by{" "}
                <code>memvault-mcp --db ~/.memvault/data.db --transport http
                --serve-web <var>dist</var></code> — the same protocol VS Code and
                Obsidian clients use. Run it on the machine that owns the SQLite file.
              </p>
            </div>
          </div>
        )}
      </main>

      {selected && !formOpen && (
        <DetailPanel
          memory={selected}
          onClose={() => setSelected(null)}
          onApprove={handleApprove}
          onReject={handleReject}
          onEdit={() => openEditForm(selected)}
        />
      )}

      {formOpen && (
        <MemoryFormPanel
          initial={editingId ? formFromMemory(selected!) : emptyForm()}
          isEdit={editingId !== null}
          onCancel={() => {
            setFormOpen(false);
            setEditingId(null);
          }}
          onSubmit={handleFormSubmit}
        />
      )}
    </div>
  );
}

function MemoryList({
  memories,
  onSelect,
  selected,
}: {
  memories: MemoryView[];
  onSelect: (m: MemoryView) => void;
  selected: MemoryView | null;
  onApprove: (id: string) => void;
  onReject: (id: string) => void;
}) {
  return (
    <div className="memory-list">
      {memories.map((m) => (
        <MemoryCard
          key={m.id}
          memory={m}
          active={selected?.id === m.id}
          onClick={() => onSelect(m)}
        />
      ))}
      {memories.length === 0 && (
        <p className="empty">No memories stored yet. Use the CLI, MCP Server, or the "New Memory" button above.</p>
      )}
    </div>
  );
}

function MemoryCard({
  memory: m,
  score,
  active,
  onClick,
}: {
  memory: MemoryView;
  score?: number;
  active?: boolean;
  onClick: () => void;
}) {
  return (
    <div className={`memory-card ${active ? "active" : ""}`} onClick={onClick}>
      <div className="card-header">
        <span className={`priority ${m.priority.toLowerCase()}`}>{m.priority}</span>
        <span className="layer">{m.layer}</span>
        <span className="type">{m.memory_type}</span>
        {m.human_reviewed && <span className="reviewed">Reviewed</span>}
        {score !== undefined && <span className="score">{score.toFixed(3)}</span>}
      </div>
      <p className="card-content">{m.instruction || m.content}</p>
      {m.skill_meta && (
        <div className="skill-info">
          {m.skill_meta.trigger && <span className="skill-trigger">⚡ {m.skill_meta.trigger}</span>}
          {m.skill_meta.steps.length > 0 && <span className="skill-steps">{m.skill_meta.steps.length} steps</span>}
          {m.skill_meta.verification && <span className="skill-verify">✓ {m.skill_meta.verification}</span>}
        </div>
      )}
      {m.tags.length > 0 && (
        <div className="tags">
          {m.tags.map((t) => (
            <span key={t} className="tag">{t}</span>
          ))}
        </div>
      )}
      <div className="card-meta">
        <span>{m.source_agent_id}</span>
        <span>{m.namespace}</span>
        <span>×{m.access_count}</span>
      </div>
    </div>
  );
}

function StatCard({ label, value, suffix }: { label: string; value: number; suffix?: string }) {
  return (
    <div className="stat-card">
      <div className="stat-value">{value}{suffix ?? ""}</div>
      <div className="stat-label">{label}</div>
    </div>
  );
}

function DetailPanel({
  memory: m,
  onClose,
  onApprove,
  onReject,
  onEdit,
}: {
  memory: MemoryView;
  onClose: () => void;
  onApprove: (id: string) => void;
  onReject: (id: string) => void;
  onEdit: () => void;
}) {
  return (
    <div className="detail-overlay" onClick={onClose}>
      <div className="detail-panel" onClick={(e) => e.stopPropagation()}>
        <button className="close-btn" onClick={onClose}>×</button>
        <h2>Memory Detail</h2>

        <div className="detail-field">
          <label>ID</label>
          <code>{m.id}</code>
        </div>
        <div className="detail-field">
          <label>Priority</label>
          <span className={`priority ${m.priority.toLowerCase()}`}>{m.priority}</span>
        </div>
        <div className="detail-field">
          <label>Layer</label>
          <span>{m.layer}</span>
        </div>
        <div className="detail-field">
          <label>Type</label>
          <span>{m.memory_type}</span>
        </div>
        <div className="detail-field">
          <label>Content</label>
          <p>{m.content}</p>
        </div>
        {m.instruction && (
          <div className="detail-field">
            <label>Instruction</label>
            <p>{m.instruction}</p>
          </div>
        )}
        <div className="detail-field">
          <label>Tags</label>
          <div className="tags">
            {m.tags.map((t) => <span key={t} className="tag">{t}</span>)}
            {m.tags.length === 0 && <span className="empty">none</span>}
          </div>
        </div>
        <div className="detail-field">
          <label>Agent</label>
          <span>{m.source_agent_id}</span>
        </div>
        <div className="detail-field">
          <label>Namespace</label>
          <span>{m.namespace}</span>
        </div>
        <div className="detail-field">
          <label>Confidence</label>
          <span>{(m.confidence * 100).toFixed(0)}%</span>
        </div>
        <div className="detail-field">
          <label>Status</label>
          <span>{m.human_reviewed ? "Reviewed" : "Pending Review"}</span>
        </div>
        <div className="detail-field">
          <label>Created</label>
          <span>{new Date(m.created_at).toLocaleString()}</span>
        </div>
        {m.skill_meta && (
          <div className="detail-field">
            <label>Skill</label>
            <div className="skill-detail">
              {m.skill_meta.trigger && <p><strong>Trigger:</strong> {m.skill_meta.trigger}</p>}
              {m.skill_meta.steps.length > 0 && (
                <ol>
                  {m.skill_meta.steps.map((s, i) => <li key={i}>{s}</li>)}
                </ol>
              )}
              {m.skill_meta.verification && <p><strong>Verify:</strong> {m.skill_meta.verification}</p>}
              <p><strong>Version:</strong> {m.skill_meta.version}</p>
            </div>
          </div>
        )}

        <div className="detail-actions">
          <button className="edit" onClick={onEdit}>Edit</button>
          {!m.human_reviewed && (
            <button className="approve" onClick={() => onApprove(m.id)}>Approve</button>
          )}
          <button className="reject" onClick={() => onReject(m.id)}>Delete</button>
        </div>
      </div>
    </div>
  );
}

function MemoryFormPanel({
  initial,
  isEdit,
  onCancel,
  onSubmit,
}: {
  initial: MemoryFormValues;
  isEdit: boolean;
  onCancel: () => void;
  onSubmit: (values: MemoryFormValues) => void;
}) {
  const [values, setValues] = useState<MemoryFormValues>(initial);
  const isSkill = values.memory_type === "skill";

  function set<K extends keyof MemoryFormValues>(key: K, value: MemoryFormValues[K]) {
    setValues((v) => ({ ...v, [key]: value }));
  }

  return (
    <div className="detail-overlay" onClick={onCancel}>
      <div className="detail-panel" onClick={(e) => e.stopPropagation()}>
        <button className="close-btn" onClick={onCancel}>×</button>
        <h2>{isEdit ? "Edit Memory" : "New Memory"}</h2>

        <div className="detail-field">
          <label>Content</label>
          <textarea
            rows={3}
            value={values.content}
            onChange={(e) => set("content", e.target.value)}
          />
        </div>
        <div className="detail-field">
          <label>Instruction (optional)</label>
          <textarea
            rows={2}
            value={values.instruction}
            onChange={(e) => set("instruction", e.target.value)}
          />
        </div>
        <div className="detail-field">
          <label>Priority</label>
          <select value={values.priority} onChange={(e) => set("priority", e.target.value)}>
            {PRIORITIES.map((p) => (
              <option key={p} value={p}>{p}</option>
            ))}
          </select>
        </div>
        <div className="detail-field">
          <label>Type</label>
          <select value={values.memory_type} onChange={(e) => set("memory_type", e.target.value)}>
            {MEMORY_TYPES.map((t) => (
              <option key={t} value={t}>{t}</option>
            ))}
          </select>
        </div>
        <div className="detail-field">
          <label>Namespace</label>
          <input value={values.namespace} onChange={(e) => set("namespace", e.target.value)} />
        </div>
        <div className="detail-field">
          <label>Tags (comma-separated)</label>
          <input value={values.tagsInput} onChange={(e) => set("tagsInput", e.target.value)} />
        </div>
        {isSkill && (
          <>
            <div className="detail-field">
              <label>Skill trigger</label>
              <input value={values.skillTrigger} onChange={(e) => set("skillTrigger", e.target.value)} />
            </div>
            <div className="detail-field">
              <label>Skill steps (comma-separated)</label>
              <input value={values.skillStepsInput} onChange={(e) => set("skillStepsInput", e.target.value)} />
            </div>
            <div className="detail-field">
              <label>Skill verification</label>
              <input value={values.skillVerification} onChange={(e) => set("skillVerification", e.target.value)} />
            </div>
          </>
        )}

        <div className="detail-actions">
          <button className="approve" onClick={() => onSubmit(values)} disabled={!values.content.trim()}>
            {isEdit ? "Save Changes" : "Create"}
          </button>
          <button className="reject" onClick={onCancel}>Cancel</button>
        </div>
      </div>
    </div>
  );
}

export default App;
