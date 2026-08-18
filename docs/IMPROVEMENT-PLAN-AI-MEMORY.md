# 借鉴 akitaonrails/ai-memory 的改进计划

> **来源**：对比分析 `docs/ai-memory`（akitaonrails/ai-memory v1.28.0，Rust，MIT）与 MemVault 自身架构后整理。
> **定位**：不是照搬对方的架构方向（它是「markdown wiki 为 truth，SQLite 为索引」，MemVault 是「SQLite 为 truth，配置文件为派生视图」），而是在 MemVault 现有路线上吸收其中可独立落地的设计点。
> **日期**：2026-08-18

---

## 背景

ai-memory 和 MemVault 解决同一类问题（跨 Agent/跨 CLI 的持久记忆共享），但架构方向相反：

| | ai-memory | MemVault |
|---|---|---|
| Source of truth | markdown wiki（git 版本化） | SQLite |
| 检索 | FTS5 + entity RRF + graph-neighbor RRF + 可选向量 RRF | BM25 + 向量 + RRF + 同义词扩展 |
| 对外产物 | 只读 web UI + MCP | CLAUDE.md/AGENTS.md（sync 生成）+ MCP + Dashboard/VSCode/Obsidian |
| 会话续接 | 编译（session 结束时把观察写成一篇 wiki 页） | 检索（实时召回相关记忆条目） |

MemVault 已经领先的点（本计划不涉及）：MCP Proxy 透明注入、Compliance tracking（inject_session_id + follow-through rate）、更广的客户端面（Tauri/VSCode/Obsidian）。

以下是值得吸收的 7 个点，按优先级排列。

---

## P0：优先级最高，收益/成本比最好（✅ 已落地）

### 1. 记忆的时间旅行能力（Git 化 wiki 快照 或 SQLite 版本表）✅ 已完成

**问题**：MemVault 单文件 SQLite 没有等价的 `checkpoints` / `restore-page` / `git log`，误删或错误 dedup/decay 后无法回滚到某个历史状态。

**方案**（两种可选，不要求照抄 git-wiki）：
- **方案 A（轻量）**：给 `memories` 表加 `history` 影子表（触发器或应用层写入），每次 UPDATE/DELETE 前记录旧值 + 时间戳，CLI 新增 `memvault checkpoints` / `memvault restore <id> [--at <ts>]`。
- **方案 B（对齐 ai-memory）**：`sync` 导出的 markdown 目录本身纳入 git（`memvault sync --git-commit`），天然获得 diff/log/revert，SQLite 仍是唯一写入源，git 只是可读快照层。

**推荐**：先做方案 A（不引入 git2 依赖，改动集中在 `memvault-core::storage`），方案 B 作为 `sync` 模块的可选增强。

**涉及模块**：`memvault-core/storage`，`memvault-cli`。

**落地记录**：方案 A，与计划一致。`sqlite.rs` 新增 `memory_history` 表（整行 JSON 快照，不逐列镜像，避免未来 `memories` 加列时要同步改历史表 schema），`update`/`delete` 改为事务内先快照旧行再写库；新增 `SqliteStore::list_checkpoints` / `restore_checkpoint`（按当前行是否存在自动选 `update` 还是 `save` 重建，"撤销的撤销"天然成立，无需特殊处理）。CLI 新增 `memvault checkpoints [--memory-id] [--limit]` / `memvault restore --history-id <id>`。未做方案 B。

---

### 2. Zero-LLM 模式的显式承诺 ✅ 已完成

**问题**：MemVault 默认已经是本地嵌入优先（`embedding.rs::build_embedder_from_env()` 未配置任何环境变量时走内嵌 fastembed 模型 `native`，完全离线；Ollama 只是显式设置 `MEMVAULT_EMBEDDING_PROVIDER=ollama` 或 `auto` 探测到本地服务时的可选项，不是默认路径），但 README/代码里没有明确"LLM 完全不可用时哪些功能仍然 100% 可用"的边界，用户不知道降级到什么程度。

**方案**：
- 审查 `memvault-core::hybrid`（RRF 融合）/ `query_expand`（同义词扩展）/ `extractor`（规则抽取，本身已 100% 不依赖 LLM），标注每个功能对 LLM/embedding 的依赖等级：`required` / `enhanced-by` / `independent`。
- CLI 新增 `memvault status` 子命令（当前 15 个子命令里没有这个，需要新建，不是扩展现有命令），输出一行 `LLM: unavailable → degraded to BM25-only search + rule-based extraction`（对齐 ai-memory 的 `status` 被动健康检查思路）。
- README 补一张"零 LLM 模式能力矩阵"表。

