import { useSyncExternalStore } from "react";

export type Locale = "en" | "zh";
export type Theme = "dark" | "light";

const LOCALE_KEY = "memvault.locale";
const THEME_KEY = "memvault.theme";

/* ------------------------------------------------------------------ */
/* Message dictionary — every key exists in both locales.              */
/* ------------------------------------------------------------------ */

type Entry = { en: string; zh: string };

const MESSAGES: Record<string, Entry> = {
  // Tabs
  "tab.memories": { en: "Memories ({count})", zh: "记忆 ({count})" },
  "tab.episodic": { en: "Episodic ({count})", zh: "情境 ({count})" },
  "tab.search": { en: "Search", zh: "搜索" },
  "tab.review": { en: "Review ({count})", zh: "评审 ({count})" },
  "tab.stats": { en: "Stats", zh: "统计" },
  "tab.system": { en: "System", zh: "系统" },
  "tab.data": { en: "Data", zh: "数据" },
  "tab.agents": { en: "Agents", zh: "智能体" },
  "tab.settings": { en: "Settings", zh: "设置" },

  // Header
  "header.extract": { en: "Extract from Text", zh: "从文本提取" },
  "header.newMemory": { en: "+ New Memory", zh: "+ 新建记忆" },

  // Memories toolbar
  "memories.allNamespaces": { en: "All namespaces", zh: "全部命名空间" },
  "pagination.prev": { en: "← Prev", zh: "← 上一页" },
  "pagination.next": { en: "Next →", zh: "下一页 →" },
  "pagination.page": { en: "Page {n}", zh: "第 {n} 页" },
  "memories.empty": {
    en: 'No memories stored yet. Use the CLI, MCP Server, or the "New Memory" button above.',
    zh: '暂无记忆。你可通过 CLI、MCP 服务或上方“新建记忆”按钮创建。',
  },
  "card.reviewed": { en: "Reviewed", zh: "已评审" },
  "card.steps": { en: "{n} steps", zh: "{n} 步" },

  // Episodic
  "episodic.title": { en: "Report Task Outcome", zh: "记录任务结果" },
  "episodic.hint": {
    en: "Record what an agent just did. Failures are reflected into lessons that get injected into similar future sessions.",
    zh: "记录智能体刚刚完成的工作。失败会转化为教训，并在未来相似的会话中注入。",
  },
  "episodic.task": { en: "Task", zh: "任务" },
  "episodic.taskPlaceholder": { en: "deploy the dashboard", zh: "例如：部署 dashboard" },
  "episodic.status": { en: "Status", zh: "状态" },
  "episodic.type": { en: "Type", zh: "类型" },
  "episodic.typePlaceholder": { en: "deploy", zh: "例如：部署" },
  "episodic.namespace": { en: "Namespace", zh: "命名空间" },
  "episodic.cause": { en: "Cause (drives lesson reflection)", zh: "原因（驱动教训反思）" },
  "episodic.causePlaceholder": { en: "missing env var", zh: "例如：缺少环境变量" },
  "episodic.record": { en: "Record Outcome", zh: "记录结果" },
  "episodic.recorded": { en: "Recorded {id}", zh: "已记录 {id}" },
  "episodic.lesson": { en: "Lesson ({source}): {lesson}", zh: "教训（来源：{source}）：{lesson}" },
  "episodic.listTitle": { en: "Episodes ({count})", zh: "情境 ({count})" },
  "episodic.filterAria": { en: "Filter by outcome status", zh: "按结果状态筛选" },
  "episodic.allOutcomes": { en: "All outcomes", zh: "全部结果" },
  "episodic.empty": { en: "No task outcomes recorded yet.", zh: "暂无任务结果记录。" },
  "episodic.colTime": { en: "Time", zh: "时间" },
  "episodic.colTask": { en: "Task", zh: "任务" },
  "episodic.colStatus": { en: "Status", zh: "状态" },
  "episodic.colType": { en: "Type", zh: "类型" },
  "episodic.colCause": { en: "Cause", zh: "原因" },
  "episodic.colLesson": { en: "Lesson", zh: "教训" },

  // Search
  "search.modeAria": { en: "Search mode", zh: "搜索模式" },
  "search.placeholder": { en: "Search memories...", zh: "搜索记忆…" },
  "search.expandRelations": { en: "Expand relations", zh: "展开关联" },
  "search.runAria": { en: "Run search", zh: "执行搜索" },
  "search.empty": { en: "No results found.", zh: "未找到结果。" },

  // Review
  "review.title": { en: "Pending Review ({count})", zh: "待评审 ({count})" },
  "review.empty": { en: "All memories have been reviewed.", zh: "所有记忆均已评审完成。" },
  "review.approve": { en: "Approve", zh: "通过" },
  "review.quickEdit": { en: "Quick Edit", zh: "快速编辑" },
  "review.reject": { en: "Reject", zh: "拒绝" },

  // Stats
  "stats.total": { en: "Total Memories", zh: "记忆总数" },
  "stats.must": { en: "MUST Rules", zh: "MUST 规则" },
  "stats.references": { en: "References", zh: "参考资料" },
  "stats.reviewed": { en: "Reviewed", zh: "已评审" },
  "stats.l3": { en: "L3 (Persona)", zh: "L3（人格）" },
  "stats.l2": { en: "L2 (Scenario)", zh: "L2（场景）" },
  "stats.l1": { en: "L1 (Atom)", zh: "L1（原子）" },
  "stats.skills": { en: "Skills", zh: "技能" },
  "stats.pipeline": { en: "Pipeline Actions", zh: "流水线操作" },
  "stats.promote": { en: "Run Promote (L1→L2→L3)", zh: "运行提升 (L1→L2→L3)" },
  "stats.decay": { en: "Run Decay", zh: "运行衰减" },
  "stats.dedup": { en: "Run Dedup", zh: "运行去重" },
  "stats.agents": { en: "Connected Agents", zh: "已连接智能体" },
  "stats.noAgents": { en: "No agents have written memories yet.", zh: "暂无智能体写入记忆。" },
  "stats.complianceTitle": { en: "Compliance (last {count} sessions)", zh: "合规（最近 {count} 个会话）" },
  "stats.complianceDisabled": { en: "Compliance tracking is not enabled on this database.", zh: "此数据库未启用合规追踪。" },
  "stats.sessions": { en: "Sessions", zh: "会话数" },
  "stats.overallRate": { en: "Overall Rate", zh: "总体合规率" },
  "stats.mustRate": { en: "MUST Rate", zh: "MUST 合规率" },
  "stats.colSession": { en: "Session", zh: "会话" },
  "stats.colAgent": { en: "Agent", zh: "智能体" },
  "stats.colInjected": { en: "Injected", zh: "已注入" },
  "stats.colMust": { en: "MUST ✓/✗", zh: "MUST ✓/✗" },
  "stats.colRef": { en: "REF ✓/✗", zh: "REF ✓/✗" },
  "stats.colRate": { en: "Rate", zh: "合规率" },
  "stats.sessionTitle": { en: "Session {id} — {agent}", zh: "会话 {id} — {agent}" },
  "stats.closeAria": { en: "Close session detail", zh: "关闭会话详情" },
  "stats.injected": { en: "Injected", zh: "已注入" },
  "stats.mustFollowed": { en: "MUST Followed", zh: "MUST 遵守" },
  "stats.mustViolated": { en: "MUST Violated", zh: "MUST 违反" },
  "stats.refFollowed": { en: "REF Followed", zh: "REF 遵守" },
  "stats.refViolated": { en: "REF Violated", zh: "REF 违反" },
  "stats.pending": { en: "Pending", zh: "待处理" },
  "stats.compliance": { en: "Compliance", zh: "合规率" },
  "stats.effectivenessTitle": { en: "Injection Effectiveness (auto-judged from record_outcome)", zh: "注入效果（由 record_outcome 自动判定）" },
  "stats.effectivenessDisabled": { en: "Effectiveness tracking is not enabled on this database.", zh: "此数据库未启用效果追踪。" },
  "stats.useful": { en: "Useful", zh: "有用" },
  "stats.neutral": { en: "Neutral", zh: "中性" },
  "stats.harmful": { en: "Harmful", zh: "有害" },
  "stats.insufficient": { en: "Insufficient Ctx", zh: "上下文不足" },
  "stats.unjudged": { en: "Unjudged", zh: "未判定" },
  "stats.usefulRate": { en: "Usefulness Rate", zh: "有用率" },
  "stats.harmfulRate": { en: "Harmful Rate", zh: "有害率" },
  "stats.coverage": { en: "Judged Coverage", zh: "已判定覆盖率" },

  // System
  "system.capabilities": { en: "Capabilities", zh: "能力" },
  "system.capabilitiesHint": { en: "What degrades without an embedding provider configured.", zh: "未配置 embedding 提供方时，以下能力会降级。" },
  "system.metrics": { en: "Metrics", zh: "指标" },
  "system.metricsEmpty": { en: "No memvault_* counters reported yet.", zh: "暂无 memvault_* 指标上报。" },
  "system.doctor": { en: "Doctor", zh: "诊断" },
  "system.doctorHint": {
    en: "Read-only hygiene scan (dangling pointers, stale/unarchived memories, live contradictions, near-duplicates, review backlog). Scans the whole store — run on demand, not automatically.",
    zh: "只读健康检查（悬空指针、过期/未归档记忆、活跃矛盾、近似重复、评审积压）。扫描整个存储 — 按需运行，不会自动执行。",
  },
  "system.doctorRun": { en: "Run Doctor", zh: "运行诊断" },
  "system.doctorScanning": { en: "Scanning…", zh: "扫描中…" },
  "system.doctorSummary": { en: "{count} memories scanned · {warnings} warning(s)", zh: "已扫描 {count} 条记忆 · {warnings} 项警告" },

  // Data
  "data.exportImport": { en: "Export / Import", zh: "导出 / 导入" },
  "data.exportHint": {
    en: "Export downloads a JSON file shaped exactly like what Import expects back — round trips through the same file.",
    zh: "导出会下载一个 JSON 文件，其结构与导入期待的文件完全一致 — 可经同一文件往返。",
  },
  "data.format": { en: "Format", zh: "格式" },
  "data.namespaceFilter": { en: "Namespace (optional filter)", zh: "命名空间（可选过滤）" },
  "data.namespacePlaceholder": { en: "all namespaces", zh: "全部命名空间" },
  "data.exporting": { en: "Exporting…", zh: "导出中…" },
  "data.export": { en: "Export", zh: "导出" },
  "data.importing": { en: "Importing…", zh: "导入中…" },
  "data.importFile": { en: "Import from file", zh: "从文件导入" },
  "data.imported": { en: "Imported {count}", zh: "已导入 {count}" },
  "data.importSkipped": { en: "{count} file(s) skipped: {list}", zh: "跳过 {count} 个文件：{list}" },
  "data.backup": { en: "Backup", zh: "备份" },
  "data.backupHint": { en: "Point-in-time SQLite snapshot, downloaded directly — nothing kept on the server.", zh: "SQLite 时点快照，直接下载 — 服务器不保留任何内容。" },
  "data.creating": { en: "Creating…", zh: "创建中…" },
  "data.createBackup": { en: "Create Backup", zh: "创建备份" },
  "data.importSkills": { en: "Import Skills from SOP", zh: "从 SOP 导入技能" },
  "data.importSkillsHint": {
    en: "Paste a Markdown SOP; each #/## heading becomes a skill (trigger:/verification: lines and list items become its metadata).",
    zh: "粘贴 Markdown SOP；每个 #/## 标题成为一个技能（trigger:/verification: 行与列表项构成其元数据）。",
  },
  "data.sopPlaceholder": {
    en: "# Deploy the dashboard\ntrigger: user asks to deploy\n1. Build the frontend\n2. Run the release script\nverification: check the health endpoint",
    zh: "# 部署 dashboard\ntrigger: user asks to deploy\n1. Build the frontend\n2. Run the release script\nverification: check the health endpoint",
  },
  "data.approveImmediately": { en: "Approve immediately (skip review inbox)", zh: "立即审批（跳过评审收件箱）" },
  "data.importSkillsBtn": { en: "Import Skills", zh: "导入技能" },
  "data.importedSkills": { en: "Imported {count} skill(s)", zh: "已导入 {count} 个技能" },
  "data.skippedNoSteps": { en: "{count} section(s) skipped (no steps)", zh: "跳过 {count} 个章节（无步骤）" },
  "data.skillLine": { en: "{title} ({count} steps)", zh: "{title}（{count} 步）" },
  "data.agentImport": { en: "Import from Other Agents", zh: "从其他智能体导入" },
  "data.agentImportHint": {
    en: "Reads memory files on this machine (Claude Code, Codex CLI, Hermes, Qoder, OpenClaw) — only useful when this server runs on the same machine as those agents. Imported candidates always land unreviewed in the Review inbox.",
    zh: "读取本机上的记忆文件（Claude Code、Codex CLI、Hermes、Qoder、OpenClaw）— 仅当服务器与这些智能体运行在同一台机器时才有用。导入的候选始终以未评审状态进入评审收件箱。",
  },
  "data.scanning": { en: "Scanning…", zh: "扫描中…" },
  "data.scanAgents": { en: "Scan for Agents", zh: "扫描智能体" },
  "data.notDetected": { en: "not detected on this machine", zh: "本机未检测到" },
  "data.preview": { en: "Preview", zh: "预览" },
  "data.namespaceOverride": { en: "Namespace override (optional)", zh: "命名空间覆盖（可选）" },
  "data.previewHint": { en: "{display}: {files} file(s) scanned, {candidates} candidate(s)", zh: "{display}：扫描了 {files} 个文件，{candidates} 个候选" },
  "data.noCandidates": { en: "No candidates parsed from this agent's files.", zh: "未能从该智能体的文件中解析出候选。" },
  "data.heuristicTitle": { en: "Best-effort guess against an unconfirmed source format — verify before approving", zh: "针对未确认源格式的最佳猜测 — 审批前请核实" },
  "data.heuristic": { en: "⚠ heuristic", zh: "⚠ 启发式" },
  "data.duplicateOf": { en: "duplicate of {id}", zh: "{id} 的重复项" },
  "data.importCandidates": { en: "Import {count} Candidate(s)", zh: "导入 {count} 个候选" },
  "data.importRunPrefix": { en: "Imported {count} · skipped {skipped} duplicate(s) — check the", zh: "已导入 {count} · 跳过 {skipped} 个重复项 — 请到" },
  "data.importRunSuffix": { en: "to approve them.", zh: "审批。" },
  "data.reviewTab": { en: "Review tab", zh: "评审页" },

  // Agents
  "agents.title": { en: "Agent Profiles", zh: "智能体配置" },
  "agents.hint": {
    en: "Read-only view of the agent registry (agents.yaml or built-in defaults) — injection rules per agent. Editing isn't supported from the dashboard; edit the YAML file and restart the server.",
    zh: "智能体注册表（agents.yaml 或内置默认）的只读视图 — 每个智能体的注入规则。dashboard 不支持编辑；请修改 YAML 文件并重启服务器。",
  },
  "agents.colId": { en: "ID", zh: "ID" },
  "agents.colDesc": { en: "Description", zh: "描述" },
  "agents.colMax": { en: "Max memories", zh: "最大记忆数" },
  "agents.colBudget": { en: "Token budget", zh: "Token 预算" },
  "agents.colPriority": { en: "Priority order", zh: "优先级顺序" },
  "agents.colNamespace": { en: "Namespace filter", zh: "命名空间过滤" },
  "agents.colExcluded": { en: "Excluded types", zh: "排除类型" },
  "agents.colApiKey": { en: "API key", zh: "API 密钥" },
  "agents.loadingPreview": { en: "Loading injection preview…", zh: "正在加载注入预览…" },
  "agents.whatReceives": { en: "What agent {profile} receives", zh: "智能体 {profile} 将接收的内容" },
  "agents.sessionSuffix": { en: " — session {id}", zh: " — 会话 {id}" },
  "agents.previewHint": {
    en: "Same pipeline as MCP session_start — MUST/REF instructions, semantic candidates, and every drop reason. Format: {format}.",
    zh: "与 MCP session_start 同一管线 — MUST/REF 指令、语义候选以及每条丢弃原因。格式：{format}。",
  },
  "agents.drops": { en: "Explainable drops ({count}):", zh: "可解释的丢弃（{count}）：" },
  "agents.nothing": { en: "Nothing to inject for this agent.", zh: "此智能体无可注入内容。" },
  "agents.namespaces": { en: "Namespaces", zh: "命名空间" },
  "agents.namespacesHint": {
    en: "Namespaces aren't a first-class entity — this aggregates the memory counts per namespace already reachable from the Memories tab's filter.",
    zh: "命名空间并非一等实体 — 此处汇总每个命名空间的记忆数量，与记忆页筛选器中的一致。",
  },
  "agents.noNamespaces": { en: "No namespaces yet.", zh: "暂无命名空间。" },

  // Settings
  "settings.title": { en: "Settings", zh: "设置" },
  "settings.backend": { en: "Backend connection", zh: "后端连接" },
  "settings.checking": { en: "Checking…", zh: "检查中…" },
  "settings.connected": { en: "● Connected", zh: "● 已连接" },
  "settings.unreachable": { en: "● Unreachable — is memvault-mcp running with --transport http?", zh: "● 无法连接 — memvault-mcp 是否已通过 --transport http 运行？" },
  "settings.apiKey": { en: "API key", zh: "API 密钥" },
  "settings.apiKeyHint": {
    en: "Sent as X-MemVault-Api-Key for admin-protected REST routes (required when the server registers an admin key in agents.yaml). Leave empty if no key is configured.",
    zh: "作为 X-MemVault-Api-Key 发送给受管理员保护的 REST 路由（当服务器在 agents.yaml 中注册了管理密钥时需要）。未配置密钥时留空即可。",
  },
  "settings.save": { en: "Save", zh: "保存" },
  "settings.saved": { en: "Saved.", zh: "已保存。" },
  "settings.agentId": { en: "Agent ID", zh: "智能体 ID" },
  "settings.agentIdHint": {
    en: "Sent as X-MemVault-Agent-Id (default admin), and used to label memories created here as dashboard. Override in the app config only if your server's registry uses a different admin agent.",
    zh: "作为 X-MemVault-Agent-Id 发送（默认为 admin），用于把在此创建的记忆标记为 dashboard 来源。仅当服务器注册表使用不同的管理智能体时，才在应用配置中覆盖。",
  },
  "settings.access": { en: "Access", zh: "访问" },
  "settings.accessHint": {
    en: "This Dashboard fetches the REST API served by memvault-mcp --db ~/.memvault/data.db --transport http --serve-web dist — the same protocol the Obsidian client uses. Run it on the machine that owns the SQLite file.",
    zh: "本 Dashboard 访问由 memvault-mcp --db ~/.memvault/data.db --transport http --serve-web dist 提供的 REST API — 与 Obsidian 客户端使用相同的协议。请在持有 SQLite 文件的机器上运行。",
  },

  // Detail
  "detail.close": { en: "Close", zh: "关闭" },
  "detail.title": { en: "Memory Detail", zh: "记忆详情" },
  "detail.relations": { en: "Relations", zh: "关联" },
  "detail.evidence": { en: "Evidence ({count}) · supports {supports} · contradicts {contradicts}", zh: "证据（{count}）· 支持 {supports} · 反驳 {contradicts}" },
  "detail.moreTraces": { en: "+ {count} more traces", zh: "另有 {count} 条追踪" },
  "detail.id": { en: "ID", zh: "ID" },
  "detail.priority": { en: "Priority", zh: "优先级" },
  "detail.layer": { en: "Layer", zh: "层" },
  "detail.type": { en: "Type", zh: "类型" },
  "detail.content": { en: "Content", zh: "内容" },
  "detail.instruction": { en: "Instruction", zh: "指令" },
  "detail.tags": { en: "Tags", zh: "标签" },
  "detail.none": { en: "none", zh: "无" },
  "detail.agent": { en: "Agent", zh: "智能体" },
  "detail.namespace": { en: "Namespace", zh: "命名空间" },
  "detail.visibility": { en: "Visibility", zh: "可见性" },
  "detail.confidence": { en: "Confidence", zh: "置信度" },
  "detail.status": { en: "Status", zh: "状态" },
  "detail.reviewed": { en: "Reviewed", zh: "已评审" },
  "detail.pending": { en: "Pending Review", zh: "待评审" },
  "detail.supersededBy": { en: " — superseded by {id}", zh: " — 已被 {id} 取代" },
  "detail.created": { en: "Created", zh: "创建时间" },
  "detail.skill": { en: "Skill", zh: "技能" },
  "detail.trigger": { en: "Trigger:", zh: "触发词：" },
  "detail.verify": { en: "Verify:", zh: "校验：" },
  "detail.version": { en: "Version:", zh: "版本：" },
  "detail.friction": { en: "Friction Evidence", zh: "摩擦证据" },
  "detail.edit": { en: "Edit", zh: "编辑" },
  "detail.markRead": { en: "Mark as Read", zh: "标记已读" },
  "detail.markReadTitle": { en: "Bump access_count — decay weighs access recency", zh: "增加访问计数 — 衰减会参考访问时效" },
  "detail.supersede": { en: "Supersede", zh: "取代" },
  "detail.history": { en: "History", zh: "历史" },
  "detail.delete": { en: "Delete", zh: "删除" },

  // History modal
  "history.title": { en: "History — {id}", zh: "历史 — {id}" },
  "history.loading": { en: "Loading…", zh: "加载中…" },
  "history.empty": { en: "No edit history recorded for this memory yet.", zh: "该记忆暂无编辑历史。" },
  "history.restore": { en: "Restore", zh: "恢复" },
  "confirm.restore": { en: "Restore this version? The current content will be replaced (and itself saved to history).", zh: "恢复此版本？当前内容将被替换（其自身会先保存到历史）。" },

  // Quick edit
  "quickEdit.title": { en: "Quick Edit", zh: "快速编辑" },
  "quickEdit.hint": { en: "Fixes the wording and approves in one step — the memory leaves the review inbox immediately once saved.", zh: "一步完成措辞修正并通过 — 保存后该记忆即刻离开评审收件箱。" },
  "quickEdit.content": { en: "Content", zh: "内容" },
  "quickEdit.save": { en: "Approve + Save", zh: "通过并保存" },
  "cancel": { en: "Cancel", zh: "取消" },

  // Reject modal
  "reject.title": { en: "Reject Candidate", zh: "拒绝候选" },
  "reject.hint": { en: "Reject and remove this candidate memory? This can't be undone.", zh: "拒绝并删除该候选记忆？此操作不可撤销。" },
  "confirm.delete": { en: "Delete this memory?", zh: "删除这条记忆？" },

  // Supersede
  "supersede.title": { en: "Supersede Memory", zh: "取代记忆" },
  "supersede.hint": { en: "The memory being replaced is archived, not deleted. Enter the ID of the memory that replaces it.", zh: "被取代的记忆会归档而不是删除。请输入取代它的记忆 ID。" },
  "supersede.id": { en: "Replacement memory ID", zh: "替代记忆 ID" },
  "supersede.btn": { en: "Supersede", zh: "取代" },

  // Memory form
  "form.editTitle": { en: "Edit Memory", zh: "编辑记忆" },
  "form.newTitle": { en: "New Memory", zh: "新建记忆" },
  "form.content": { en: "Content", zh: "内容" },
  "form.instruction": { en: "Instruction (optional)", zh: "指令（可选）" },
  "form.priority": { en: "Priority", zh: "优先级" },
  "form.type": { en: "Type", zh: "类型" },
  "form.namespace": { en: "Namespace", zh: "命名空间" },
  "form.tags": { en: "Tags (comma-separated)", zh: "标签（逗号分隔）" },
  "form.visibility": { en: "Visibility", zh: "可见性" },
  "form.skillTrigger": { en: "Skill trigger", zh: "技能触发词" },
  "form.skillSteps": { en: "Skill steps (comma-separated)", zh: "技能步骤（逗号分隔）" },
  "form.skillVerification": { en: "Skill verification", zh: "技能校验" },
  "form.save": { en: "Save Changes", zh: "保存修改" },
  "form.create": { en: "Create", zh: "创建" },

  // Extract
  "extract.title": { en: "Extract from Text", zh: "从文本提取" },
  "extract.hint": {
    en: "Paste conversation text or notes; MemVault detects candidate preferences, facts, and skills. Review the extracted list below before saving.",
    zh: "粘贴对话文本或笔记；MemVault 会识别候选偏好、事实与技能。保存前请先核对下方的提取列表。",
  },
  "extract.text": { en: "Text", zh: "文本" },
  "extract.placeholder": { en: "I always prefer dark mode. The deploy script lives in scripts/deploy.sh...", zh: "我始终偏好暗色模式。部署脚本位于 scripts/deploy.sh..." },
  "extract.mode": { en: "Mode", zh: "模式" },
  "extract.modeRule": { en: "rule (keyword pattern matching)", zh: "rule（关键词模式匹配）" },
  "extract.modeLlm": { en: "llm (semantic, requires provider configured)", zh: "llm（语义，需配置提供方）" },
  "extract.namespace": { en: "Namespace for saved memories", zh: "保存记忆的命名空间" },
  "extract.running": { en: "Extracting…", zh: "提取中…" },
  "extract.run": { en: "Run Extraction", zh: "运行提取" },
  "extract.coverage": { en: "{input} line(s) in · {extracted} extracted · {noSignal} no signal · {empty} empty", zh: "输入 {input} 行 · 提取 {extracted} 行 · 无信号 {noSignal} 行 · 空行 {empty} 行" },
  "extract.empty": { en: "No candidates extracted from this text.", zh: "未从此文本提取到候选。" },
  "extract.saveSelected": { en: "Save Selected ({count})", zh: "保存选中 ({count})" },

  // Alerts
  "alert.promote": { en: "Promote: {l2} → L2, {l3} → L3", zh: "提升：{l2} → L2，{l3} → L3" },
  "alert.decay": { en: "Decay: {updated} updated, {archived} archived", zh: "衰减：更新 {updated} 条，归档 {archived} 条" },
  "alert.dedup": { en: "Dedup: {unique} unique, {duplicates} duplicates found", zh: "去重：唯一 {unique} 条，发现重复 {duplicates} 条" },
  "alert.saveFailed": { en: "Save failed: {error}", zh: "保存失败：{error}" },
  "alert.exportFailed": { en: "Export failed: {error}", zh: "导出失败：{error}" },
  "alert.backupFailed": { en: "Backup failed: {error}", zh: "备份失败：{error}" },
  "alert.restoreFailed": { en: "Restore failed: {error}", zh: "恢复失败：{error}" },

  // Header toggles
  "toggle.light": { en: "Switch to light theme", zh: "切换到白天模式" },
  "toggle.dark": { en: "Switch to dark theme", zh: "切换到夜间模式" },
  "toggle.lang": { en: "Switch language (中文/English)", zh: "切换语言（中文/English）" },
};

