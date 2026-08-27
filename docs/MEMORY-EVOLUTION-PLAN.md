# 三类记忆演进计划：情景 / 语义 / 程序

> **版本**：v0.1
> **日期**：2026-08-26
> **状态**：规划中（待评审）
> **定位**：把 MemVault 从「陈述性记忆路由层」演进为完整的 Agent 认知记忆系统
> **前提**：本计划构建在现有能力（混合检索、自动注入、promote 管道、compliance 追踪、decay）之上，不改动任何已发布的行为承诺

---

## 目录

1. [背景与目标](#1-背景与目标)
2. [总体架构：经验学习闭环](#2-总体架构经验学习闭环)
3. [情景记忆（Episodic）](#3-情景记忆episodic任务成败与教训)
4. [语义记忆（Semantic）](#4-语义记忆semantic领域知识库)
5. [程序记忆（Procedural）](#5-程序记忆procedural技能与工作流)
6. [分阶段计划](#6-分阶段计划)
7. [里程碑与交付物](#7-里程碑与交付物)
8. [风险与应对](#8-风险与应对)
9. [兼容性与不破坏承诺](#9-兼容性与不破坏承诺)
10. [开放问题](#10-开放问题待决策)

---

## 1. 背景与目标

### 1.1 为什么是这三类记忆

`DESIGN.md` §5.3 早已定义了记忆分层模型（L1 工作记忆 → L2 情景 → L3 语义 → L4 程序），§8 的 Obsidian 目录也预留了 `10-Daily`(情景) / `20-Entities`(语义) / `40-Skills`(程序)。但当前实现以陈述性记忆（preference / fact）为中心，三类认知记忆的成熟度不均：

| 记忆类型 | 认知含义 | 现有对应 | 成熟度 |
|---|---|---|---|
| 情景记忆 | 「发生过什么、哪次成功/失败」 | `MemoryType::Episode` 存在，但无结构化结果、无专用抽取通道 | ★☆☆ |
| 语义记忆 | 「领域知识、事实与关系」 | `Fact` / `Entity` 类型、实体抽取、dedup、confidence | ★★☆ |
| 程序记忆 | 「怎么做」的技能与流程 | `Skill` + `SkillMeta { trigger, steps, verification, version }`，REST 已支持局部更新 | ★★★ |

### 1.2 目标

- **G1**：Agent 不再重复踩同一个坑——任务失败 → 教训 → 自动注入后续会话（情景闭环）
- **G2**：领域知识被积累、被修正、被关联——而不是散落的孤立事实（语义知识库）
- **G3**：被验证过的工作流沉淀为可执行技能，在意图命中时自动浮现（程序技能库）
- **G4**：三类记忆互相喂养——情景反思出教训，高频事实巩固为语义知识，重复成功的流程沉淀为技能

### 1.3 非目标（范围约束）

- **不做通用世界知识库**：世界常识由模型自身承担；MemVault 只沉淀个人/项目/组织级的领域知识
- **不引入图数据库**（本轮）：轻量关系表起步，图数据库集成维持 `DESIGN.md` Phase 5 的排期
- **不做 Agent 运行时**：MemVault 仍是记忆基础设施层，不是框架

---

## 2. 总体架构：经验学习闭环

```
                    ┌─────────────────────────────────────────────┐
                    │              MemVault 记忆闭环               │
                    └─────────────────────────────────────────────┘

  Agent 执行任务 ──→ record_outcome（主动汇报 / proxy 兜底抽取）
                          │
                          ▼
                 ┌─────────────────┐      LLM 反思（本地优先）
                 │  情景记忆        │ ─────────────────────┐
                 │  Episode + 结果  │                      ▼
                 └────────┬────────┘                 教训（lesson）
                          │                               │
            promote 巩固   │                               │ 指令化注入
            （高频事实）    ▼                               ▼
                 ┌─────────────────┐              ┌─────────────────┐
                 │  语义记忆        │              │ 下一次同类任务    │
                 │  Fact/Entity     │              │ 带着教训开始      │
                 │  + 关系三元组     │              └────────┬────────┘
                 └─────────────────┘                        │
                          ▲              重复成功 + 人工确认   │
                          │                                 ▼
                 ┌─────────────────┐              ┌─────────────────┐
                 │  程序记忆        │ ←─── 沉淀 ─── │  任务结果再汇报    │
                 │  Skill + Meta    │              │  （compliance）   │
                 │  trigger/steps   │              └─────────────────┘
                 └─────────────────┘
                    意图命中 → 结构化注入
```

### 三类记忆与现有模块的映射

| 模块 | 情景记忆 | 语义记忆 | 程序记忆 |
|---|---|---|---|
| **存储** | `memories`(type=episode) + 新 `episodes` 表 | `memories`(type=fact/entity) + 新 `memory_relations` 表 | `memories`(type=skill) + `skill_meta` 列（已有） |
| **写入** | `record_outcome` tool + 反思 | extract 管道 + promote 巩固 | 抽取信号词（已有）+ 教训沉淀 + 手工 |
| **检索** | 混合检索 + outcome/task_type 过滤 | 混合检索 + 关系一跳扩展 | `intent.rs` × `skill_meta.trigger` 匹配 |
| **注入** | 教训以 REFERENCE（重复失败升 MUST） | BACKGROUND 上下文 | steps + verification 结构化指令 |
| **生命周期** | decay（经验淡化）+ archive L0 | `superseded_by` 版本取代 | version 演化 + 成功率 |
| **追踪** | compliance 复用：避坑率 | confidence | 技能成功率（compliance 扩展） |

**核心论点**：竞品（Mem0 / MemPalace / Zep）均为被动检索，而经验类记忆「不主动出现就没有价值」。MemVault 的自动注入 + MUST 指令化 + compliance 追踪正好是三类记忆各自最需要的载体——这是本计划的可行性根基。

---

## 3. 情景记忆（Episodic）：任务成败与教训

### 3.1 现状盘点

- `MemoryType::Episode` 已存在，`llm_extractor.rs` 的输出 schema 支持 `"episode"` 类型
- **缺口**：无结构化任务结果（成败/原因/教训）；无专用规则抽取通道（`extractor.rs` 只有 skill 信号词）；无「相似历史失败」的检索维度
- **可复用地基**：`promote.rs` L1→L2 巩固、`rerank.rs` 对 episodic 的新鲜度加权、`decay` 生命周期、`source_session_id` 会话关联

### 3.2 Schema 设计（migration 6）

**方案：独立 `episodes` 表**（推荐，保持 `memories` 主表泛化，episode 专有字段不污染其他类型）：

```sql
CREATE TABLE episodes (
    memory_id        TEXT PRIMARY KEY REFERENCES memories(id),
    task             TEXT NOT NULL,          -- 任务描述
    task_type        TEXT,                   -- 归类标签：deploy / debug / refactor ...
    status           TEXT NOT NULL,          -- success | failure | partial
    cause            TEXT,                   -- 归因（可选）
    lesson           TEXT,                   -- 反思产出的教训（可后补）
    lesson_memory_id TEXT,                   -- 教训对应的指令化记忆 id（若已生成）
    occurred_at      TEXT NOT NULL
);
CREATE INDEX idx_episodes_task_type ON episodes(task_type);
CREATE INDEX idx_episodes_status ON episodes(status);
```

配套 `memories` 增列（migration 6 同批）：

```sql
ALTER TABLE memories ADD COLUMN superseded_by TEXT;  -- 语义记忆版本化用，见 §4.4
```

### 3.3 接口设计

**新增 MCP tool**（13 → 14 个）：

| Tool | 参数 | 说明 |
|---|---|---|
| `record_outcome` | `task`, `status`, `cause?`, `task_type?`, `tags?`, `namespace?` | Agent 主动汇报任务结果；自动创建 episode 记忆（type=episode, layer=L1） |

**CLI**：`memvault outcome --task "..." --status failure --cause "..." --type deploy`
**REST**：`POST /api/outcome`（接入 `AgentAuth`）

**检索扩展**：`search_memory` / `memvault search` 增加 `--status`、`--task-type` 过滤（SQL 层过滤，不影响打分）。

### 3.4 反思机制（lesson 生成）

- `status = failure`（或 `partial`）触发反思：复用 `LlmExtractor` 架构（本地优先 Ollama，失败回退规则），输入 = task + cause + 相关上下文，输出 = 一句话教训
- 教训双写：① `episodes.lesson` 留档；② 生成一条指令化记忆（`instruction` 字段，默认 `REFERENCE`）
- **升级规则**：同一 `task_type` 累计 ≥2 条同类失败教训 → 提示人工确认后升级为 `MUST`（防止失控，见 §10）
- 复用 `SourceRole` 守卫（`extract_guarded`）：agent 自我汇报的结果打 `review:required` 标记并降置信，防止自我强化漂移

### 3.5 注入策略

- `session_start` 时，按上下文意图匹配 `task_type`，命中的教训以 REFERENCE 注入；MUST 教训走指令层（永不过滤，现有机制）
- 教训计入现有 token budget，逐条 `InjectSkipReason` 留痕（现有机制）
- 增加类型配额：单次注入中教训类记忆上限（建议 3 条），避免「失败史」挤占工作上下文

### 3.6 验收标准

- **H5**（新假设，方法沿用 `docs/experiments/`）：注入教训后，同类任务重复失败率下降——A/B：有教训注入 vs 无，LLM-as-judge
- 教训抽取质量：人工抽检准确率 ≥ 80%
- `record_outcome` 全链路（MCP + CLI + REST）测试覆盖 ≥ 80%（仓库门槛）

> **✅ H5 已于 2026-08-26 实测通过（CONFIRMED）**：本地开源模型（qwen2.5-1.5b 执行 / qwen2.5-3b 评审）下，特定知识传达率对照组 0% → 实验组 90%（+90%）。方法与结果详见 `docs/experiments/REPORT.md` H5 章节。
>
> **实施状态（2026-08-26）**：Phase A 的 A1–A6 已全部落地——Schema（migration 6–9）、`record_outcome` 全链路（MCP/CLI/REST）、教训反思（`reflection.rs`）、教训注入（`router.rs` task_type 匹配 + 配额）、Dashboard Episodic 页、H5 验收。全工作区测试 584 绿、覆盖率达标（`episode.rs` 86% / `reflection.rs` 92%）。

---

## 4. 语义记忆（Semantic）：领域知识库

### 4.1 现状盘点

- `Fact` / `Entity` 类型、实体抽取、语义去重、`confidence` 均已具备
- **缺口**：实体间无关系；事实无版本化（旧知识无法被新事实取代）；知识无溯源（不知道某条知识从哪次经历来）
- **可复用地基**：`DESIGN.md` §8 已预留 `20-Entities` 目录；`query_expand.rs` 有查询扩展基础；dedup 管道可做实体归一

### 4.2 Schema 设计（migration 7）

轻量三元组关系表起步，不引入图数据库：

```sql
CREATE TABLE memory_relations (
    relation_id      INTEGER PRIMARY KEY AUTOINCREMENT,
    subject_id       TEXT NOT NULL,        -- 实体记忆 id
    predicate        TEXT NOT NULL,        -- uses | belongs_to | depends_on | decided | located_in ...
    object_id        TEXT,                 -- 对象为实体时
    object_text      TEXT,                 -- 对象为自由文本时（与 object_id 二选一）
    confidence       REAL NOT NULL DEFAULT 0.8,
    source_memory_id TEXT,                 -- 来源情景记忆（溯源）
    created_at       TEXT NOT NULL
);
CREATE INDEX idx_relations_subject ON memory_relations(subject_id);
CREATE INDEX idx_relations_object ON memory_relations(object_id);
```

### 4.3 事实巩固：情景 → 语义（扩展现有 promote 管道）

在 `promote.rs` 新增第三阶段：

- **事实巩固**：在多条 episode 中反复出现（≥N 次）的事实性陈述 → 提升为 L2/L3 语义记忆，`source_memory_id` 记录来源情景
- **实体归一**：保存实体前先检索现有实体（复用 `dedup.rs` 相似度判定），命中则合并而非新建

### 4.4 事实版本化

- `memories.superseded_by` 列（§3.2 已随 migration 6 加入）：新事实保存时若与旧事实冲突，旧事实指向新事实并归档到 L0——**不物理删除**
- 检索默认过滤被取代事实；`memory_history` + `memvault restore` 已提供回滚能力
- 冲突检测策略：先做保守版——LLM 抽取时输出 `supersedes` 候选，人工确认后生效（避免误杀）

### 4.5 检索增强

- 实体命中时一跳关系扩展：返回实体 + 直接关系（预算内），为 `search_memory` 增加 `expand_relations: bool` 参数
- 检索结果附溯源：该知识由哪些情景记忆巩固而来（复用 `hit_sources` 的留痕思路）

### 4.6 验收标准

- **H6**（新假设）：同领域知识跨会话一致——同一问题在两次会话中得到一致且正确的答案（注入语义记忆组 vs 裸跑）
- 关系抽取精确率（人工抽检）≥ 75%；误取代率 < 5%

> **实施状态（2026-08-27）**：Phase C 的 C1–C5 已全部落地——
> - **C1 关系存储**：`memory_relations` 表（实际为 migration 11–13：表 + 双索引；计划中的「7」为占位编号），三元组（subject→predicate→object），端点级联清理、溯源置空；`MemoryStore` 新增 `add_relation`/`relations_of_subject`/`relations_of_object`/`delete_relation`
> - **C2 关系抽取**：`LlmExtractor::extract_relations`（本地优先，独立提示词含注入防护）+ `relations::store_relation_triples`（实体归一解析、去重、自由文本对象）；`MEMVAULT_RELATIONS=on` 显式开启（Q4 决策），接入 MCP `extract_memories`（mode=llm）
> - **C3 语义巩固**：`promote` 管道新增两个前置阶段——**事实巩固**（相似事实聚类合并为单条 L2 语义事实，置信度提升，来源以 `consolidated_from` 关系留痕并归档 L0）+ **实体归一**（近重复实体合并，关系重定向到「连接更多」的规范实体，被并者打 `superseded_by` 归档）
> - **C4 事实版本取代**：`MemoryStore::supersede`（旧知识归档 L0 + 指向新事实，不删除、可回滚）；REST `POST /api/memories/{id}/supersede` + CLI `supersede`；**检索默认排除被取代记忆**（含向量路径），`list` 仍可见（历史可查）
> - **C5 检索关系扩展**：`SearchQuery.expand_relations` 参数；MCP `search_memory` / REST `/api/search` 支持 `expand_relations`，逐结果附一跳关系邻域（`relations` 数组 + 可读 `line`）；注入侧 `format_injection_with_relations` 追加 `[RELATIONS]` 块（限 8 条记忆 × 5 行，防上下文膨胀）
> - 全工作区测试绿；`relations.rs` 覆盖率 95%；端到端用例验证「巩固 → 检索展开」闭环
> - **H6 验收实验（C6）待做**：方法同 H5/H7（本地模型 A/B + 客观主判定）

---

## 5. 程序记忆（Procedural）：技能与工作流

### 5.1 现状盘点（基础最好的一环）

- `Skill` 类型 + `SkillMeta { trigger, steps, verification, version }` 已建模（`models.rs`）
- `extractor.rs` 已有 skill 信号词通道（「部署流程」「how to」「steps to」…）
- REST `PUT /api/memories/{id}` 已支持 `skill_trigger/skill_steps/skill_verification` 局部更新
- **缺口**：trigger 未与 `intent.rs` 打通（技能不会在意图命中时主动注入）；`version` 无演化机制；无成功率追踪

### 5.2 工作项

| # | 工作项 | 说明 |
|---|---|---|
| P1 | **Trigger 激活** | `session_start` 时将上下文意图与 `skill_meta.trigger` 匹配，命中技能进入注入候选 |
| P2 | **结构化注入** | 命中的技能以指令格式注入（复用 `router/format.rs`），格式见 §5.3 |
| P3 | **成功率追踪** | 扩展 `compliance.rs`：注入技能后，`record_outcome` 回报关联该技能 → 累计 `skill_stats`（注入次数 / 遵循次数 / 任务成功次数） |
| P4 | **版本演化** | 技能关联任务失败（教训命中技能步骤）→ 标记技能待修订，人工修订后 `version += 1`；旧版本经 `memory_history` 可回滚 |
| P5 | **技能审核** | 自动沉淀的技能强制进 inbox 人工审核（现有 review 流），人工创建可跳过 |

### 5.3 注入格式

```
[SKILL: deploy-pages] (v3 · 成功率 87% · 基于 8 次执行)
触发条件：部署静态站点到 Aone Pages
步骤：
  1. ...
  2. ...
验证：...
```

- 技能注入也计入 token budget 与 `InjectSkipReason`；类型配额建议单次 ≤ 2 个技能
- 成功率基于最小样本（建议 ≥ 3 次）才展示，避免小样本误导

### 5.4 技能的三个来源

1. **显式教授**：用户描述流程（现有抽取通道）
2. **经验沉淀**：同类任务 ≥3 次成功且流程相似 → 提示用户确认后生成技能草稿（情景 → 程序）
3. **外部导入**（Phase D）：Markdown SOP 解析为 `SkillMeta`

### 5.5 验收标准

- **H7**（新假设）：技能注入提升任务一次性成功率——A/B：注入技能组 vs 无
- Trigger 误命中率 < 5%（方法沿用 H4 的误注入率实验）

> **实施状态（2026-08-27）**：Phase B 的 B1–B5 已全部落地——
> - **B1/B2 触发注入**：`session_start` 按 `skill_meta.trigger` × 上下文匹配（整体包含 + 半数 token 重叠，CJK 友好，见 `intent::trigger_matches_context`），命中技能渲染为结构化指令块（`[SKILL: 标题] (v版本 · 成功率 · 执行次数)` + 触发条件/步骤/验证）注入；技能配额单次 ≤2（`SkillQuotaExceeded` 留痕）；成功率 ≥3 次执行才展示（`SKILL_RATE_MIN_SAMPLES`）
> - **B3 成功率追踪**：新 `skill_stats` 表（migration 10，级联清理）；`record_outcome` 新增 `skill_id` 归因（MCP/CLI/REST 三端）；注入即计 `injected_count`，结果计 `success/failure_count`
> - **B4 版本演化**：失败命中技能触发器 → 自动打 `needs-revision` 标记（`flagged_skills` 随响应返回）；人工经 `PUT /api/memories/{id}` 修订技能 → `version += 1` 并清除标记（`memory_history` 可回滚旧版）；内容未变的编辑不升版
> - **B5 经验沉淀**：同 `task_type` 累计 ≥3 次成功（`SKILL_DRAFT_THRESHOLD`）→ 自动生成技能草稿（trigger=task_type，steps 取自成功任务描述），`skill-draft` 标记 + 强制进 inbox 人工审核，同一 task_type 不重复生成
> - **顺带补齐**：REST `POST /api/memories` 支持 `skill_trigger/skill_steps/skill_verification`（此前仅 MCP 有，REST 创建的技能无 meta 而无法被触发）
> - 全工作区 611 测试绿；新代码覆盖率达标（`episode.rs` 91%、`intent.rs` 85%）；真实服务器端到端验证：结构化注入 → 成功率展示（100% · 基于 3 次执行）→ 失败标记 → 修订升版（v1→v2）→ 草稿进 inbox
> - **✅ H7 验收实验（B6）已通过（2026-08-27）**：驱动真实 `memvault-mcp` 服务器实测——技能注入使特定步骤传达率 0% → 78%（+78%，CONFIRMED）；40 次无关上下文零误注入（0% < 5%，CONFIRMED）。实验暴露并修复了配额缺陷（显式匹配技能优先于泛检索浮入项，`HitSource::ExplicitMatch`）。详见 `docs/experiments/REPORT.md` H7 章节

---

## 6. 分阶段计划

> 排期依据单人/小队节奏估算；每阶段结束先跑验收实验再进下一阶段。

### Phase A：情景记忆 MVP（第 1–3 周）

| # | 任务 | 涉及模块 |
|---|---|---|
| A1 | `episodes` 表 + `superseded_by` 列（migration 6，含 schema checksum） | `storage/sqlite.rs` |
| A2 | `record_outcome`：MCP tool + CLI `outcome` + REST `POST /api/outcome`（接入 AgentAuth） | `memvault-mcp`, `memvault-cli` |
| A3 | 教训反思：`LlmExtractor` 扩展 + 规则回退 + SourceRole 守卫 | `llm_extractor.rs`, `extractor.rs` |
| A4 | 教训注入：router 集成 + task_type 意图匹配 + 类型配额 | `router.rs`, `intent.rs` |
| A5 | Dashboard：outcome 上报表单 + 教训列表 | `dashboard/` |
| A6 | H5 验收实验 | `docs/experiments/` |

**依赖**：无新增外部依赖，全部复用现有基建。

### Phase B：程序记忆激活 + 情景↔程序闭环（第 4–6 周）

| # | 任务 | 涉及模块 |
|---|---|---|
| B1 | Skill trigger × intent 匹配（P1） | `router.rs`, `intent.rs` |
| B2 | 技能结构化注入格式（P2） | `router/format.rs` |
| B3 | compliance 扩展：技能成功率（P3） | `compliance.rs` |
| B4 | 教训驱动的技能版本演化（P4） | `promote.rs` 或新 `skill_evolve.rs` |
| B5 | 经验沉淀：重复成功 → 技能草稿（进 inbox） | `extractor.rs` |
| B6 | H7 + trigger 误命中率验收 | `docs/experiments/` |

**依赖**：Phase A 完成（教训与 outcome 回报是技能演化的输入）。

### Phase C：语义巩固（第 7–10 周）

| # | 任务 | 涉及模块 |
|---|---|---|
| C1 | `memory_relations` 表（migration 7） | `storage/sqlite.rs` |
| C2 | 关系抽取（LLM，本地优先，进审核队列） | `llm_extractor.rs` |
| C3 | promote 第三阶段：事实巩固 + 实体归一 | `promote.rs`, `dedup.rs` |
| C4 | `superseded_by` 版本取代流程（冲突检测保守版） | `dedup.rs`, `llm_extractor.rs` |
| C5 | 检索关系一跳扩展 + 溯源 | `hybrid.rs`, `query_expand.rs` |
| C6 | H6 验收实验 | `docs/experiments/` |

**依赖**：Phase A/B 持续供给情景素材；C4 使用 migration 6 已加的列。

### Phase D：远期（可选，进入原路线图 Phase 5）

- 图数据库集成（关系规模超出单表一跳扩展的收益点时再启动）
- 团队共享经验池（可见性控制 + 命名空间隔离）
- 外部 SOP/Markdown 批量导入技能
- Obsidian 插件三向同步：`10-Daily` / `20-Entities` / `40-Skills` 目录落地

---

## 7. 里程碑与交付物

| 里程碑 | 时间 | 交付物 | 验收 |
|---|---|---|---|
| **M1** | 第 3 周末 | 情景记忆闭环：`record_outcome` + 教训反思 + 自动注入 | ✅ H5 实测 0%→90%（CONFIRMED，2026-08-26） |
| **M2** | 第 6 周末 | 程序记忆激活：技能触发注入 + 成功率 + 版本演化 | ✅ B1–B6 全部落地（2026-08-27）；H7 实测 0%→78% + 误注入 0%，均 CONFIRMED |
| **M3** | 第 10 周末 | 语义巩固：关系表 + 事实巩固 + 版本取代 | ✅ C1–C5 落地（2026-08-27）；H6 实验待做 |

每个里程碑同时要求：`cargo test` 全绿、`cargo llvm-cov` 覆盖率不低于仓库基线（~92%）、新模块 ≥ 80%、CI 全 job 通过、README/CHANGELOG 同步。

---

## 8. 风险与应对

| 风险 | 影响 | 应对 |
|---|---|---|
| 教训抽取幻觉 | 错误教训被注入，误导后续任务 | SourceRole 守卫（已有）+ inbox 审核 + confidence 阈值 + 教训升 MUST 必须人工确认 |
| 自我强化漂移 | Agent 自我汇报的失败生成自我辩护式教训 | `extract_guarded` 降级 + 自我汇报强制 `review:required` |
| 注入膨胀 | 教训/技能挤占工作上下文预算 | 现有 token budget + `InjectSkipReason` 逐条留痕 + 类型配额（教训 ≤3、技能 ≤2） |
| outcome 回报率低 | 情景记忆断供 | dsh 插件式 turn-end 自动抽取（已验证路径）+ MCP Proxy 兜底抽取 |
| Trigger 误命中 | 无关技能被注入 | 复用 router 软惩罚（降分而非硬排除）+ H4 式误命中率回归实验 |
| 误取代正确知识 | 语义版本化误杀旧事实 | 取代需人工确认（保守版）+ `memory_history` 可回滚 + L0 归档不删除 |
| Schema 演进破坏旧库 | 存量用户数据不可用 | 仅用增量 migration + schema checksum fail-closed（均已有机制） |

---

## 9. 兼容性与不破坏承诺

- 所有 schema 变更走 migration 追加，**不改列、不删列**；旧数据库无缝升级（`schema_migrations` + checksum 已兜底）
- 现有 13 个 MCP tool 签名不变；新能力以新 tool（`record_outcome`）与新参数（`expand_relations`、`--status` 等）形式加入
- 未启用新特性（无 episode 数据、无关系数据）时，行为与当前版本完全一致——检索、注入、decay 均无感知
- 导出/导入兼容：`memvault export` 格式向后兼容，新增字段缺省可读

---

## 10. 开放问题（待决策）

| # | 问题 | 当前倾向 |
|---|---|---|
| Q1 | 教训升级为 MUST 是否需要人工确认？ | **需要**（防止低置信教训变强制指令） |
| Q2 | `episodes` 与 `memories` 一对一还是一对多（一个任务多条结果）？ | 一对一（保持检索统一）；重试场景用多条 episode |
| Q3 | 技能成功率展示的最小样本数？ | 3 次（低于则不展示百分比） |
| Q4 | 关系抽取是否默认开启？ | 默认关闭，`MEMVAULT_RELATIONS=on` 显式开启（成本与噪音考量） |
| Q5 | 教训是否需要跨命名空间共享（全局避坑）？ | 默认命名空间内；`global` 教训需人工标记 |

---

## 附：与现有文档的关系

- 本计划是 `DESIGN.md` §5.3（记忆分层模型）的**落地路线图**；DESIGN.md Phase 5 的图数据库集成对应本计划 Phase D
- 验收实验方法沿用 `docs/experiments/`（H1–H4 模板），新增假设编号从 H5 起
- 实施产出的变更按仓库惯例记录于 `CHANGELOG.md`