**涉及模块**：`memvault-core/hybrid`、`memvault-core/query_expand`、`memvault-core/extractor`、`memvault-cli`（新增 `status` 子命令）。

**落地记录**：调研确认 `hybrid`/`query_expand`/`extractor` 对 embedder 缺失已经优雅降级，不需要改代码；因此没有在这三个文件里逐个标注依赖等级，而是新建 `memvault-core/capabilities.rs`（`capability_report()`）集中输出同等信息，`memvault-cli` 复用已有的 `build_embedder_from_env()` 传给它。顺带修了一个真实缺口：CLI 的 `Dedup` 命令之前硬编码 `Deduplicator::new(store, None)`，从未接线 embedder，配置了 `OPENAI_API_KEY` 也用不上向量辅助去重——现在改成调用 `build_embedder_from_env()`，与 `memvault-mcp`/`memvault-proxy` 的行为对齐。README 的"零 LLM 能力矩阵"表未补，`memvault status` 的输出已经覆盖了这个信息，暂无额外必要性。

---

## P1：中等优先级，需要设计但收益明确

> #4 已完成。#3 改为**远期规划**：schema 迁移成本在这批里最高，只有出现明确的 monorepo/多客户/多项目隔离需求时才启动，不主动排期。

### 3. 多项目/多工作区隔离的 marker 文件路由 🔭 远期规划（暂不排期）

**问题**：MemVault 目前记忆是全局单库，没有按项目/工作区隔离的机制。对 monorepo、多客户咨询、work/personal 分离场景不友好。

**方案**：
- 新增 `.memvault.toml` marker 文件（对齐 ai-memory 的 `.ai-memory.toml`），字段：`workspace_id`、`project_id`（可选覆盖，默认从 git root 推导）。
- `memvault-core` 存储层加 `workspace_id/project_id` 列，检索默认限定当前 project，`_global` scope（见 #6）跨项目可见。
- 路由规则：CLI 命令从 `$cwd` 向上找最近的 marker 文件或 git root；MCP hook 默认 `basename($cwd)`，可通过 marker 文件覆盖。

**涉及模块**：`memvault-core/storage`（schema 迁移）、`memvault-mcp`（路由逻辑）、`memvault-cli`。

**风险**：schema 迁移需要兼容现有单库用户的数据，需提供 `memvault migrate --add-workspace` 一次性迁移命令。

**现状**：远期规划，暂不启动。触发条件：出现真实的 monorepo / 多客户咨询 / work-personal 分离场景，需要按项目隔离记忆时再排期。

---

### 4. 权威分层召回（bounded ranking adjustment）✅ 已完成

**问题**：MemVault 现在 MUST 是二元强制注入（永不过滤/截断），REFERENCE/NORMAL 走普通检索排序。中间没有"这条记忆更权威但不是 MUST"的层级，容易出现 episodic 记忆排到 rules 前面。

**方案**：
- 在现有 RRF 融合分数之后加一个 bounded adjustment 步骤：对打了 `decision` / `procedure` / `gotcha` 标签（或 MemoryLayer 中 L2/L3）的条目给一个有界加权（而非硬过滤），episodic/session 类记忆保持原分数。
- 不改变 MUST 的二元语义，只是给 REFERENCE/NORMAL 之间加一层可调权重，复用现有 `rerank` 模块。

**涉及模块**：`memvault-core/rerank`。

**落地记录**：`MultiSignalReranker` 新增第 6 个信号 `authority_weight`（默认 0.15，和其它权重一样参与 `total_weight` 归一化，不是单独的硬过滤开关）。`compute_authority_score` 按 `layer`（L3=0.8、L2=0.5、L0/L1=0）和 tag（`decision`/`procedure`/`gotcha`，大小写不敏感）取 max，二者不叠加。MUST 的绝对优先完全没动——`sqlite.rs`/`hybrid.rs`/`router.rs` 里已有的 MUST-first 排序比较器和固定 1.0 分逻辑都是独立于这个信号的。只改了 `rerank.rs` 一个文件。新增 3 个测试验证：layer 加权生效、tag 加权生效、"软加权不是硬过滤"（高 overlap/recency 的新鲜记忆仍能压过 stale 的 decision 标签记忆）。

**后续补做**：`memvault-mcp` 的 `search_memory` 跳过 `rerank` 的 gap 已单独修复——`MemVaultMcp` 加了 `reranker: MultiSignalReranker` 字段（在 `new()` 内部用 `RerankConfig::default()` 初始化，没改构造函数签名，`build_server` 等测试辅助函数不用跟着改），`search_memory` 在 merge 结果后、格式化输出前插入 `self.reranker.rerank(...)` 调用。新增 `test_tool_search_reranks_by_authority_tier` 验证两条其它信号完全打平的记忆里，打了 `decision` 标签的会排到前面。现在 MCP 直接检索路径和 `MemoryRouter::session_start` 一样能吃到全部 rerank 信号（overlap/recency/priority/access/authority）。