const en: Record<string, string> = {};
const zh: Record<string, string> = {};
for (const [key, entry] of Object.entries(MESSAGES)) {
  en[key] = entry.en;
  zh[key] = entry.zh;
}

export function translate(locale: Locale, key: string, vars?: Record<string, string | number>): string {
  const dict = locale === "zh" ? zh : en;
  let out = dict[key] ?? en[key] ?? key;
  if (vars) {
    for (const [k, v] of Object.entries(vars)) {
      out = out.split("{" + k + "}").join(String(v));
    }
  }
  return out;
}

/* ------------------------------------------------------------------ */
/* Tiny reactive store so every component re-renders on switch.        */
/* ------------------------------------------------------------------ */

function detectLocale(): Locale {
  try {
    const saved = localStorage.getItem(LOCALE_KEY);
    if (saved === "en" || saved === "zh") return saved;
  } catch {
    /* ignore */
  }
  try {
    return (navigator.language || "en").toLowerCase().startsWith("zh") ? "zh" : "en";
  } catch {
    return "en";
  }
}

function detectTheme(): Theme {
  try {
    const saved = localStorage.getItem(THEME_KEY);
    if (saved === "dark" || saved === "light") return saved;
  } catch {
    /* ignore */
  }
  return "dark";
}

interface State {
  locale: Locale;
  theme: Theme;
  version: number;
}

