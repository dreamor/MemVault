import { useState, useEffect, useRef } from "react";
import {
  MemoryView,
  SearchResultView,
  RelationView,
  StatsView,
  ComplianceSummary,
  ComplianceReport,
  EpisodeView,
  OutcomeStatus,
  RecordOutcomeResult,
  ExtractResult,
  DoctorReport,
  CapabilityStatus,
  MetricSample,
  ExportResult,
  ImportResult,
  CheckpointEntry,
  ImportSkillsResult,
  AgentScanResult,
  AgentImportPreview,
  AgentImportRunResult,
  AgentProfileView,
  createMemory,
  updateMemory,
  searchMemories,
  listMemories,
  getInbox,
  getStats,
  approveMemory,
  rejectMemory,
  rejectPendingMemory,
  editPendingMemory,
  supersedeMemory,
  runPromote,
  runDecay,
  runDedup,
  getComplianceSummary,
  getComplianceSession,
  confirmRead,
  getEffectivenessReport,
  previewSession,
  getMemoryRelations,
  getMemoryEvidence,
  EvidenceChainView,
  EffectivenessSummary,
  SessionPreview,
  recordOutcome,
  listEpisodes,
  extractMemories,
  runDoctor,
  getCapabilities,
  getMetrics,
  exportMemories,
  importMemories,
  createBackup,
  listCheckpoints,
  restoreCheckpoint,
  importSkills,
  scanAgentImport,
  previewAgentImport,
  runAgentImport,
  getAgentProfiles,
  health,
  getApiKey,
  setApiKey,
} from "./api";
import "./App.css";
import { useI18n } from "./i18n";

type Tab = "memories" | "episodic" | "search" | "review" | "stats" | "system" | "data" | "agents" | "settings";

/** Trigger a browser download of in-memory content — used by Export/Backup. */
function downloadBlob(content: Blob | string, filename: string, mimeType = "application/json") {
  const blob = typeof content === "string" ? new Blob([content], { type: mimeType }) : content;
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  a.click();
  URL.revokeObjectURL(url);
}

const OUTCOME_STATUSES: OutcomeStatus[] = ["success", "failure", "partial"];

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
  visibility: string;
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
    visibility: "scoped",
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
    visibility: m.visibility || "scoped",
    skillTrigger: m.skill_meta?.trigger ?? "",
    skillStepsInput: m.skill_meta?.steps.join(", ") ?? "",
    skillVerification: m.skill_meta?.verification ?? "",
  };
}

const SEARCH_MODES = ["keyword", "semantic", "hybrid"];
const VISIBILITIES = ["scoped", "shared"];