---

## P2：值得做但可以排后面

> 以下三项全部改为**远期规划**：P0+P1-4 落地后先观察实际使用一段时间，发现具体痛点再回来挑着做，不主动排期。

### 5. Entity-assisted 召回 🔭 远期规划（暂不排期）

**问题**：MemVault 已有同义词扩展，但没有从记忆内容中抽取"实体"（人名、项目名、专有名词）做精确匹配这一路检索信号，查询用词和记忆原文不一致时容易漏检。

**方案**：
- `memvault-core/extractor` 增加实体抽取（复用现有 `Extractor` 的规则抽取能力，不引入新 LLM 依赖 —— 优先用规则/词典，LLM 可用时再增强），写入记忆的 `entities` 字段（JSON array，非 frontmatter，因为 MemVault 是 SQLite-first）。
- 检索时加一路 exact/prefix 实体匹配，进 RRF 融合流（`memvault-core/hybrid`）。

**涉及模块**：`memvault-core/extractor`、`memvault-core/hybrid`。

**现状**：远期规划。同义词扩展已经覆盖了大部分查询用词不一致的场景，性价比不如其它几项，暂不启动。

---

### 6. 全局偏好 scope 显式化 🔭 远期规划（暂不排期）

**问题**：MemVault 的 MUST 级别是"必须遵守"，但没有区分"全项目通用的标准偏好"（如代码风格、工具链选择）和"单项目规则"。用户换项目时，全局偏好需要重新设置。

**方案**：
- 复用 #3 的 `workspace_id/project_id` schema，新增保留值 `project_id = "_global"`。
- `memvault save --scope global` 写入 `_global`；默认查询自动 UNION 当前项目 + `_global` 结果（对齐 ai-memory 的 `global_scope_hits`）。
- 依赖 #3 落地后再做，工作量小。

**涉及模块**：`memvault-core/storage`、`memvault-cli save`。

**现状**：远期规划，依赖 #3（项目隔离）先落地，#3 本身还没排期，所以这项自然也没有。

---

### 7. 记忆自省循环（curator / auto-improve） 🔭 远期规划（暂不排期）

**问题**：MemVault 有 dedup/decay，但没有"定期检查记忆库内部矛盾并生成待审核提案"这一层——比如两条 MUST 记忆互相冲突时，现在无法自动发现。

**方案**：
- 新增 `memvault lint` 命令：扫描全库，用规则（同 scope 下语义相近但内容矛盾的 MUST 记忆）+ 可选 LLM 辅助判断，输出冲突列表。
- `memvault curator --propose`：对过期/低置信度记忆生成"建议归档/合并"提案，写入 `pending_actions` 表，人工用 `memvault curator --approve <id>` 确认后才执行（不自动生效，避免误删）。

**涉及模块**：`memvault-core` 下新增 `curator` 模块（跟 `dedup.rs`/`decay.rs` 平级的顶层模块，MemVault 没有 `pipeline` 这层目录）；CLI 新增两个命令。

**风险**：这是本计划里最大的一块，建议排在 P0/P1 落地后再启动，且首版只做"发现+提案"，不做自动执行。

**现状**：远期规划里工作量最大、最容易做成半成品的一项，排最后。

---

## 落地状态

P0（时间旅行、Zero-LLM 承诺）和 P1-4（权威分层召回，含 MCP rerank gap 补丁）已完成，见各小节"落地记录"。

**剩下的 #3、#5、#6、#7 全部标记为远期规划，当前不排期**：先用一段时间观察 P0+P1-4 的实际效果，等出现具体痛点（尤其是 #3 需要真实的多项目隔离需求）再回来挑着做。若未来重启，原定依赖关系依然成立：#3 → #6（全局 scope 依赖项目隔离 schema），#7 排最后（工作量最大、最容易半成品）。

---

## 不建议做的事

- **不要**把 SQLite-as-truth 换成 markdown-wiki-as-truth——这是架构方向级别的分叉，MemVault 现有的 Dashboard/VSCode/Obsidian/Proxy 都建立在 SQLite 实时查询之上，换方向等于重写。
- **不要**为了对齐 ai-memory 的跨 harness session resume（`ai-memory run`）而去接管其他 CLI 的原生会话状态——那是它的核心壁垒功能，实现成本高且和 MemVault "轻量共享记忆层"的定位不符，除非用户明确要求。