let state: State = { locale: detectLocale(), theme: detectTheme(), version: 0 };
const listeners = new Set<() => void>();

function setState(patch: Partial<State>): void {  state = { ...state, ...patch, version: state.version + 1 };
  listeners.forEach((l) => l());
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

function getSnapshot(): number {
  return state.version;
}

export function useI18n() {
  useSyncExternalStore(subscribe, getSnapshot);
  const setLocale = (locale: Locale) => {
    setState({ locale });
    try {
      localStorage.setItem(LOCALE_KEY, locale);
    } catch {
      /* ignore */
    }
    document.documentElement.lang = locale === "zh" ? "zh-CN" : "en";
    const meta = document.querySelector('meta[name="theme-color"]');
    if (meta) meta.setAttribute("content", state.theme === "dark" ? "#0b1120" : "#f4f6fb");
  };
  const setTheme = (theme: Theme) => {
    setState({ theme });
    try {
      localStorage.setItem(THEME_KEY, theme);
    } catch {
      /* ignore */
    }
    document.documentElement.dataset.theme = theme;
    const meta = document.querySelector('meta[name="theme-color"]');
    if (meta) meta.setAttribute("content", theme === "dark" ? "#0b1120" : "#f4f6fb");
  };
  return {
    locale: state.locale,
    theme: state.theme,
    setLocale,
    setTheme,
    t: (key: string, vars?: Record<string, string | number>) => translate(state.locale, key, vars),
  };
}

/** Test helper: re-read persisted prefs without clearing them. */
export function __reloadI18n(): void {
  state = { locale: detectLocale(), theme: detectTheme(), version: state.version + 1 };
  listeners.forEach((l) => l());
}

/** Test helper: reset to environment defaults and clear persisted prefs. */
export function __resetI18n(): void {
  try {
    localStorage.removeItem(LOCALE_KEY);
    localStorage.removeItem(THEME_KEY);
  } catch {
    /* ignore */
  }
  state = { locale: detectLocale(), theme: detectTheme(), version: state.version + 1 };
  listeners.forEach((l) => l());
}
