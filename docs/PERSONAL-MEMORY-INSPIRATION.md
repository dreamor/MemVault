# 启发来源：《一文搞懂个人AI记忆系统构建全流程》对 MemVault 的启发

> **来源**：腾讯云开发者公众号，左德军，2026-08-27
> **原文一句话**：把个人记忆拆成 identity/principles/preferences/context/knowledge 五层，配合两级目录、7 字段 frontmatter、INDEX+README 两层索引，以及 recall/curator 两个 skill（写入必须人工确认），实现"换 AI 工具不丢上下文"。
> **本文目的**：记录该文章与 MemVault 现有设计的对照分析，说明哪些点值得吸收、哪些已经被 MemVault 覆盖、哪些不适用，并给出对应的落地改动。

## 对照表

| 文章的做法 | MemVault 现状 | 结论 | 落地 |
|---|---|---|---|
| 五层记忆本体（identity/principles/preferences/context/knowledge） | 已有三条正交轴：`Priority`(Must/Reference/Background)、`MemoryLayer`(L0-L3)、`MemoryType`(Preference/Fact/Episode/Entity/Skill) | **不直接采用** —— 见下文理由 | 无新字段，改为让 `MemoryType` 承担更多职责 |
| 权重与目录绑定，人不单独填权重字段 | `Priority` 是显式字段，直接可查询、可排序 | **已覆盖，且更优** | 无需改动 |
| 7 字段 YAML frontmatter（含 `privacy: internal/public`） | 有 `Visibility`(Scoped/Shared)、`tags`、`namespace`，但无独立 `title`/`description` 字段 | **部分借鉴** | 见 Feature D 的取舍说明 |
| 冲突处理：AI 检测到冲突要显式提出来，裁决权留给人 | `evidence.rs` 已建模 `contradicts` 关系，`decay.rs` 用它把矛盾记忆的衰减速度乘 3；但唯一人可见的入口是 `doctor.rs` 的离线巡检报告，注入链路里完全没有冲突提示 | **采用** | Feature B |
| recall skill 先查"任务类型 → 该读哪层"路由表，再检索 | `intent.rs::should_exclude_for_intent` 只有排除型规则（负向），检索阶段没有正向的"这个任务类型应该优先这个记忆类型"加权 | **采用** | Feature C |
| identity/principles 稳定几乎不变，context 易变频繁更新 | `decay.rs` 对所有 `MemoryType` 用同一个 `daily_decay_rate`，只按 `Priority::Must` 全免、按矛盾关系加速，不按类型区分 | **采用** | Feature A |
| INDEX.md：只放 title + description 一行，AI 先扫地图再决定读哪篇正文 | `sync.rs` 的每个 `generate_*` 都是全文转储（按字符预算截断），没有"只给目录不给正文"的轻量产物 | **借鉴** | Feature D |
| 经验反复出现 → 提炼成技能，目前作者仍是人工判断，列为未来计划 | `episode.rs::maybe_draft_skill` 已经做到：同一 `task_type` 下连续 3 次成功自动起草技能草稿，进 review inbox；`promote.rs` 也已有 L1→L2→L3 自动晋升 | **已领先，无需改动** | 仅记录，不产生代码变更 |
| 写入必须人工确认，故意不做全自动 | `ai_generated`/`human_reviewed` 两个布尔位 + `review_memory`/`list_inbox` 的 approve/reject/edit 工作流，语义完全一致 | **已覆盖** | 无需改动 |
| 目录有且只有两级、禁止无限嵌套 | MemVault 是 SQLite 存储，不是文件树；唯一写文件的是 `sync.rs`（生成 CLAUDE.md 等）和 Obsidian 插件的单向同步，两者本身已经是扁平结构 | **不适用** | 无需改动 |

## 为什么不直接搬五层本体

MemVault 已经有三条正交的分类轴（优先级、分层、类型），文章的 identity/principles/preferences/context/knowledge 如果原样加进来，会变成第四条与前三条高度重叠的轴——`identity`/`principles` 基本对应现在的 `Priority::Must` + `MemoryType::Preference/Skill`，`context` 基本对应 `Episode`，`knowledge` 基本对应 `Fact`/`Skill`。多加一个字段只会增加分类时的选择成本，而不会带来新信息。

真正有价值的是文章背后的**信念**——"稳定的自我"和"易变的当下"应该被系统区别对待——这一点 MemVault 现在完全没体现（衰减速率对所有类型一视同仁）。所以选择的落地方式是：不加字段，而是让已有的 `MemoryType` 在衰减速度（Feature A）和检索路由（Feature C）上真正发挥区分作用。

## 落地功能

### Feature A — 按类型区分衰减稳定性

`decay.rs` 新增 `type_stability_multiplier(memory_type)`：`Skill`/`Preference`（近似文章的 identity/principles/knowledge）衰减更慢（×0.5），`Episode`（近似 context）衰减更快（×1.3），`Fact`/`Entity` 保持基线（×1.0）。与现有的 `Priority::Must` 全免、矛盾关系 ×3 加速叠加计算。

### Feature B — 注入阶段显式提示冲突，而不是静默处理

`evidence.rs` 新增 `contradictions_among`，在 `router.rs::session_start_layered` 组装完最终注入列表后，对本次真正会被注入的记忆做一次矛盾关系批量检查，结果放进 `SessionStartOutput.conflicts`；`format.rs` 渲染出一个独立的 `[MEMORY CONFLICT - 需要你决定]` 区块，明确写"不要自动二选一，请结合当前上下文自行判断"。`doctor.rs` 的巡检报告保持不变，两者互补：一个是离线体检，一个是实时提示。

### Feature C — 检索阶段按任务类型正向加权

`intent.rs` 新增 `intent_type_boost(intent, memory_type)`，与现有的排除表对称：`Coding → Skill/Fact` 加权、`Writing → Preference` 加权、`Research → Fact/Entity` 加权、`Project → Episode` 加权。在 `router.rs` 原本应用排除惩罚的同一处乘入这个加权系数。

### Feature D — `memvault sync` 增加轻量 INDEX 产物

`sync.rs` 新增 `generate_index_md`，只输出「类型 + 优先级 + 内容前 80 字符当作伪标题 + namespace/tags」的一行式清单，写成 `MEMORY-INDEX.md`。

**取舍说明**：文章的 frontmatter 里 `title`/`description` 是独立维护的字段，MemVault 的 `Memory` 目前没有对应字段，v1 直接用 `content` 的前若干字符做伪标题，不是真正意义上的人工撰写摘要。这是有意的简化，不是疏漏——真正加 `title`/`description` 字段涉及存储 schema 迁移和所有写入路径的改动，超出本次范围，留作后续可选项。

## 明确不做的事

- 不引入 identity/principles/preferences/context/knowledge 作为新字段或新枚举。
- 不改动目录深度约束——不适用于 SQLite 存储模型。
- 不改动 promote/技能自动起草流程——已经比文章的实现更完整，本次只做记录。
