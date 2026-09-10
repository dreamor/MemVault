# Trace 摄入计划 — 让 Agent 会话日志成为记忆的自动来源

> **版本**：v0.1（草案）
> **日期**：2026-09-09
> **灵感来源**：Hugging Face funes（[blog/funes](https://huggingface.co/blog/funes)、[github.com/huggingface/funes](https://github.com/huggingface/funes)）
> **一句话**：MemVault 目前靠 Agent「自觉保存」记忆；本文把 funes 的洞见落到本架构上——**自动摄入 session trace 作为 L0，让现有 promote 管线把原始会话蒸馏成策展记忆**，并让每条策展记忆可回溯到产生它的那次会话。

---

## 目录

1. [背景与动机](#1-背景与动机)
2. [现状盘点：基座与缺口](#2-现状盘点基座与缺口)
3. [目标与非目标](#3-目标与非目标)
4. [概念模型：把 L0 定义为原始证据](#4-概念模型把-l0-定义为原始证据)
5. [方案设计](#5-方案设计)
6. [涉及模块与 schema 改动](#6-涉及模块与-schema-改动)
7. [分阶段实施](#7-分阶段实施)
8. [风险与对策](#8-风险与对策)
9. [验收标准与指标](#9-验收标准与指标)

---

## 1. 背景与动机

### 1.1 funes 说了什么

funes 是 Hugging Face 开源的 Agent 持久记忆层，核心观点：

- **Agent 会话日志（trace）就是记忆的原始记录**——搜索、试错、换方向的过程里留下了「为什么这样做」的密集证据。诊断「Agent 每次都是陌生人」是对的，但 trace 只是存档：要真正可用，需要**索引、检索、排序、精确出处**。
- **写入时不蒸馏，检索时带证据**：recall 返回原始文本（不是摘要）+（agent、时间戳、session、turn）出处，并附 `get` 展开完整上下文。
- **成本论证**：handoff-vs-recall benchmark 上，recall 比手写交接便宜 4–8 倍；compaction 会「压平」关键发现导致任务失败。
- 实现上：本地 embedding + BM25 混合检索 → cross-encoder 重排 → recency 重加权；增量索引；记忆即（Lance/HF）数据集，跨机器、跨 Agent、跨团队。

### 1.2 为什么对 MemVault 是启发而非重复

MemVault 与 funes 处在记忆光谱的两端：

| | funes | MemVault |
|---|---|---|
| 记忆观 | 原始证据，写入时不提炼 | 分层蒸馏（L0 raw → L3 persona） |
| 写入方式 | **自动**（hook 索引每个 turn） | **显式**（Agent 主动 `save_memory`） |
| 出处 | 每条命中带 session/turn | `session_id`/`source_agent`/`hit_sources` 已存在但未贯通分层 |
| 生命周期 | append-only，不整理不遗忘 | decay / dedup / promote / compliance |

**缺口一句话**：MemVault 已有「蒸馏后半段」（promote、decay、注入、合规追踪——funes 完全没有），但缺「自动摄入前半段」。会话日志目前只能靠插件 hook 手动转发给提取器，没有一条「发现 → 解析 → 增量提取 → 落 L0 → 复用 promote」的自动管线。把 funes 的两条原则——**自动摄入原始证据**与**出处可回溯**——接进现有分层架构，就是本计划。

---

## 2. 现状盘点：基座与缺口

### 2.1 已有基座（本计划复用的组件）

| 组件 | 位置 | 能力 | 复用方式 |
|---|---|---|---|
| `transcript.rs` | `memvault-core/src/transcript.rs` | Claude Code JSONL 会话 → `role: text` turn 文本；容忍解析、200KB 上限、保尾 | 作为逐 host 解析器的公共入口，扩展其他 host |
| `llm_extractor` | `memvault-core/src/llm_extractor.rs` | 从多 turn 上下文提取结构化记忆；`reflect_lesson`（从失败任务蒸馏一条教训，best-effort） | L1 提取的后端之一 |
| `agent_import/` | `memvault-core/src/agent_import/` | 冷启动导入框架：`AgentMemorySource` trait + claude/codex/hermes/openclaw/qoder 适配器；产物进 review inbox、Reference 层 | **模式模板**（每 host 一个 source），但对象是记忆文件而非会话日志，需平行扩展 |
| 分层与生命周期 | `models.rs` + promote/decay/dedup | L0–L3；`Must→L3, Reference→L2, Background→L1` | L0 语义需要重新明确（见 §4） |
| 出处字段 | `models.rs` | `session_id`、`source_agent`、`hit_sources`（召回路径）；Episode 与任务 1:1；relation 带 `source_memory_id` | 现有种子，需贯通到分层蒸馏产物 |
| 敏感内容防护 | 存储层（sensitive guard） | 写入时拦截 | 摄入管线直接受益，无需新做 |

### 2.2 明确缺口

1. **无自动、增量、按 host 的 trace 摄入通道**：trace 只在插件 hook 转发时才被消费；无「发现新会话 → 水位线去重 → 提取 → 落库」管线。
2. **L0 语义模糊**：目前 L0 混用「superseded/归档」与「原始候选」。funes 的 turn 块证据可以给 L0 一个清晰身份（见 §4）。
3. **provenance 不贯通分层**：L1/L2/L3 蒸馏产物没有指向「产生它的那段原始会话」的链接；`report_compliance` 争议一条 MUST 规则时无法拿出原始依据。
4. **无证据展开工具**：有 `hit_sources` 记录「怎么命中的」，但没有 funes `get` 那样的「从记忆展开到原始 episode/turn」的工具。
5. **查询侧无 recency 重加权与邻近扩展**：decay 是写入侧的；funes 提示查询时还应按新旧加权，并对命中 chunk 附相邻上下文。

---

## 3. 目标与非目标

### 3.1 目标

- **G1（摄入）**：自动发现并增量摄入各 host 的 session trace，归一化为统一 turn 形状后落为 L0 记忆，不要求 Agent 主动保存。
- **G2（蒸馏）**：复用现有 promote 管线把 L0 蒸馏为 L1/L2/L3，低置信候选进现有 review inbox（与冷启动导入同一审核流）。
- **G3（溯源）**：每条 L1+ 记忆可回溯到产生它的 L0 证据（session/turn/agent），并提供 MCP 工具展开原始上下文。
- **G4（成本）**：以可测指标证明「自动召回既有记忆」比「重读会话/手写交接」更省 token（参考 funes benchmark 思路，指标见 §9）。

### 3.2 非目标（本期不做，另行评估）

- 不做本地 embedding / cross-encoder 重排运行时（当前检索栈不变；仅记录为后续检索增强候选）。
- 不做跨机器记忆数据集同步（记忆即数据集）——见 `docs/DISTRIBUTION-*.md` 既有路线，不在本计划内展开。
- 不替代 `agent_import` 冷启动文件导入；两条通道并存、共享 review inbox 与 dedup。
- 不做 trace 的全量永久保留；只保留提取/蒸馏后的产物 + 定向保留的证据片段。

---

## 4. 概念模型：把 L0 定义为原始证据

给 L0 一个清晰、与现有映射不冲突的定义：

> **L0 = 原始证据（raw evidence）**：从 session trace 切出的 turn 块（或明确标注 superseded 的旧策展记忆，沿用现状）。它是 L1+ 蒸馏的唯一合法源，携带完整出处，永不参与注入（不直接给 Agent 看），只作为证据被查询与展开。

Layer 语义修订：

| Layer | 语义（修订后） | 来源 | 注入 |
|---|---|---|---|
| L0 | 原始证据 / 归档 | trace 摄入、superseded 回收 | ❌ |
| L1 | 原子事实（Background） | L0 蒸馏、规则/LLM 提取 | 弱 |
| L2 | 场景总结（Reference） | L1 聚合 | 中 |
| L3 | 人格规则（Must） | L2 提升 | 强 |

现有 `Priority → Layer` 映射（Must→L3 / Reference→L2 / Background→L1）不变；L0 不与任何 Priority 绑定。promote 管线的输入端从「只有显式保存」扩展为「显式保存 + L0」。

---

## 5. 方案设计

### 5.1 摄入管线（五段）

```
Discover → Parse → Extract → Dedup → Ingest(L0)
                                    ↘ 低置信 → Review inbox（复用现有）
                                    ↘ Promote（复用现有，蒸馏到 L1+）
```

**① Discover（发现）**
- 每 host 一个 discoverer，定位该 host 的会话目录（Claude Code：`~/.claude/projects/*/*.jsonl`；Codex/pi/Hermes 类似），映射到 `agent_import` 已有的 `agent_key`。
- 只读探测、无安装则返回空——沿用 `agent_import` 的「detect 返回 None 是正常结果」约定。

**② Parse（解析，复用并扩展 `transcript.rs`）**
- `transcript_to_text` 已覆盖 Claude Code JSONL；将其容忍式解析器按 host 扩展，统一输出 turn 列表（每 turn 带 host 内序号），沿用「逐行容忍、绝不因单行失败而中断、保尾」原则。
- 产物是候选 turn 块，携带 `(agent_key, session_id, turn_idx, timestamp)`——这是后续一切出处的种子。

**③ Extract（提取）**
- 两级策略，与现有提取器一致地「LLM 失败回落规则」：
  1. 规则级（`Extractor`）：低成本的偏好/事实信号，直接落 L1。
  2. LLM 级（`LlmExtractor` + `reflect_lesson`）：从多 turn 上下文提取原子记忆；对「未完成任务」做 episodic reflection。
- 语义化提取天然贵，故只对**水位线之后的新 turn** 且优先对**对话型 turn**（非纯命令回显）执行，避免全量 embedding 式成本。

**④ Dedup（复用）**
- 摄入候选先过现有 dedup（Jaccard/文本相似），与既有记忆去重；**同一次会话的重复提取在摄入侧按 turn 块指纹合并**，避免 promote 时重复聚合。

**⑤ Ingest + Review + Promote**
- 高置信候选 → 直接落 L0（超短期）或 L1（短期保留后再 promote）。建议**先落 L0**、由既有 promote 周期蒸馏——把「何时蒸馏」交给现有管线决策，摄入侧不越权蒸馏。
- 低置信 / 首次出现的 host → 走 review inbox，与 `agent_import` 冷启动候选同流（沿用「一律 Reference/未审核」策略）。

**增量水位线（贯穿 ①–⑤）**
- 每 `(agent_key, session_id)` 记录 `last_processed_turn` 与 `last_mtime`；新会话只处理新 turn，避免整库重嵌入——与 funes「增量索引、bounded backfill」一致。水位线放 storage 层新表（见 §6）。

### 5.2 触发模式

| Host | 触发 | 说明 |
|---|---|---|
| Claude Code | 会话结束 hook（现状路径，transcript 已由插件转发） | 现有机制，补「落 L0」落点即可 |
| Codex / pi / Hermes | 后台周期扫描（如 `memvault-cli ingest` + 定时） | 无 hook 或不想侵入会话时用水位线扫描；CLI 子命令 + 增量水位线天然支持 cron |
| 任意 host | 显式 `memvault-cli ingest --agent <key> --since <watermark>` | 手动补采 / 调试入口，dry-run 展示将摄入的 turn 数 |

### 5.3 Provenance 贯通（G3）

- **写入侧**：L1+ 记录新增 `source_trace` 引用——指向一组 L0 记忆 ID（或直接指向 episode 的 session/turn 段）。promote 聚合时**显式携带源 ID 列表**，使 L3 规则 → L2 → L1 → L0 → 原始 turn 形成一条完整链。
- **检索侧**：`hit_sources` 之外，新增只读 MCP 工具 `get_memory_evidence(memory_id)`：返回该记忆的完整溯源链 + 命中证据的原始文本块（等价 funes 的 `get`）。Agent 在需要「给出依据」或用户质疑时调用；`report_compliance` 争议场景可引用链上原始证据。
- 不破坏现有 `save_memory`：显式保存仍可带 `session_id`/`source_agent`（字段已在），只是不再可选地「丢出处」。

### 5.4 检索侧增强（候选，排期见 P3）

- **查询时 recency 重加权**：在 hybrid 结果融合阶段，用记忆的 recency 对得分做轻量加权（与存储侧 decay 正交——decay 决定「是否还值得留」，查询加权决定「同样相关时先给谁看」）。
- **邻近证据扩展**：当命中的是 L1/L2 且溯源链存在时，允许一次展开附加其 L0 证据块作为上下文（等同 funes 的 neighbor chunk）。
- 不做 cross-encoder 重排（见非目标），除非 benchmark 证明现有排序在 trace 来源上显著退化。

---

## 6. 涉及模块与 schema 改动

### 6.1 新增模块（`memvault-core`）

| 模块 | 职责 | 依赖 |
|---|---|---|
| `trace/`（新目录） | Discover + Parse 入口、逐 host discoverer/parser | `transcript.rs`（重构为共享 turn 解析内核） |
| `trace/watermark.rs` | 每 `(agent_key, session_id)` 水位线读写 | storage 层新表 |
| `ingest.rs` | 编排五段管线；CLI/服务的公共入口 | trace/, extractor, dedup, promote |

沿用 `agent_import` 的模式：每 host 一个子模块 + 一个 trait + `all_adapters()` 注册表。

### 6.2 schema 改动

- `memory` 表：新增 `source_trace_ids TEXT`（JSON 数组，指向源 L0 ID，仅 L1+ 非空）与 `ingest_watermark INTEGER` 相关索引列；L0 记录用 `(agent_key, session_id, turn_start, turn_end)` 表达出处区间。
- 新表 `trace_watermark(agent_key, session_id, last_turn, last_mtime, pk(agent_key, session_id))`。
- `episode`/`relation` 已有 `source_memory_id` 可衔接，不改动。

### 6.3 服务与工具面改动

- `memvault-cli`：新增 `ingest`（含 `--dry-run`、`--agent`、`--since`）。
- `memvault-mcp`：新增只读工具 `get_memory_evidence`（返回溯源链 + 原始证据块）。不含任何写操作，安全敏感面小。
- hook/插件侧：Claude Code 会话结束路径把转发产物接到 L0 落点（小改，方向见 §5.2）。

---

## 7. 分阶段实施

> 每阶段独立可验收、可回滚；除 P1 外互不阻塞。

### P0 — 概念验证（1 个 host、只读、不落库）

- 目标：证明「trace → 提取 → 有意义的 L1」成立且成本可控。
- 动作：写最小 ingest spike——扫本地 Claude Code `~/.claude/projects` 最近 N 个会话 → `transcript_to_text` → 现有规则/LLM 提取 → 人工对比产物与「Agent 主动保存」的记忆，评估重复率与新增信号。
- 验收：≥50% 的摄入候选与既有记忆不重复（或能证明可被 dedup 吸收）；单会话摄入耗时/成本可接受。

### P1 — 摄入管线落地（水位线 + L0 + CLI）

- 动作：`trace/` + `watermark.rs` + `ingest.rs`；Claude Code 解析复用；CLI `ingest --dry-run` 先行，再开实写；落 L0 交由既有 promote 周期（先手动触发）蒸馏。
- 验收：`ingest` 连续跑两次，第二次处理 turn 数 ≈ 0（水位线生效）；L0 记录带完整出处；promote 可消费 L0 产出 L1+。

### P2 — Provenance 贯通 + 证据工具

- 动作：`source_trace_ids` 字段 + promote 携带源链；MCP `get_memory_evidence`；`report_compliance` 争议路径可引用证据。
- 验收：从任意 L3 规则点击可回溯到原始 turn 文本（端到端测试用例）；`get_memory_evidence` 对无溯源（旧数据/手动保存）记忆优雅降级（返回空链而非报错）。

### P3 — 多 host 扩展 + 检索增强 + 成本基准

- 动作：接入 Codex/pi/Hermes 会话目录（复用 `agent_import` 的 `agent_key` 命名）；查询侧 recency 重加权；复刻 funes 式 handoff-vs-recall 基准（见 §9）。
- 验收：≥3 host 可摄入；基准数据支撑「召回既有记忆 ≤ 手写交接成本」的结论（或如实报告反面结果）。

---

## 8. 风险与对策

| 风险 | 影响 | 对策 |
|---|---|---|
| 摄入噪声：会话里大量「未完成/随口」内容被蒸馏成记忆 | 记忆质量下降 | 低置信走 review inbox（现状）；只对对话型 turn 做 LLM 提取；episodic reflection 只针对失败任务（`reflect_lesson` 语义已限定） |
| 隐私：会话日志含密钥/敏感信息 | 高 | 摄入复用存储层 sensitive guard；水位线表不存内容；trace 原文不落库（只落提取产物与定向证据块）——比 funes 的「本地保留全量」更保守 |
| 重复与漂移：同一件事在不同会话反复出现 | dedup 压力 | 摄入侧 turn 指纹预合并 + 复用 dedup；promote 已有聚合语义 |
| 水位线漂移：host 更新改变日志格式/路径 | 摄入中断或漏采 | 沿用「detect 失败返回 None 而非报错」约定；解析器逐行容忍；水位线按 mtime 兜底，格式变更后至少可重扫 |
| token/成本失控 | LLM 提取成本 | 默认规则级提取，LLM 级仅对话型 turn；`--since` 限流；全部走已有回落链 |
| 分层链数据膨胀 | 存储增长 | 只保留证据块指针而非全文（`source_trace_ids` 是 ID 不是文本）；decay 对 L0 同样生效 |

---

## 9. 验收标准与指标

### 9.1 功能验收（贯穿 P0–P3）

- [ ] 新会话产生的新 turn 自动出现在摄入管线，二次运行无重复处理（水位线）。
- [ ] L0 → L1+ 蒸馏产物全部携带可展开的溯源链，端到端可回溯（P2 起）。
- [ ] 摄入候选与既有记忆去重率达标（P0 定义基线），低置信全部可见于 review inbox。
- [ ] 敏感内容防护在摄入路径同样生效（不弱于显式保存路径）。

### 9.2 成本/价值基准（P3，参考 funes handoff-vs-recall）

构造两条「答案依赖会话先验知识」的任务，对比三种通道的**每成功任务加权 token**：

1. 手写交接（handoff）：人类/Agent 写交接文档后在新会话继续。
2. 压缩续跑（compaction）：长会话压缩后继续。
3. 召回（MemVault 注入 + `get_memory_evidence`）：本计划落地后，session_start 注入既有记忆。

预期（funes 数据为参考，不预设必达）：召回 ≤ 交接成本；如实报告三通道结果，不掩盖失败项。该基准沉淀为 `memvault-core/benches/` 或独立脚本，随 P3 交付。

---

## 10. 实施记录（v0.2 · 2026-09-10）

本计划已实施。落地结果与关键偏差如下。

### 已交付

| 计划项 | 交付物 | 验证 |
|---|---|---|
| 结构化 turn 解析（§5.1 ②） | `transcript.rs` 新增 `TraceTurn`/`parse_turns`（按行号 `seq`，供水位线），`transcript_to_text` 委派且零回归 | 8 单测 |
| 水位线（§5.1 增量） | `MIGRATIONS` v18 建 `trace_watermark` 表；`MemoryStore::get/set_trace_watermark` | sqlite 测试 |
| provenance 字段（§5.3/§6.2） | `MIGRATIONS` v19 加 `Memory.source_trace_ids`（serde 空跳过，向后兼容），四处写路径与行映射同步 | serde roundtrip 测试 |
| 摄入管线（§5.1） | 新模块 `trace.rs`：discover / parse / watermark / 规则提取 / L0 证据行 / 候选 | 7 单测 |
| CLI 入口（§6.3） | `memvault ingest [--agent --home --approve --dry-run --max-sessions]` | 5 测试（含 fake-home 端到端） |
| 蒸馏溯源贯通（§5.3） | `promote.rs` 四个 pass 均以 order-stable 去重并集继承 `source_trace_ids` | promote 测试 |
| 证据展开（§5.3 `get`） | `evidence::trace_evidence_chain` + MCP 只读工具 `get_memory_evidence` | 136 mcp 测试 |
| 多 host（§7 P3） | `trace.rs` discover 覆盖 claude / codex / hermes | 单测 |
| 成本基准（§9.2） | `docs/experiments/TRACE-RECALL-BENCH.md` 协议 + `benches/trace_recall_cost.rs` | `cargo bench --no-run` |

### 与计划的偏差

- **L0 落地方式**：计划写「turn 块落 L0」。实施进一步收紧为**只有产生提取信号的 turn 才保留 L0 证据行**（无信号 turn 不落库），与 §8「只保留提取产物与定向证据块」一致，避免日志全量入库。
- **provenance 载体**：计划拟「加 relation」，实施采用 **`Memory.source_trace_ids` 列**（更利于检索侧快速判断有无证据可展开），`evidence.rs` 的 `sourced_from` 仍服务于外部来源（URL/文档），两者分工不重叠。
- **检索 recency 加权**：计划 P3 拟做，实施时发现 `rerank.rs` 的 `MultiSignalReranker` **已含 `recency_weight`**（六信号加权），故未重复实现。
- **成本基准**：handoff / compaction 两通道需真实 LLM 才能测成本，交付为**协议文档**而非伪造数字；离线可测部分（recall 注入 vs 全量会话的压缩比）已落为 criterion bench（示例输出：9.2× bytes / 9.3× tokens）。
- **P0 spike**：未单独产出一次性脚本，其「trace→提取是否有意义」的验证意图由 `trace.rs` 单测与 CLI 端到端测试覆盖。

### 尚未做（后续候选）

- Claude Code Stop hook 自动触发 `ingest`（当前为 CLI/MCP 显式调用；既有 `session-extract.sh` 仍走 `extract` 路径）。
- REST 对称端点 `GET /api/memories/{id}/evidence`（MCP 工具已交付）。
- 跨机器记忆同步（计划明示非目标，见 `docs/DISTRIBUTION-*.md`）。

---

## 11. 相关文档

- [DESIGN.md](./DESIGN.md) — 产品与架构总纲（L0–L3、Memory Router、promote/decay）
- [AGENT-PORTABILITY.md](./AGENT-PORTABILITY.md) — 多 host 适配现状（`agent_key` 命名、T3 批次）
- [DISTRIBUTION.md](./DISTRIBUTION.md) / [DISTRIBUTION-TODO.md](./DISTRIBUTION-TODO.md) — 记忆分发路线（「记忆即数据集」在此展开，不在本计划内）
- funes 原始材料：[blog/funes](https://huggingface.co/blog/funes) · [github.com/huggingface/funes](https://github.com/huggingface/funes) · [handoff-vs-recall benchmark](https://huggingface.co/datasets/dacorvo/funes-handoff-recall-benchmark/blob/main/results/README.md)