/** Escape a string for safe use inside a RegExp. */
function escapeRegExp(input: string): string {
  return input.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/**
 * Highlight every whitespace-separated query term inside `text` with <mark>.
 * Case-insensitive; output is React nodes, so matched text stays escaped.
 */
function highlight(text: string, query: string): React.ReactNode {
  const terms = query
    .split(/\s+/)
    .map((t) => t.trim())
    .filter(Boolean);
  if (terms.length === 0) return text;
  const re = new RegExp(`(${terms.map(escapeRegExp).join("|")})`, "gi");
  return text.split(re).map((part, i) =>
    i % 2 === 1 ? <mark key={i}>{part}</mark> : part,
  );
}

function App() {
  const { t, locale, setLocale, theme, setTheme } = useI18n();
  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    document.documentElement.lang = locale === "zh" ? "zh-CN" : "en";
  }, [theme, locale]);

  const [tab, setTab] = useState<Tab>("memories");
  const [memories, setMemories] = useState<MemoryView[]>([]);
  const [pendingReview, setPendingReview] = useState<MemoryView[]>([]);
  const memoriesRequestId = useRef(0);
  const pendingReviewRequestId = useRef(0);
  const [searchQuery, setSearchQuery] = useState("");
  const [searchMode, setSearchMode] = useState<string>("keyword");
  const [expandRelations, setExpandRelations] = useState(false);
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
  const [sessionDetail, setSessionDetail] = useState<ComplianceReport | null>(null);
  const [sessionDetailError, setSessionDetailError] = useState<string | null>(null);
  const [effectiveness, setEffectiveness] = useState<EffectivenessSummary | null>(null);
  const [effectivenessError, setEffectivenessError] = useState<string | null>(null);
  const [sessionPreview, setSessionPreview] = useState<SessionPreview | null>(null);
  const [previewBusy, setPreviewBusy] = useState(false);
  const [previewError, setPreviewError] = useState<string | null>(null);
  // Relations of the memory shown in the detail panel (review-path visibility).
  const [selectedRelations, setSelectedRelations] = useState<RelationView[]>([]);
  // Evidence chain (L0 traces) of the memory shown in the detail panel.
  const [selectedEvidence, setSelectedEvidence] = useState<EvidenceChainView | null>(null);

  useEffect(() => {
    let cancelled = false;
    if (!selected?.id) { setSelectedRelations([]); setSelectedEvidence(null); return; }
    getMemoryRelations(selected.id)
      .then((rels) => { if (!cancelled) setSelectedRelations(rels); })
      .catch(() => { if (!cancelled) setSelectedRelations([]); });
    getMemoryEvidence(selected.id)
      .then((ev) => { if (!cancelled) setSelectedEvidence(ev); })
      .catch(() => { if (!cancelled) setSelectedEvidence(null); });
    return () => { cancelled = true; };
  }, [selected?.id]);

  // Episodic memory tab state.
  const [episodes, setEpisodes] = useState<EpisodeView[]>([]);
  const [episodeStatusFilter, setEpisodeStatusFilter] = useState<string>("");
  const [outcomeForm, setOutcomeForm] = useState({
    task: "",
    status: "success" as OutcomeStatus,
    cause: "",
    task_type: "",
    namespace: "global",
  });
  const [outcomeResult, setOutcomeResult] = useState<RecordOutcomeResult | null>(null);
  const [outcomeError, setOutcomeError] = useState<string | null>(null);

  // Extract-from-text panel state (Memories tab).
  const [extractOpen, setExtractOpen] = useState(false);
  const [extractText, setExtractText] = useState("");
  const [extractMode, setExtractMode] = useState<"rule" | "llm">("rule");
  const [extractNamespace, setExtractNamespace] = useState("global");
  const [extractResult, setExtractResult] = useState<ExtractResult | null>(null);
  const [extractSelected, setExtractSelected] = useState<Set<number>>(new Set());
  const [extractError, setExtractError] = useState<string | null>(null);
  const [extracting, setExtracting] = useState(false);

  // System tab (Doctor / Capabilities / Metrics) state.
  const [capabilities, setCapabilities] = useState<CapabilityStatus[] | null>(null);
  const [capabilitiesError, setCapabilitiesError] = useState<string | null>(null);
  const [metrics, setMetrics] = useState<MetricSample[] | null>(null);
  const [metricsError, setMetricsError] = useState<string | null>(null);
  const [doctorReport, setDoctorReport] = useState<DoctorReport | null>(null);
  const [doctorError, setDoctorError] = useState<string | null>(null);
  const [doctorRunning, setDoctorRunning] = useState(false);

  // Data tab state.
  const [exportFormat, setExportFormat] = useState<"json" | "markdown">("json");
  const [exportNamespace, setExportNamespace] = useState("");
  const [exportBusy, setExportBusy] = useState(false);
  const [importBusy, setImportBusy] = useState(false);
  const [importResult, setImportResult] = useState<ImportResult | null>(null);
  const [importError, setImportError] = useState<string | null>(null);
  const [backupBusy, setBackupBusy] = useState(false);

  const [skillMarkdown, setSkillMarkdown] = useState("");
  const [skillNamespace, setSkillNamespace] = useState("global");
  const [skillApprove, setSkillApprove] = useState(false);
  const [skillBusy, setSkillBusy] = useState(false);
  const [skillResult, setSkillResult] = useState<ImportSkillsResult | null>(null);
  const [skillError, setSkillError] = useState<string | null>(null);

  const [agentScanResults, setAgentScanResults] = useState<AgentScanResult[] | null>(null);
  const [agentScanBusy, setAgentScanBusy] = useState(false);
  const [agentScanError, setAgentScanError] = useState<string | null>(null);
  const [agentImportNamespace, setAgentImportNamespace] = useState("");
  const [agentImportPreview, setAgentImportPreview] = useState<AgentImportPreview | null>(null);
  const [agentImportBusy, setAgentImportBusy] = useState(false);
  const [agentImportError, setAgentImportError] = useState<string | null>(null);
  const [agentImportRunResult, setAgentImportRunResult] = useState<AgentImportRunResult | null>(null);

  const [checkpointsFor, setCheckpointsFor] = useState<MemoryView | null>(null);
  const [quickEditFor, setQuickEditFor] = useState<MemoryView | null>(null);
  const [quickEditText, setQuickEditText] = useState("");
  const [quickEditError, setQuickEditError] = useState<string | null>(null);
  const [reviewRejectFor, setReviewRejectFor] = useState<string | null>(null);
  const [supersedeFor, setSupersedeFor] = useState<string | null>(null);
  const [supersedeTargetId, setSupersedeTargetId] = useState("");
  const [supersedeError, setSupersedeError] = useState<string | null>(null);
  const [checkpoints, setCheckpoints] = useState<CheckpointEntry[] | null>(null);
  const [checkpointsError, setCheckpointsError] = useState<string | null>(null);
  const [checkpointsBusy, setCheckpointsBusy] = useState(false);

  // Agents tab state.
  const [agentProfiles, setAgentProfiles] = useState<AgentProfileView[] | null>(null);
  const [agentProfilesError, setAgentProfilesError] = useState<string | null>(null);
  const [namespaceCounts, setNamespaceCounts] = useState<Record<string, number> | null>(null);
  const [namespaceCountsError, setNamespaceCountsError] = useState<string | null>(null);

  useEffect(() => {
    if (tab === "memories") loadMemories();
    if (tab === "review") loadPendingReview();
    if (tab === "episodic") loadEpisodes();
    if (tab === "stats") {
      loadStats();
      loadCompliance();
      loadEffectiveness();
    }
    if (tab === "system") {
      loadCapabilities();
      loadMetrics();
    }
    if (tab === "agents") {
      loadAgentProfiles();
      loadNamespaceCounts();
    }
    if (tab === "settings") checkConnection();
  }, [tab, namespaceFilter, page, episodeStatusFilter]);

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

  async function loadEffectiveness() {
    try {
      const result = await getEffectivenessReport({});
      setEffectiveness(result);
      setEffectivenessError(null);
    } catch (e) {
      setEffectivenessError(String(e));
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

  async function loadCapabilities() {
    try {
      const result = await getCapabilities();
      setCapabilities(result);
      setCapabilitiesError(null);
    } catch (e) {
      setCapabilities(null);
      setCapabilitiesError(String(e));
    }
  }

  async function loadMetrics() {
    try {
      const result = await getMetrics();
      setMetrics(result);
      setMetricsError(null);
    } catch (e) {
      setMetrics(null);
      setMetricsError(String(e));
    }
  }

  /** O(n) scan over the whole store — only ever triggered by an explicit
   * click, never on tab load or the periodic refresh. */
  async function handleRunDoctor() {
    setDoctorRunning(true);
    try {
      const result = await runDoctor();
      setDoctorReport(result);
      setDoctorError(null);
    } catch (e) {
      setDoctorReport(null);
      setDoctorError(String(e));
    } finally {
      setDoctorRunning(false);
    }
  }

  async function loadAgentProfiles() {
    try {
      const result = await getAgentProfiles();
      setAgentProfiles(result);
      setAgentProfilesError(null);
    } catch (e) {
      setAgentProfiles(null);
      setAgentProfilesError(String(e));
    }
  }

  /** Namespaces have no first-class entity/count endpoint — this fetches
   * each namespace's memories (capped at 5000) and counts them, reusing
   * the existing list endpoint rather than adding a new one. */
  async function loadNamespaceCounts() {
    const namespaces = stats?.namespaces ?? [];
    if (namespaces.length === 0) {
      setNamespaceCounts({});
      return;
    }
    try {
      const entries = await Promise.all(
        namespaces.map(async (ns) => {
          const mems = await listMemories({ namespace: ns, limit: 5000, offset: 0 });
          return [ns, mems.length] as const;
        }),
      );
      setNamespaceCounts(Object.fromEntries(entries));
      setNamespaceCountsError(null);
    } catch (e) {
      setNamespaceCounts(null);
      setNamespaceCountsError(String(e));
    }
  }

  async function handleExport() {
    setExportBusy(true);
    try {
      const result: ExportResult = await exportMemories({
        format: exportFormat,
        namespace: exportNamespace || undefined,
      });
      // The downloaded file is exactly the body `/api/import` expects back —
      // Export and Import round-trip through the same JSON shape.
      downloadBlob(
        JSON.stringify(result),
        `memvault-export-${exportFormat}-${exportNamespace || "all"}.json`,
      );
    } catch (e) {
      alert(t("alert.exportFailed", { error: String(e) }));
    } finally {
      setExportBusy(false);
    }
  }

  async function handleImportFile(e: React.ChangeEvent<HTMLInputElement>) {
    const file = e.target.files?.[0];
    if (!file) return;
    setImportBusy(true);
    setImportError(null);
    try {
      const text = await file.text();
      const parsed = JSON.parse(text) as ExportResult;
      const result = await importMemories({
        format: parsed.format,
        content: parsed.content,
        files: parsed.files,
      });
      setImportResult(result);
      loadMemories();
      loadStats();
      loadPendingReview();
    } catch (err) {
      setImportResult(null);
      setImportError(String(err));
    } finally {
      setImportBusy(false);
      e.target.value = "";
    }
  }

  async function handleBackup() {
    setBackupBusy(true);
    try {
      const { blob, filename } = await createBackup();
      downloadBlob(blob, filename, "application/octet-stream");
    } catch (e) {
      alert(t("alert.backupFailed", { error: String(e) }));
    } finally {
      setBackupBusy(false);
    }
  }

  async function handleImportSkills() {
    if (!skillMarkdown.trim()) return;
    setSkillBusy(true);
    try {
      const result = await importSkills({
        markdown: skillMarkdown,
        namespace: skillNamespace || "global",
        approve: skillApprove,
      });
      setSkillResult(result);
      setSkillError(null);
      loadMemories();
      loadStats();
      loadPendingReview();
    } catch (e) {
      setSkillResult(null);
      setSkillError(String(e));
    } finally {
      setSkillBusy(false);
    }
  }

  async function handleAgentScan() {
    setAgentScanBusy(true);
    try {
      const result = await scanAgentImport();
      setAgentScanResults(result);
      setAgentScanError(null);
    } catch (e) {
      setAgentScanResults(null);
      setAgentScanError(String(e));
    } finally {
      setAgentScanBusy(false);
    }
  }

  async function handleAgentPreview(agentKey: string) {
    setAgentImportBusy(true);
    setAgentImportRunResult(null);
    try {
      const result = await previewAgentImport({
        agent: agentKey,
        namespace: agentImportNamespace || undefined,
      });
      setAgentImportPreview(result);
      setAgentImportError(null);
    } catch (e) {
      setAgentImportPreview(null);
      setAgentImportError(String(e));
    } finally {
      setAgentImportBusy(false);
    }
  }

  async function handleAgentImportRun() {
    if (!agentImportPreview) return;
    setAgentImportBusy(true);
    try {
      const result = await runAgentImport({
        agent: agentImportPreview.agent_key,
        namespace: agentImportNamespace || undefined,
      });
      setAgentImportRunResult(result);
      setAgentImportError(null);
      loadPendingReview();
      loadStats();
    } catch (e) {
      setAgentImportError(String(e));
    } finally {
      setAgentImportBusy(false);
    }
  }

  async function openCheckpoints(m: MemoryView) {
    setCheckpointsFor(m);
    setCheckpoints(null);
    setCheckpointsError(null);
    setCheckpointsBusy(true);
    try {
      const result = await listCheckpoints(m.id);
      setCheckpoints(result);
    } catch (e) {
      setCheckpointsError(String(e));
    } finally {
      setCheckpointsBusy(false);
    }
  }

  async function handleRestoreCheckpoint(historyId: number) {
    if (!confirm(t("confirm.restore"))) {
      return;
    }
    try {
      await restoreCheckpoint(historyId);
      setCheckpointsFor(null);
      loadMemories();
      setSelected(null);
    } catch (e) {
      alert(t("alert.restoreFailed", { error: String(e) }));
    }
  }

  async function loadSessionDetail(sessionId: string) {
    try {
      const result = await getComplianceSession(sessionId);
      setSessionDetail(result);
      setSessionDetailError(null);
    } catch (e) {
      setSessionDetail(null);
      setSessionDetailError(String(e));
    }
  }

  async function loadEpisodes() {
    try {
      const result = await listEpisodes({
        status: (episodeStatusFilter || undefined) as OutcomeStatus | undefined,
        limit: 100,
      });
      setEpisodes(result);
    } catch (e) {
      console.error("Failed to load episodes:", e);
    }
  }

  async function submitOutcome(e: React.FormEvent) {
    e.preventDefault();
    if (!outcomeForm.task.trim()) return;
    try {
      const result = await recordOutcome({
        task: outcomeForm.task.trim(),
        status: outcomeForm.status,
        cause: outcomeForm.cause.trim() || undefined,
        task_type: outcomeForm.task_type.trim() || undefined,
        namespace: outcomeForm.namespace.trim() || "global",
      });
      setOutcomeResult(result);
      setOutcomeError(null);
      setOutcomeForm({
        task: "",
        status: "success",
        cause: "",
        task_type: "",
        namespace: outcomeForm.namespace,
      });
      await loadEpisodes();
    } catch (err) {
      setOutcomeError(String(err));
      setOutcomeResult(null);
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
        mode: searchMode,
        namespace: namespaceFilter || undefined,
        expandRelations,
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
    if (!confirm(t("confirm.delete"))) return;
    try {
      await rejectMemory(id);
      loadMemories();
      loadPendingReview();
      setSelected(null);
    } catch (e) {
      console.error("Reject failed:", e);
    }
  }

  /** Review tab reject — goes through the dedicated inbox endpoint rather
   * than the generic hard-delete `handleReject` uses. Confirmation is a
   * modal (see reviewRejectFor below), not a native confirm(), to match
   * the rest of the app's dialog style. */
  function openReviewReject(id: string) {
    setReviewRejectFor(id);
  }

  async function confirmReviewReject() {
    if (!reviewRejectFor) return;
    try {
      await rejectPendingMemory(reviewRejectFor);
      loadPendingReview();
      setSelected(null);
    } catch (e) {
      console.error("Reject failed:", e);
    } finally {
      setReviewRejectFor(null);
    }
  }

  /** Fix up wording before approving, in one step — content is fixed and
   * the memory leaves the review inbox immediately (server sets
   * human_reviewed=true), rather than a separate edit-then-approve pass. */
  function openQuickEdit(m: MemoryView) {
    setQuickEditFor(m);
    setQuickEditText(m.content);
    setQuickEditError(null);
  }

  async function submitQuickEdit() {
    if (!quickEditFor) return;
    try {
      await editPendingMemory(quickEditFor.id, { editedContent: quickEditText });
      loadMemories();
      loadPendingReview();
      setSelected(null);
      setQuickEditFor(null);
    } catch (e) {
      setQuickEditError(String(e));
    }
  }

  function openSupersede(id: string) {
    setSupersedeFor(id);
    setSupersedeTargetId("");
    setSupersedeError(null);
  }

  async function confirmSupersede() {
    if (!supersedeFor || !supersedeTargetId.trim()) return;
    try {
      await supersedeMemory(supersedeFor, supersedeTargetId.trim());
      loadMemories();
      loadStats();
      setSelected(null);
      setSupersedeFor(null);
    } catch (e) {
      setSupersedeError(String(e));
    }
  }

  async function handlePreviewSession(agentId: string, contextHint?: string) {
    setPreviewBusy(true);
    setPreviewError(null);
    setSessionPreview(null);
    try {
      setSessionPreview(await previewSession({ agentId, contextHint }));
    } catch (e) {
      setPreviewError(String(e));
    } finally {
      setPreviewBusy(false);
    }
  }

  async function handleMarkRead(id: string) {
    try {
      await confirmRead([id]);
      // Optimistic local refresh: the server bumped access_count by one.
      if (selected?.id === id) setSelected({ ...selected, access_count: selected.access_count + 1 });
      loadMemories();
    } catch (e) {
      console.error("Confirm read failed:", e);
    }
  }

  async function handlePromote() {
    try {
      const result = await runPromote();
      alert(t("alert.promote", { l2: result.promoted_to_l2, l3: result.promoted_to_l3 }));
      loadMemories();
      loadStats();
    } catch (e) {
      console.error("Promote failed:", e);
    }
  }

  async function handleDecay() {
    try {
      const result = await runDecay();
      alert(t("alert.decay", { updated: result.updated, archived: result.archived }));
      loadMemories();
      loadStats();
    } catch (e) {
      console.error("Decay failed:", e);
    }
  }

  async function handleDedup() {
    try {
      const result = await runDedup();
      alert(t("alert.dedup", { unique: result.unique_count, duplicates: result.duplicate_count }));
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
          visibility: values.visibility,
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
          visibility: values.visibility,
          skill_trigger: isSkill ? values.skillTrigger || null : null,
          skill_steps: isSkill ? skillSteps : null,
          skill_verification: isSkill ? values.skillVerification || null : null,
        });
      }
      setFormOpen(false);
      setEditingId(null);
      setSelected(null);
      loadMemories();
      loadStats();
    } catch (e) {
      alert(t("alert.saveFailed", { error: String(e) }));
    }
  }

  function openExtractPanel() {
    setExtractText("");
    setExtractMode("rule");
    setExtractResult(null);
    setExtractSelected(new Set());
    setExtractError(null);
    setExtractOpen(true);
  }

  async function runExtraction() {
    if (!extractText.trim()) return;
    setExtracting(true);
    try {
      const result = await extractMemories({ text: extractText, mode: extractMode });
      setExtractResult(result);
      // Pre-select every candidate — most extractions are small and mostly useful.
      setExtractSelected(new Set(result.memories.map((_, i) => i)));
      setExtractError(null);
    } catch (e) {
      setExtractResult(null);
      setExtractError(String(e));
    } finally {
      setExtracting(false);
    }
  }

  function toggleExtractSelected(i: number) {
    setExtractSelected((prev) => {
      const next = new Set(prev);
      if (next.has(i)) next.delete(i);
      else next.add(i);
      return next;
    });
  }

  /** The candidates the user kept after reviewing them here in the panel —
   * that review IS the human-review step, so these save straight to the
   * list (human_reviewed=true), same as a manually authored "New Memory". */
  async function saveSelectedExtracted() {
    if (!extractResult) return;
    const toSave = extractResult.memories.filter((_, i) => extractSelected.has(i));
    if (toSave.length === 0) return;
    try {
      for (const c of toSave) {
        await createMemory({
          content: c.content,
          instruction: c.instruction,
          priority: c.priority.toUpperCase(),
          memory_type: c.type.toLowerCase(),
          namespace: extractNamespace || "global",
          tags: c.tags,
        });
      }
      setExtractOpen(false);
      loadMemories();
      loadStats();
    } catch (e) {
      alert(t("alert.saveFailed", { error: String(e) }));
    }
  }

  return (
    <div className="app">
      <header className="header">
        <div className="brand">
          <svg className="brand-mark" viewBox="0 0 32 32" aria-hidden="true">
            <defs>
              <linearGradient id="brandGrad" x1="0" y1="0" x2="1" y2="1">
                <stop offset="0" stopColor="#3b82f6" />
                <stop offset="1" stopColor="#22d3ee" />
              </linearGradient>
            </defs>
            <rect width="32" height="32" rx="7" fill="#0f172a" />
            <path d="M9 10h14M9 16h14M9 22h14" stroke="url(#brandGrad)" strokeWidth="2.6" strokeLinecap="round" />
            <circle cx="24" cy="9" r="2.2" fill="#34d399" />
          </svg>
          <h1>MemVault</h1>
        </div>
        <nav className="tabs">
          {(["memories", "episodic", "search", "review", "stats", "system", "data", "agents", "settings"] as Tab[]).map((tabKey) => (
            <button
              key={tabKey}
              className={tab === tabKey ? "active" : ""}
              onClick={() => setTab(tabKey)}
            >
              {tabKey === "memories" && t("tab.memories", { count: memories.length })}
              {tabKey === "episodic" && t("tab.episodic", { count: episodes.length })}
              {tabKey === "search" && t("tab.search")}
              {tabKey === "review" && t("tab.review", { count: pendingReview.length })}
              {tabKey === "stats" && t("tab.stats")}
              {tabKey === "system" && t("tab.system")}
              {tabKey === "data" && t("tab.data")}
              {tabKey === "agents" && t("tab.agents")}
              {tabKey === "settings" && t("tab.settings")}
            </button>
          ))}
        </nav>
        <div className="header-controls">
          <button
            className="toggle-btn"
            onClick={() => setTheme(theme === "dark" ? "light" : "dark")}
            aria-label={theme === "dark" ? t("toggle.light") : t("toggle.dark")}
            title={theme === "dark" ? t("toggle.light") : t("toggle.dark")}
          >
            {theme === "dark" ? (
              <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" aria-hidden="true">
                <circle cx="12" cy="12" r="4" />
                <path d="M12 2v2M12 20v2M4.93 4.93l1.41 1.41M17.66 17.66l1.41 1.41M2 12h2M20 12h2M4.93 19.07l1.41-1.41M17.66 6.34l1.41-1.41" />
              </svg>
            ) : (
              <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <path d="M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z" />
              </svg>
            )}
          </button>
          <button
            className="toggle-btn lang-toggle"
            onClick={() => setLocale(locale === "zh" ? "en" : "zh")}
            aria-label={t("toggle.lang")}
            title={t("toggle.lang")}
          >
            {locale === "zh" ? "EN" : "中"}
          </button>
        </div>
        {tab === "memories" && (
          <div className="header-actions">
            <button onClick={openExtractPanel}>{t("header.extract")}</button>
            <button className="new-memory-btn" onClick={openCreateForm}>{t("header.newMemory")}</button>
          </div>
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
                <option value="">{t("memories.allNamespaces")}</option>
                {(stats?.namespaces ?? []).map((ns) => (
                  <option key={ns} value={ns}>
                    {ns}
                  </option>
                ))}
              </select>
              <div className="pagination">
                <button disabled={page === 0} onClick={() => setPage((p) => Math.max(0, p - 1))}>{t("pagination.prev")}</button>
                <span>{t("pagination.page", { n: page + 1 })}</span>
                <button disabled={!hasNextPage} onClick={() => setPage((p) => p + 1)}>{t("pagination.next")}</button>
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

        {tab === "episodic" && (
          <div className="episodic-panel">
            <section className="outcome-form-section">
              <h2>{t("episodic.title")}</h2>
              <p className="section-hint">{t("episodic.hint")}</p>
              <form className="outcome-form" onSubmit={submitOutcome}>
                <div className="form-row">
                  <label>{t("episodic.task")}<input
                      value={outcomeForm.task}
                      onChange={(e) => setOutcomeForm({ ...outcomeForm, task: e.target.value })}
                      placeholder={t("episodic.taskPlaceholder")}
                      required
                    />
                  </label>
                  <label>{t("episodic.status")}<select
                      value={outcomeForm.status}
                      onChange={(e) =>
                        setOutcomeForm({ ...outcomeForm, status: e.target.value as OutcomeStatus })
                      }
                    >
                      {OUTCOME_STATUSES.map((s) => (
                        <option key={s} value={s}>
                          {s}
                        </option>
                      ))}
                    </select>
                  </label>
                  <label>{t("episodic.type")}<input
                      value={outcomeForm.task_type}
                      onChange={(e) =>
                        setOutcomeForm({ ...outcomeForm, task_type: e.target.value })
                      }
                      placeholder={t("episodic.typePlaceholder")}
                    />
                  </label>
                  <label>{t("episodic.namespace")}<input
                      value={outcomeForm.namespace}
                      onChange={(e) =>
                        setOutcomeForm({ ...outcomeForm, namespace: e.target.value })
                      }
                    />
                  </label>
                </div>
                <label>{t("episodic.cause")}<input
                    value={outcomeForm.cause}
                    onChange={(e) => setOutcomeForm({ ...outcomeForm, cause: e.target.value })}
                    placeholder={t("episodic.causePlaceholder")}
                  />
                </label>
                <button type="submit">{t("episodic.record")}</button>
              </form>
              {outcomeResult && (
                <div className="outcome-result">
                  <p>{t("episodic.recorded", { id: outcomeResult.id })}</p>
                  {outcomeResult.lesson && (
                    <p className="lesson-line">{t("episodic.lesson", { source: outcomeResult.lesson.source, lesson: outcomeResult.lesson.lesson })}</p>
                  )}
                  {outcomeResult.lesson?.escalation_hint && (
                    <p className="escalation-hint">{outcomeResult.lesson.escalation_hint}</p>
                  )}
                </div>
              )}
              {outcomeError && <p className="outcome-error">{outcomeError}</p>}
            </section>

            <section className="episode-list-section">
              <div className="list-toolbar">
                <h2>{t("episodic.listTitle", { count: episodes.length })}</h2>
                <select
                  value={episodeStatusFilter}
                  onChange={(e) => setEpisodeStatusFilter(e.target.value)}
                  aria-label={t("episodic.filterAria")}
                >
                  <option value="">{t("episodic.allOutcomes")}</option>
                  {OUTCOME_STATUSES.map((s) => (
                    <option key={s} value={s}>
                      {s}
                    </option>
                  ))}
                </select>
              </div>
              {episodes.length === 0 ? (
                <p className="empty">{t("episodic.empty")}</p>
              ) : (
                <table className="episode-table">
                  <thead>
                    <tr>
                      <th>{t("episodic.colTime")}</th>
                      <th>{t("episodic.task")}</th>
                      <th>{t("episodic.status")}</th>
                      <th>{t("episodic.type")}</th>
                      <th>{t("episodic.colCause")}</th>
                      <th>{t("episodic.colLesson")}</th>
                    </tr>
                  </thead>
                  <tbody>
                    {episodes.map((ep) => (
                      <tr key={ep.memory_id}>
                        <td className="episode-time">
                          {new Date(ep.occurred_at).toLocaleString()}
                        </td>
                        <td>{ep.task}</td>
                        <td>
                          <span className={`status-badge status-${ep.status}`}>{ep.status}</span>
                        </td>
                        <td>{ep.task_type ?? "—"}</td>
                        <td>{ep.cause ?? "—"}</td>
                        <td className="episode-lesson">{ep.lesson ?? "—"}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              )}
            </section>
          </div>
        )}

        {tab === "search" && (
          <div className="search-panel">
            <div className="search-bar">
              <select
                value={searchMode}
                onChange={(e) => setSearchMode(e.target.value)}
                aria-label={t("search.modeAria")}
              >
                {SEARCH_MODES.map((mode) => (
                  <option key={mode} value={mode}>
                    {mode}
                  </option>
                ))}
              </select>
              <input
                value={searchQuery}
                onChange={(e) => setSearchQuery(e.target.value)}
                onKeyDown={(e) => e.key === "Enter" && doSearch()}
                placeholder={t("search.placeholder")}
              />
              <label className="expand-relations-toggle">
                <input
                  type="checkbox"
                  checked={expandRelations}
                  onChange={(e) => setExpandRelations(e.target.checked)}
                />{t("search.expandRelations")}</label>
              <button aria-label={t("search.runAria")} onClick={doSearch}>{t("tab.search")}</button>
            </div>
            <div className="results">
              {searchResults.map((r) => (
                <div key={r.memory.id} className="search-result">
                  <MemoryCard
                    memory={r.memory}
                    score={r.score}
                    highlight={searchQuery}
                    hitSources={r.hitSources}
                    onClick={() => setSelected(r.memory)}
                  />
                  {r.relations && r.relations.length > 0 && (
                    <ul className="relations-list">
                      {r.relations.map((rel, i) => (
                        <li key={i}>{rel.line}</li>
                      ))}
                    </ul>
                  )}
                </div>
              ))}
              {searchResults.length === 0 && searchQuery && (
                <p className="empty">{t("search.empty")}</p>
              )}
            </div>
          </div>
        )}

        {tab === "review" && (
          <div className="review-panel">
            <h2>{t("review.title", { count: pendingReview.length })}</h2>
            {pendingReview.length === 0 ? (
              <p className="empty">{t("review.empty")}</p>
            ) : (
              pendingReview.map((m) => (
                <div key={m.id} className="review-card">
                  <MemoryCard memory={m} onClick={() => setSelected(m)} />
                  <div className="review-actions">
                    <button className="approve" onClick={() => handleApprove(m.id)}>{t("review.approve")}</button>
                    <button onClick={() => openQuickEdit(m)}>{t("review.quickEdit")}</button>
                    <button className="reject" onClick={() => openReviewReject(m.id)}>{t("review.reject")}</button>
                  </div>
                </div>
              ))
            )}
          </div>
        )}

        {tab === "stats" && stats && (
          <div className="stats-panel">
            <div className="stat-grid">
              <StatCard label={t("stats.total")} value={stats.total} />
              <StatCard label={t("stats.must")} value={stats.must_count} />
              <StatCard label={t("stats.references")} value={stats.reference_count} />
              <StatCard label={t("stats.reviewed")} value={stats.reviewed_count} />
              <StatCard label={t("stats.l3")} value={stats.layers.l3} />
              <StatCard label={t("stats.l2")} value={stats.layers.l2} />
              <StatCard label={t("stats.l1")} value={stats.layers.l1} />
              <StatCard label={t("stats.skills")} value={stats.skills} />
            </div>
            <div className="actions-section">
              <h3>{t("stats.pipeline")}</h3>
              <button onClick={handlePromote}>{t("stats.promote")}</button>
              <button onClick={handleDecay}>{t("stats.decay")}</button>
              <button onClick={handleDedup}>{t("stats.dedup")}</button>
            </div>
            <div className="agents-section">
              <h3>{t("stats.agents")}</h3>
              {stats.agents.length === 0 ? (
                <p className="empty">{t("stats.noAgents")}</p>
              ) : (
                <ul>
                  {stats.agents.map((a) => (
                    <li key={a}>{a}</li>
                  ))}
                </ul>
              )}
            </div>
            <div className="compliance-section">
              <h3>{t("stats.complianceTitle", { count: compliance?.recent_sessions.length ?? 0 })}</h3>
              {complianceError && (
                <p className="empty">{t("stats.complianceDisabled")}</p>
              )}
              {compliance && (
                <>
                  <div className="stat-grid compliance-grid">
                    <StatCard label={t("stats.sessions")} value={compliance.total_sessions} />
                    <StatCard label={t("stats.overallRate")} value={Math.round(compliance.overall_rate * 100)} suffix="%" />
                    <StatCard label={t("stats.mustRate")} value={Math.round(compliance.must_rate * 100)} suffix="%" />
                  </div>
                  {compliance.recent_sessions.length > 0 && (
                    <table className="compliance-table">
                      <thead>
                        <tr>
                          <th>{t("stats.colSession")}</th>
                          <th>{t("detail.agent")}</th>
                          <th>{t("stats.injected")}</th>
                          <th>{t("stats.colMust")}</th>
                          <th>{t("stats.colRef")}</th>
                          <th>{t("stats.colRate")}</th>
                        </tr>
                      </thead>
                      <tbody>
                        {compliance.recent_sessions.map((r) => (
                          <tr
                            key={r.inject_session_id}
                            className="clickable-row"
                            onClick={() => loadSessionDetail(r.inject_session_id)}
                          >
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
                  {sessionDetailError && <p className="empty">{sessionDetailError}</p>}
                  {sessionDetail && (
                    <div className="session-detail">
                      <div className="session-detail-header">
                        <h4>{t("stats.sessionTitle", { id: sessionDetail.inject_session_id, agent: sessionDetail.agent_id })}</h4>
                        <button className="icon-btn" aria-label={t("stats.closeAria")} onClick={() => setSessionDetail(null)}>×</button>
                      </div>
                      <div className="stat-grid compliance-grid">
                        <StatCard label={t("stats.injected")} value={sessionDetail.total_injected} />
                        <StatCard label={t("stats.mustFollowed")} value={sessionDetail.must_followed} />
                        <StatCard label={t("stats.mustViolated")} value={sessionDetail.must_violated} />
                        <StatCard label={t("stats.refFollowed")} value={sessionDetail.ref_followed} />
                        <StatCard label={t("stats.refViolated")} value={sessionDetail.ref_violated} />
                        <StatCard label={t("stats.pending")} value={sessionDetail.pending} />
                        <StatCard
                          label={t("stats.compliance")}
                          value={Math.round(sessionDetail.compliance_rate * 100)}
                          suffix="%"
                        />
                      </div>
                    </div>
                  )}
                </>
              )}
            </div>
          </div>
        )}

        {tab === "stats" && stats && (
          <div className="compliance-section effectiveness-section">
            <h3>{t("stats.effectivenessTitle")}</h3>
            {effectivenessError && (
              <p className="empty">{t("stats.effectivenessDisabled")}</p>
            )}
            {effectiveness && (
              <div className="stat-grid compliance-grid">
                <StatCard label={t("stats.useful")} value={effectiveness.useful} />
                <StatCard label={t("stats.neutral")} value={effectiveness.neutral} />
                <StatCard label={t("stats.harmful")} value={effectiveness.harmful} />
                <StatCard label={t("stats.insufficient")} value={effectiveness.insufficient} />
                <StatCard label={t("stats.unjudged")} value={effectiveness.unjudged} />
                <StatCard
                  label={t("stats.usefulRate")}
                  value={Math.round(effectiveness.usefulness_rate * 100)}
                  suffix="%"
                />
                <StatCard
                  label={t("stats.harmfulRate")}
                  value={Math.round(effectiveness.harmful_rate * 100)}
                  suffix="%"
                />
                <StatCard
                  label={t("stats.coverage")}
                  value={Math.round(effectiveness.coverage * 100)}
                  suffix="%"
                />
              </div>
            )}
          </div>
        )}

        {tab === "system" && (
          <div className="system-panel">
            <section className="system-section">
              <h3>{t("system.capabilities")}</h3>
              <p className="section-hint">{t("system.capabilitiesHint")}</p>
              {capabilitiesError && <p className="empty">{capabilitiesError}</p>}
              {capabilities && (
                <table className="capabilities-table">
                  <tbody>
                    {capabilities.map((c) => (
                      <tr key={c.name}>
                        <td>{c.available ? "✅" : "❌"}</td>
                        <td>{c.name}</td>
                        <td className="capability-note">{c.note}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              )}
            </section>

            <section className="system-section">
              <h3>{t("system.metrics")}</h3>
              {metricsError && <p className="empty">{metricsError}</p>}
              {metrics && (
                <div className="stat-grid metrics-grid">
                  {metrics
                    .filter((m) => m.name.startsWith("memvault_"))
                    .map((m, i) => (
                      <StatCard
                        key={`${m.name}-${i}`}
                        label={m.name.replace(/^memvault_/, "").replace(/_/g, " ")}
                        value={m.value}
                      />
                    ))}
                  {metrics.filter((m) => m.name.startsWith("memvault_")).length === 0 && (
                    <p className="empty">{t("system.metricsEmpty")}</p>
                  )}
                </div>
              )}
            </section>

            <section className="system-section">
              <h3>{t("system.doctor")}</h3>
              <p className="section-hint">{t("system.doctorHint")}</p>
              <button onClick={handleRunDoctor} disabled={doctorRunning}>
                {doctorRunning ? t("system.doctorScanning") : t("system.doctorRun")}
              </button>
              {doctorError && <p className="outcome-error">{doctorError}</p>}
              {doctorReport && (
                <div className="doctor-report">
                  <p className="section-hint">{t("system.doctorSummary", { count: doctorReport.total_memories, warnings: doctorReport.findings.filter((f) => f.severity === "warn" && f.count > 0).length })}</p>
                  {(["warn", "info"] as const).map((sev) => {
                    const findings = doctorReport.findings.filter((f) => f.severity === sev);
                    if (findings.length === 0) return null;
                    return (
                      <div key={sev} className="doctor-severity-group">
                        <h4 className={`doctor-severity doctor-severity-${sev}`}>{sev}</h4>
                        {findings.map((f) => (
                          <div key={f.check} className="doctor-finding">
                            <div className="doctor-finding-header">
                              <strong>{f.check}</strong>
                              <span className="doctor-finding-count">{f.count}</span>
                            </div>
                            {f.items.length > 0 && (
                              <ul className="doctor-finding-items">
                                {f.items.map((item) => (
                                  <li key={item.id}>
                                    <code>{item.id}</code> — {item.detail}
                                  </li>
                                ))}
                              </ul>
                            )}
                          </div>
                        ))}
                      </div>
                    );
                  })}
                </div>
              )}
            </section>
          </div>
        )}

        {tab === "data" && (
          <div className="data-panel">
            <section className="system-section">
              <h3>{t("data.exportImport")}</h3>
              <p className="section-hint">{t("data.exportHint")}</p>
              <div className="form-row">
                <label>{t("data.format")}<select value={exportFormat} onChange={(e) => setExportFormat(e.target.value as "json" | "markdown")}>
                    <option value="json">json</option>
                    <option value="markdown">markdown</option>
                  </select>
                </label>
                <label>{t("data.namespaceFilter")}<input value={exportNamespace} onChange={(e) => setExportNamespace(e.target.value)} placeholder={t("data.namespacePlaceholder")} />
                </label>
              </div>
              <div className="detail-actions">
                <button onClick={handleExport} disabled={exportBusy}>
                  {exportBusy ? t("data.exporting") : t("data.export")}
                </button>
                <label className="file-import-btn">
                  {importBusy ? t("data.importing") : t("data.importFile")}
                  <input type="file" accept="application/json" onChange={handleImportFile} disabled={importBusy} />
                </label>
              </div>
              {importError && <p className="outcome-error">{importError}</p>}
              {importResult && (
                <p className="section-hint">{t("data.imported", { count: importResult.imported })}{importResult.skipped && importResult.skipped.length > 0 && (" · " + t("data.importSkipped", { count: importResult.skipped.length, list: importResult.skipped.map((s) => s.filename + " (" + s.reason + ")").join(", ") }))}</p>
              )}
            </section>

            <section className="system-section">
              <h3>{t("data.backup")}</h3>
              <p className="section-hint">{t("data.backupHint")}</p>
              <button onClick={handleBackup} disabled={backupBusy}>
                {backupBusy ? t("data.creating") : t("data.createBackup")}
              </button>
            </section>

            <section className="system-section">
              <h3>{t("data.importSkills")}</h3>
              <p className="section-hint">{t("data.importSkillsHint")}</p>
              <div className="detail-field">
                <textarea
                  rows={6}
                  value={skillMarkdown}
                  onChange={(e) => setSkillMarkdown(e.target.value)}
                  placeholder={t("data.sopPlaceholder")}
                />
              </div>
              <div className="form-row">
                <label>{t("episodic.namespace")}<input value={skillNamespace} onChange={(e) => setSkillNamespace(e.target.value)} />
                </label>
                <label className="checkbox-label">
                  <input type="checkbox" checked={skillApprove} onChange={(e) => setSkillApprove(e.target.checked)} />{t("data.approveImmediately")}</label>
              </div>
              <button onClick={handleImportSkills} disabled={!skillMarkdown.trim() || skillBusy}>
                {skillBusy ? t("data.importing") : t("data.importSkillsBtn")}
              </button>
              {skillError && <p className="outcome-error">{skillError}</p>}
              {skillResult && (
                <div className="section-hint">
                  <p>
                    {t("data.importedSkills", { count: skillResult.imported.length })}
                    {skillResult.skipped_no_steps > 0 && ` · ${t("data.skippedNoSteps", { count: skillResult.skipped_no_steps })}`}
                  </p>
                  <ul>
                    {skillResult.imported.map((s) => (
                      <li key={s.id}>{t("data.skillLine", { title: s.title, count: s.steps })}</li>
                    ))}
                  </ul>
                </div>
              )}
            </section>

            <section className="system-section">
              <h3>{t("data.agentImport")}</h3>
              <p className="section-hint">{t("data.agentImportHint")}</p>
              <button onClick={handleAgentScan} disabled={agentScanBusy}>
                {agentScanBusy ? t("data.scanning") : t("data.scanAgents")}
              </button>
              {agentScanError && <p className="empty">{agentScanError}</p>}
              {agentScanResults && (
                <table className="capabilities-table">
                  <tbody>
                    {agentScanResults.map((a) => (
                      <tr key={a.agent_key}>
                        <td>{a.found ? "✅" : "—"}</td>
                        <td>{a.display_name}</td>
                        <td className="capability-note">
                          {a.found ? a.paths.join(", ") : t("data.notDetected")}
                        </td>
                        <td>
                          {a.found && (
                            <button onClick={() => handleAgentPreview(a.agent_key)} disabled={agentImportBusy}>{t("data.preview")}</button>
                          )}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              )}

              {agentImportError && <p className="outcome-error">{agentImportError}</p>}

              {agentImportPreview && (
                <div className="extract-results">
                  <div className="form-row">
                    <label>{t("data.namespaceOverride")}<input
                        value={agentImportNamespace}
                        onChange={(e) => setAgentImportNamespace(e.target.value)}
                        placeholder={agentImportPreview.candidates[0]?.namespace ?? "global"}
                      />
                    </label>
                  </div>
                  <p className="section-hint">{t("data.previewHint", { display: agentImportPreview.display_name, files: agentImportPreview.files_scanned, candidates: agentImportPreview.candidates.length })}</p>
                  {agentImportPreview.candidates.length === 0 ? (
                    <p className="empty">{t("data.noCandidates")}</p>
                  ) : (
                    <>
                      <ul className="extract-candidate-list">
                        {agentImportPreview.candidates.map((c, i) => (
                          <li key={i} className="extract-candidate">
                            <span className={`priority ${c.priority.toLowerCase()}`}>{c.priority}</span>
                            <span className="type">{c.type}</span>
                            {c.parse_confidence === "Heuristic" && (
                              <span className="tag confidence-heuristic" title={t("data.heuristicTitle")}>
                                {t("data.heuristic")}</span>
                            )}
                            {c.duplicate_of && <span className="tag">{t("data.duplicateOf", { id: c.duplicate_of })}</span>}
                            {" "}
                            {c.content}
                          </li>
                        ))}
                      </ul>
                      <button
                        className="approve"
                        onClick={handleAgentImportRun}
                        disabled={agentImportBusy}
                      >
                        {agentImportBusy ? t("data.importing") : t("data.importCandidates", { count: agentImportPreview.candidates.length })}
                      </button>
                    </>
                  )}
                </div>
              )}

              {agentImportRunResult && (
                <p className="section-hint">
  {t("data.importRunPrefix", { count: agentImportRunResult.imported.length, skipped: agentImportRunResult.duplicates_skipped })}{" "}
  <button onClick={() => setTab("review")}>{t("data.reviewTab")}</button>{" "}
  {t("data.importRunSuffix")}
</p>
              )}
            </section>
          </div>
        )}

        {tab === "agents" && (
          <div className="system-panel">
            <section className="system-section">
              <h3>{t("agents.title")}</h3>
              <p className="section-hint">{t("agents.hint")}</p>
              {agentProfilesError && <p className="empty">{agentProfilesError}</p>}
              {agentProfiles && (
                <table className="agents-table">
                  <thead>
                    <tr>
                      <th>{t("agents.colId")}</th>
                      <th>{t("episodic.type")}</th>
                      <th>{t("agents.colDesc")}</th>
                      <th>{t("agents.colMax")}</th>
                      <th>{t("agents.colBudget")}</th>
                      <th>{t("agents.colPriority")}</th>
                      <th>{t("agents.colNamespace")}</th>
                      <th>{t("agents.colExcluded")}</th>
                      <th>{t("agents.colApiKey")}</th>
                      <th>{t("data.preview")}</th>
                    </tr>
                  </thead>
                  <tbody>
                    {agentProfiles.map((p) => (
                      <tr key={p.id}>
                        <td><code>{p.id}</code></td>
                        <td>{p.agent_type}</td>
                        <td>{p.description || "—"}</td>
                        <td>{p.inject_rules.max_memories}</td>
                        <td>{p.inject_rules.token_budget}</td>
                        <td>{p.inject_rules.priority_order.join(", ")}</td>
                        <td>{p.inject_rules.namespace_filter.join(", ") || "—"}</td>
                        <td>{p.inject_rules.exclude_types.join(", ") || "—"}</td>
                        <td>{p.has_api_key ? "🔒" : "—"}</td>
                        <td>
                          <button onClick={() => handlePreviewSession(p.id)}>{t("data.preview")}</button>
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              )}
              {previewBusy && <p className="empty">{t("agents.loadingPreview")}</p>}
              {previewError && <p className="empty">{previewError}</p>}
              {sessionPreview && (
                <div className="session-preview">
                  <h4>{t("agents.whatReceives", { profile: sessionPreview.agentProfile })}{sessionPreview.injectSessionId && t("agents.sessionSuffix", { id: sessionPreview.injectSessionId })}</h4>
                  <p className="section-hint">{t("agents.previewHint", { format: sessionPreview.format })}</p>
                  {sessionPreview.note && <p className="empty">{sessionPreview.note}</p>}
                  {sessionPreview.results.length > 0 && (
                    <ul className="memory-list">
                      {sessionPreview.results.map((r) => (
                        <li key={r.memory.id} className="preview-entry">
                          <span className={`priority ${r.memory.priority.toLowerCase()}`}>
                            {r.memory.priority}
                          </span>
                          <span>{r.memory.content}</span>
                          {(r.hitSources ?? []).length > 0 && (
                            <span className="tag">{r.hitSources!.join(", ")}</span>
                          )}
                        </li>
                      ))}
                    </ul>
                  )}
                  {sessionPreview.skipped.length > 0 && (
                    <div>
                      <strong>{t("agents.drops", { count: sessionPreview.skipped.length })}</strong>
                      <ul>
                        {sessionPreview.skipped.map((s) => (
                          <li key={s.id}>
                            <code>{s.id}</code> — {s.reason}
                          </li>
                        ))}
                      </ul>
                    </div>
                  )}
                  {sessionPreview.results.length === 0 &&
                    sessionPreview.skipped.length === 0 &&
                    !sessionPreview.note && <p className="empty">{t("agents.nothing")}</p>}
                </div>
              )}
            </section>

            <section className="system-section">
              <h3>{t("agents.namespaces")}</h3>
              <p className="section-hint">{t("agents.namespacesHint")}</p>
              {namespaceCountsError && <p className="empty">{namespaceCountsError}</p>}
              {namespaceCounts && Object.keys(namespaceCounts).length === 0 && (
                <p className="empty">{t("agents.noNamespaces")}</p>
              )}
              {namespaceCounts && Object.keys(namespaceCounts).length > 0 && (
                <div className="stat-grid metrics-grid">
                  {Object.entries(namespaceCounts).map(([ns, count]) => (
                    <StatCard key={ns} label={ns} value={count} suffix={count >= 5000 ? "+" : ""} />
                  ))}
                </div>
              )}
            </section>
          </div>
        )}

        {tab === "settings" && (
          <div className="settings-panel">
            <h2>{t("settings.title")}</h2>
            <div className="settings-field">
              <label>{t("settings.backend")}</label>
              <p className="settings-hint">
                {connected === null && <span>{t("settings.checking")}</span>}
                {connected === true && (
                  <span className="status-connected">{t("settings.connected")}</span>
                )}
                {connected === false && (
                  <span className="status-disconnected">{t("settings.unreachable")}</span>
                )}
              </p>
            </div>
            <div className="settings-field">
              <label>{t("agents.colApiKey")}</label>
              <input
                type="password"
                value={apiKeyInput}
                onChange={(e) => {
                  setApiKeyInput(e.target.value);
                  setApiKeySaved(false);
                }}
              />
              <p className="settings-hint">{t("settings.apiKeyHint")}</p>
              <button onClick={saveApiKeyInfo}>{t("settings.save")}</button>
              {apiKeySaved && <span className="settings-saved">{t("settings.saved")}</span>}
            </div>
            <div className="settings-field">
              <label>{t("settings.agentId")}</label>
              <p className="settings-hint">{t("settings.agentIdHint")}</p>
            </div>
            <div className="settings-field">
              <label>{t("settings.access")}</label>
              <p className="settings-hint">{t("settings.accessHint")}</p>
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
          onMarkRead={handleMarkRead}
          onSupersede={openSupersede}
          onHistory={() => openCheckpoints(selected)}
          relations={selectedRelations}
          evidence={selectedEvidence}
          onEdit={() => openEditForm(selected)}
        />
      )}

      {checkpointsFor && (
        <div className="detail-overlay" onClick={() => setCheckpointsFor(null)}>
          <div className="detail-panel" onClick={(e) => e.stopPropagation()}>
            <button className="close-btn" aria-label={t("detail.close")} onClick={() => setCheckpointsFor(null)}>×</button>
            <h2>{t("history.title", { id: checkpointsFor.id })}</h2>
            {checkpointsBusy && <p className="empty">{t("history.loading")}</p>}
            {checkpointsError && <p className="empty">{checkpointsError}</p>}
            {checkpoints && checkpoints.length === 0 && (
              <p className="empty">{t("history.empty")}</p>
            )}
            {checkpoints && checkpoints.length > 0 && (
              <ul className="extract-candidate-list">
                {checkpoints.map((c) => (
                  <li key={c.history_id} className="extract-candidate checkpoint-entry">
                    <span>{new Date(c.changed_at).toLocaleString()}</span>
                    <span className="tag">{c.operation}</span>
                    <button onClick={() => handleRestoreCheckpoint(c.history_id)}>{t("history.restore")}</button>
                  </li>
                ))}
              </ul>
            )}
          </div>
        </div>
      )}

      {quickEditFor && (
        <div className="detail-overlay" onClick={() => setQuickEditFor(null)}>
          <div className="detail-panel" onClick={(e) => e.stopPropagation()}>
            <button className="close-btn" aria-label={t("detail.close")} onClick={() => setQuickEditFor(null)}>×</button>
            <h2>{t("review.quickEdit")}</h2>
            <p className="section-hint">{t("quickEdit.hint")}</p>
            <div className="detail-field">
              <label>{t("detail.content")}</label>
              <textarea
                rows={4}
                value={quickEditText}
                onChange={(e) => setQuickEditText(e.target.value)}
              />
            </div>
            {quickEditError && <p className="outcome-error">{quickEditError}</p>}
            <div className="detail-actions">
              <button className="approve" onClick={submitQuickEdit} disabled={!quickEditText.trim()}>{t("quickEdit.save")}</button>
              <button className="reject" onClick={() => setQuickEditFor(null)}>{t("cancel")}</button>
            </div>
          </div>
        </div>
      )}

      {reviewRejectFor && (
        <div className="detail-overlay" onClick={() => setReviewRejectFor(null)}>
          <div className="detail-panel" onClick={(e) => e.stopPropagation()}>
            <button className="close-btn" aria-label={t("detail.close")} onClick={() => setReviewRejectFor(null)}>×</button>
            <h2>{t("reject.title")}</h2>
            <p className="section-hint">{t("reject.hint")}</p>
            <div className="detail-actions">
              <button className="reject" onClick={confirmReviewReject}>{t("review.reject")}</button>
              <button onClick={() => setReviewRejectFor(null)}>{t("cancel")}</button>
            </div>
          </div>
        </div>
      )}

      {supersedeFor && (
        <div className="detail-overlay" onClick={() => setSupersedeFor(null)}>
          <div className="detail-panel" onClick={(e) => e.stopPropagation()}>
            <button className="close-btn" aria-label={t("detail.close")} onClick={() => setSupersedeFor(null)}>×</button>
            <h2>{t("supersede.title")}</h2>
            <p className="section-hint">{t("supersede.hint")}</p>
            <div className="detail-field">
              <label>{t("supersede.id")}</label>
              <input
                value={supersedeTargetId}
                onChange={(e) => setSupersedeTargetId(e.target.value)}
                placeholder="mem_..."
                autoFocus
              />
            </div>
            {supersedeError && <p className="outcome-error">{supersedeError}</p>}
            <div className="detail-actions">
              <button className="approve" onClick={confirmSupersede} disabled={!supersedeTargetId.trim()}>{t("detail.supersede")}</button>
              <button className="reject" onClick={() => setSupersedeFor(null)}>{t("cancel")}</button>
            </div>
          </div>
        </div>
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

      {extractOpen && (
        <div className="detail-overlay" onClick={() => setExtractOpen(false)}>
          <div className="detail-panel" onClick={(e) => e.stopPropagation()}>
            <button className="close-btn" aria-label={t("detail.close")} onClick={() => setExtractOpen(false)}>×</button>
            <h2>{t("header.extract")}</h2>
            <p className="section-hint">{t("extract.hint")}</p>

            <div className="detail-field">
              <label>{t("extract.text")}</label>
              <textarea
                rows={8}
                value={extractText}
                onChange={(e) => setExtractText(e.target.value)}
                placeholder={t("extract.placeholder")}
              />
            </div>
            <div className="detail-field">
              <label>{t("extract.mode")}</label>
              <select value={extractMode} onChange={(e) => setExtractMode(e.target.value as "rule" | "llm")}>
                <option value="rule">{t("extract.modeRule")}</option>
                <option value="llm">{t("extract.modeLlm")}</option>
              </select>
            </div>
            <div className="detail-field">
              <label>{t("extract.namespace")}</label>
              <input value={extractNamespace} onChange={(e) => setExtractNamespace(e.target.value)} />
            </div>

            <div className="detail-actions">
              <button className="approve" onClick={runExtraction} disabled={!extractText.trim() || extracting}>
                {extracting ? t("extract.running") : t("extract.run")}
              </button>
              <button className="reject" onClick={() => setExtractOpen(false)}>{t("cancel")}</button>
            </div>

            {extractError && <p className="outcome-error">{extractError}</p>}

            {extractResult && (
              <div className="extract-results">
                {extractResult.coverage && (
                  <p className="section-hint">{t("extract.coverage", { input: extractResult.coverage.input_lines, extracted: extractResult.coverage.extracted_lines, noSignal: extractResult.coverage.no_signal_lines, empty: extractResult.coverage.empty_lines })}</p>
                )}
                {extractResult.memories.length === 0 ? (
                  <p className="empty">{t("extract.empty")}</p>
                ) : (
                  <>
                    <ul className="extract-candidate-list">
                      {extractResult.memories.map((c, i) => (
                        <li key={i} className="extract-candidate">
                          <label>
                            <input
                              type="checkbox"
                              checked={extractSelected.has(i)}
                              onChange={() => toggleExtractSelected(i)}
                            />
                            <span className={`priority ${c.priority.toLowerCase()}`}>{c.priority}</span>
                            <span className="type">{c.type}</span>
                            {c.content}
                          </label>
                        </li>
                      ))}
                    </ul>
                    <div className="detail-actions">
                      <button
                        className="approve"
                        onClick={saveSelectedExtracted}
                        disabled={extractSelected.size === 0}
                      >{t("extract.saveSelected", { count: extractSelected.size })}</button>
                    </div>
                  </>
                )}
              </div>
            )}
          </div>
        </div>
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
  const { t } = useI18n();
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
        <p className="empty">{t("memories.empty")}</p>
      )}
    </div>
  );
}

function MemoryCard({
  memory: m,
  score,
  hitSources,
  highlight: highlightQuery,
  active,
  onClick,
}: {
  memory: MemoryView;
  score?: number;
  hitSources?: string[];
  highlight?: string;
  active?: boolean;
  onClick: () => void;
}) {
  const { t } = useI18n();
  return (
    <div className={`memory-card ${active ? "active" : ""}`} onClick={onClick}>
      <div className="card-header">
        <span className={`priority ${m.priority.toLowerCase()}`}>{m.priority}</span>
        <span className="layer">{m.layer}</span>
        <span className="type">{m.memory_type}</span>
        {m.human_reviewed && <span className="reviewed">{t("card.reviewed")}</span>}
        {score !== undefined && <span className="score">{score.toFixed(3)}</span>}
        {hitSources && hitSources.length > 0 && (
          <span className="hits">{hitSources.join(" ")}</span>
        )}
      </div>
      <p className="card-content">
        {highlightQuery ? highlight(m.instruction || m.content, highlightQuery) : m.instruction || m.content}
      </p>
      {m.skill_meta && (
        <div className="skill-info">
          {m.skill_meta.trigger && <span className="skill-trigger">⚡ {m.skill_meta.trigger}</span>}
          {m.skill_meta.steps.length > 0 && <span className="skill-steps">{t("card.steps", { n: m.skill_meta.steps.length })}</span>}
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
  onMarkRead,
  onSupersede,
  relations,
  evidence,
  onHistory,
  onEdit,
}: {
  memory: MemoryView;
  onClose: () => void;
  onApprove: (id: string) => void;
  onReject: (id: string) => void;
  onMarkRead: (id: string) => void;
  onSupersede: (id: string) => void;
  relations: RelationView[];
  evidence: EvidenceChainView | null;
  onHistory: () => void;
  onEdit: () => void;
}) {
  const { t } = useI18n();
  return (
    <div className="detail-overlay" onClick={onClose}>
      <div className="detail-panel" onClick={(e) => e.stopPropagation()}>
        <button className="close-btn" aria-label={t("detail.close")} onClick={onClose}>×</button>
        <h2>{t("detail.title")}</h2>
        {relations.length > 0 && (
          <div className="detail-field">
            <label>{t("detail.relations")}</label>
            <ul className="relations-list">
              {relations.map((rel, i) => <li key={i}>{rel.line}</li>)}
            </ul>
          </div>
        )}
        {evidence && evidence.evidence_count > 0 && (
          <div className="detail-field">
            <label>{t("detail.evidence", { count: evidence.evidence_count, supports: evidence.summary.supports, contradicts: evidence.summary.contradicts })}</label>
            <ul className="relations-list">
              {evidence.evidence.slice(0, 5).map((t2) => (
                <li key={t2.id}>
                  <span>{t2.content}</span>
                  {t2.session_id && <span className="tag">{t2.session_id}</span>}
                </li>
              ))}
              {evidence.evidence_count > 5 && (
                <li className="empty">{t("detail.moreTraces", { count: evidence.evidence_count - 5 })}</li>
              )}
            </ul>
          </div>
        )}

        <div className="detail-field">
          <label>{t("agents.colId")}</label>
          <code>{m.id}</code>
        </div>
        <div className="detail-field">
          <label>{t("detail.priority")}</label>
          <span className={`priority ${m.priority.toLowerCase()}`}>{m.priority}</span>
        </div>
        <div className="detail-field">
          <label>{t("detail.layer")}</label>
          <span>{m.layer}</span>
        </div>
        <div className="detail-field">
          <label>{t("episodic.type")}</label>
          <span>{m.memory_type}</span>
        </div>
        <div className="detail-field">
          <label>{t("detail.content")}</label>
          <p>{m.content}</p>
        </div>
        {m.instruction && (
          <div className="detail-field">
            <label>{t("detail.instruction")}</label>
            <p>{m.instruction}</p>
          </div>
        )}
        <div className="detail-field">
          <label>{t("detail.tags")}</label>
          <div className="tags">
            {m.tags.map((t) => <span key={t} className="tag">{t}</span>)}
            {m.tags.length === 0 && <span className="empty">{t("detail.none")}</span>}
          </div>
        </div>
        <div className="detail-field">
          <label>{t("detail.agent")}</label>
          <span>{m.source_agent_id}</span>
        </div>
        <div className="detail-field">
          <label>{t("episodic.namespace")}</label>
          <span>{m.namespace}</span>
        </div>
        <div className="detail-field">
          <label>{t("detail.visibility")}</label>
          <span>{m.visibility}</span>
        </div>
        <div className="detail-field">
          <label>{t("detail.confidence")}</label>
          <span>{(m.confidence * 100).toFixed(0)}%</span>
        </div>
        <div className="detail-field">
          <label>{t("episodic.status")}</label>
          <span>
            {m.human_reviewed ? t("detail.reviewed") : t("detail.pending")}
            {m.superseded_by && ` — superseded by ${m.superseded_by}`}
          </span>
        </div>
        <div className="detail-field">
          <label>{t("detail.created")}</label>
          <span>{new Date(m.created_at).toLocaleString()}</span>
        </div>
        {m.skill_meta && (
          <div className="detail-field">
            <label>{t("detail.skill")}</label>
            <div className="skill-detail">
              {m.skill_meta.trigger && <p><strong>{t("detail.trigger")}</strong> {m.skill_meta.trigger}</p>}
              {m.skill_meta.steps.length > 0 && (
                <ol>
                  {m.skill_meta.steps.map((s, i) => <li key={i}>{s}</li>)}
                </ol>
              )}
              {m.skill_meta.verification && <p><strong>{t("detail.verify")}</strong> {m.skill_meta.verification}</p>}
              <p><strong>{t("detail.version")}</strong> {m.skill_meta.version}</p>
            </div>
          </div>
        )}
        {m.friction_evidence && (
          <div className="detail-field">
            <label>{t("detail.friction")}</label>
            <span>{m.friction_evidence}</span>
          </div>
        )}

        <div className="detail-actions">
          <button className="edit" onClick={onEdit}>{t("detail.edit")}</button>
          {!m.human_reviewed && (
            <button className="approve" onClick={() => onApprove(m.id)}>{t("review.approve")}</button>
          )}
          <button onClick={() => onMarkRead(m.id)} title={t("detail.markReadTitle")}>{t("detail.markRead")}</button>
          <button onClick={() => onSupersede(m.id)}>{t("supersede.btn")}</button>
          <button onClick={onHistory}>{t("detail.history")}</button>
          <button className="reject" onClick={() => onReject(m.id)}>{t("detail.delete")}</button>
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
  const { t } = useI18n();
  const [values, setValues] = useState<MemoryFormValues>(initial);
  const isSkill = values.memory_type === "skill";

  function set<K extends keyof MemoryFormValues>(key: K, value: MemoryFormValues[K]) {
    setValues((v) => ({ ...v, [key]: value }));
  }

  return (
    <div className="detail-overlay" onClick={onCancel}>
      <div className="detail-panel" onClick={(e) => e.stopPropagation()}>
        <button className="close-btn" aria-label={t("detail.close")} onClick={onCancel}>×</button>
        <h2>{isEdit ? t("form.editTitle") : t("form.newTitle")}</h2>

        <div className="detail-field">
          <label>{t("detail.content")}</label>
          <textarea
            rows={3}
            value={values.content}
            onChange={(e) => set("content", e.target.value)}
          />
        </div>
        <div className="detail-field">
          <label>{t("form.instruction")}</label>
          <textarea
            rows={2}
            value={values.instruction}
            onChange={(e) => set("instruction", e.target.value)}
          />
        </div>
        <div className="detail-field">
          <label>{t("detail.priority")}</label>
          <select value={values.priority} onChange={(e) => set("priority", e.target.value)}>
            {PRIORITIES.map((p) => (
              <option key={p} value={p}>{p}</option>
            ))}
          </select>
        </div>
        <div className="detail-field">
          <label>{t("episodic.type")}</label>
          <select value={values.memory_type} onChange={(e) => set("memory_type", e.target.value)}>
            {MEMORY_TYPES.map((t) => (
              <option key={t} value={t}>{t}</option>
            ))}
          </select>
        </div>
        <div className="detail-field">
          <label>{t("episodic.namespace")}</label>
          <input value={values.namespace} onChange={(e) => set("namespace", e.target.value)} />
        </div>
        <div className="detail-field">
          <label>{t("form.tags")}</label>
          <input value={values.tagsInput} onChange={(e) => set("tagsInput", e.target.value)} />
        </div>
        <div className="detail-field">
          <label>{t("detail.visibility")}</label>
          <select value={values.visibility} onChange={(e) => set("visibility", e.target.value)}>
            {VISIBILITIES.map((v) => (
              <option key={v} value={v}>{v}</option>
            ))}
          </select>
        </div>
        {isSkill && (
          <>
            <div className="detail-field">
              <label>{t("form.skillTrigger")}</label>
              <input value={values.skillTrigger} onChange={(e) => set("skillTrigger", e.target.value)} />
            </div>
            <div className="detail-field">
              <label>{t("form.skillSteps")}</label>
              <input value={values.skillStepsInput} onChange={(e) => set("skillStepsInput", e.target.value)} />
            </div>
            <div className="detail-field">
              <label>{t("form.skillVerification")}</label>
              <input value={values.skillVerification} onChange={(e) => set("skillVerification", e.target.value)} />
            </div>
          </>
        )}

        <div className="detail-actions">
          <button className="approve" onClick={() => onSubmit(values)} disabled={!values.content.trim()}>
            {isEdit ? t("form.save") : t("form.create")}
          </button>
          <button className="reject" onClick={onCancel}>{t("cancel")}</button>
        </div>
      </div>
    </div>
  );
}

export default App;
