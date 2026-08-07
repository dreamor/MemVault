import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./App.css";

interface MemoryView {
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
  created_at: string;
  updated_at: string;
}

interface SearchResultView {
  memory: MemoryView;
  score: number;
}

interface StatsView {
  total: number;
  must_count: number;
  reference_count: number;
  reviewed_count: number;
  agents: string[];
}

type Tab = "memories" | "search" | "review" | "stats";

function App() {
  const [tab, setTab] = useState<Tab>("memories");
  const [memories, setMemories] = useState<MemoryView[]>([]);
  const [searchQuery, setSearchQuery] = useState("");
  const [searchResults, setSearchResults] = useState<SearchResultView[]>([]);
  const [stats, setStats] = useState<StatsView | null>(null);
  const [selected, setSelected] = useState<MemoryView | null>(null);

  useEffect(() => {
    if (tab === "memories" || tab === "review") loadMemories();
    if (tab === "stats") loadStats();
  }, [tab]);

  async function loadMemories() {
    try {
      const result = await invoke<MemoryView[]>("list_memories", { limit: 200 });
      setMemories(result);
    } catch (e) {
      console.error("Failed to load memories:", e);
    }
  }

  async function loadStats() {
    try {
      const result = await invoke<StatsView>("get_stats");
      setStats(result);
    } catch (e) {
      console.error("Failed to load stats:", e);
    }
  }

  async function doSearch() {
    if (!searchQuery.trim()) return;
    try {
      const result = await invoke<SearchResultView[]>("search_memories", {
        query: searchQuery,
        topK: 20,
      });
      setSearchResults(result);
    } catch (e) {
      console.error("Search failed:", e);
    }
  }

  async function handleApprove(id: string) {
    try {
      await invoke("approve_memory", { id });
      loadMemories();
      setSelected(null);
    } catch (e) {
      console.error("Approve failed:", e);
    }
  }

  async function handleReject(id: string) {
    if (!confirm("Delete this memory?")) return;
    try {
      await invoke("reject_memory", { id });
      loadMemories();
      setSelected(null);
    } catch (e) {
      console.error("Reject failed:", e);
    }
  }

  const pendingReview = memories.filter((m) => !m.human_reviewed);

  return (
    <div className="app">
      <header className="header">
        <h1>MemVault</h1>
        <nav className="tabs">
          {(["memories", "search", "review", "stats"] as Tab[]).map((t) => (
            <button
              key={t}
              className={tab === t ? "active" : ""}
              onClick={() => setTab(t)}
            >
              {t === "memories" && `Memories (${memories.length})`}
              {t === "search" && "Search"}
              {t === "review" && `Review (${pendingReview.length})`}
              {t === "stats" && "Stats"}
            </button>
          ))}
        </nav>
      </header>

      <main className="content">
        {tab === "memories" && (
          <MemoryList
            memories={memories}
            onSelect={setSelected}
            selected={selected}
            onApprove={handleApprove}
            onReject={handleReject}
          />
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
          </div>
        )}
      </main>

      {selected && (
        <DetailPanel
          memory={selected}
          onClose={() => setSelected(null)}
          onApprove={handleApprove}
          onReject={handleReject}
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
        <p className="empty">No memories stored yet. Use the CLI or MCP Server to add memories.</p>
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
        <span className="type">{m.memory_type}</span>
        {m.human_reviewed && <span className="reviewed">Reviewed</span>}
        {score !== undefined && <span className="score">{score.toFixed(3)}</span>}
      </div>
      <p className="card-content">{m.instruction || m.content}</p>
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
      </div>
    </div>
  );
}

function StatCard({ label, value }: { label: string; value: number }) {
  return (
    <div className="stat-card">
      <div className="stat-value">{value}</div>
      <div className="stat-label">{label}</div>
    </div>
  );
}

function DetailPanel({
  memory: m,
  onClose,
  onApprove,
  onReject,
}: {
  memory: MemoryView;
  onClose: () => void;
  onApprove: (id: string) => void;
  onReject: (id: string) => void;
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

        <div className="detail-actions">
          {!m.human_reviewed && (
            <button className="approve" onClick={() => onApprove(m.id)}>Approve</button>
          )}
          <button className="reject" onClick={() => onReject(m.id)}>Delete</button>
        </div>
      </div>
    </div>
  );
}

export default App;
