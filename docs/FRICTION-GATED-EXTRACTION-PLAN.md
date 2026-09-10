# 摩擦信号驱动的经验沉淀 —— 实现计划

> 状态：Phase 1（§1–§7）与 Phase 2（§9）已实现；Phase 3（§10–§12）仍是待评审的草案。
>
> - **Phase 1（§1–§7，已实现）**：Stop hook 抽取门控。实现见 `crates/memvault-core/src/friction.rs`（信号计算 + `min_friction_threshold`）与 `crates/memvault-cli/src/lib.rs::resolve_extract_text`（门控点，`MEMVAULT_HOOK_EXTRACT_MIN_FRICTION` 默认 `1`）。测试见 `friction.rs` 内的单元测试与 `memvault-cli/src/lib.rs` 的 `test_extract_hook_skips_low_friction_transcript` / `test_extract_hook_saves_high_friction_transcript`。
> - **Phase 2（§9，已实现，落地方式与原草案不同，见下）**：摩擦证据落库 + outcome 补录建议。实际实现没有采用 §9.1/§9.2 原设想的"复用现有字段，不加列"——探查发现 `Memory` 上没有任何空闲文本字段可复用，塞进 `content` 又会把证据文本污染进记忆本身、被未来的注入一直带着。改为给 `Memory` 加一个可空新列 `friction_evidence: Option<String>`（migration 20，`ALTER TABLE memories ADD COLUMN friction_evidence TEXT`，与 `occurred_at`/`skill_meta`/`source_trace_ids` 等历史上 17 次加列走同一模式），单个格式化字符串（信号计数 + outcome 补录建议一句话），不是 §9.1 设想的 `FrictionEvidence`/`BTreeMap`/`summaries` 结构。实现见 `crates/memvault-core/src/friction.rs::evidence_note`、`models.rs::Memory.friction_evidence`、`storage/sqlite.rs`（migration + `row_to_memory`/`save`/`update`/`save_with_embedding`）、`memvault-cli/src/lib.rs`（`ResolvedExtract` 携带 `FrictionScore` 穿过门控 + `memvault review` 打印证据）、`memvault-mcp/src/server.rs::list_inbox` 与 `rest_api.rs::memory_to_json`（JSON 输出新增字段）、dashboard `api.ts`/`App.tsx`（详情页展示）。
> - Phase 1+2 的 `cargo test`（core/cli/mcp 全绿）、`cargo clippy --all-targets`（零告警）、`cargo fmt --all -- --check`、dashboard `npm run build`（tsc 通过）与 `npm test`（65 测试全绿）均已验证。
> - **Phase 3（§10，已实现，落地方式与原草案有几处实质差异，见下）**：复发匹配细化 escalation（10.1）、harmful→decay（10.2，只降置信度/加速衰减，明确不碰 `human_reviewed`）、contradicts→更新草稿（10.3）。三项 + 收件箱关系可见性（新增的第四项，原草案未单独列出）均已实现并通过全量 `cargo test --workspace`（core 683 / mcp 137 / cli 57 / proxy 75，全绿）、`cargo clippy --workspace --all-targets`（零告警）、`cargo fmt --all -- --check`。

## 1. 背景与动机

[TeamAI-cli](https://github.com/Tencent/teamai-cli) 的自动经验积累核心是一套"摩擦信号打分 → 达标才总结入库"的触发机制：又长又顺的会话不会触发抽取，只有用户打断/纠正过 AI、拒绝过某次工具调用、或工具反复重试出错的会话才被认为值得沉淀经验。每个会话最多提示一次，人始终在环上。

MemVault 现有的 Stop hook（`plugins/memvault/hooks/session-extract.sh`，由 `MEMVAULT_HOOK_EXTRACT=1` 开启）已经具备与 TeamAI 对应的大部分闭环：

| TeamAI 环节 | MemVault 对应能力 | 位置 |
|---|---|---|
| 总结与入库 | `extract` 命令解析 transcript，存入审核收件箱（未审核草稿） | `crates/memvault-cli/src/lib.rs` `Commands::Extract` |
| 人在环上,不自动提交 | 抽取结果默认 `human_reviewed=false`，落入收件箱等人工 `review_memory` | 同上 + `review_memory` |
| 防止再犯 | `record_outcome`：失败任务自动蒸馏教训，注入同类型后续任务 | `crates/memvault-core/src/episode.rs` |
| 生命周期治理（晋升/归档） | `run_promote`（L1→L2→L3 合并）、`run_decay`（过期归档） | `crates/memvault-core/src/promote.rs`、`decay.rs` |

**唯独缺的是触发层**：现在 Stop hook 不管会话顺不顺、有没有摩擦，只要开关是开的，每次会话结束都无差别跑一次抽取。本计划要补的就是这一层——用摩擦评分替换"每次都抽"，其余环节完全复用现有基础设施，不做改动。

### 目标

在不引入新存储表、不改变现有 `extract` / `record_outcome` / `run_promote` / `run_decay` 语义的前提下，让 Stop hook 只在"这个会话有摩擦"时才触发抽取。

### 非目标（本轮不做）

TeamAI 在达标后会给用户一个**可见提示**，建议手动运行分享命令——这一步在 MemVault 里没有对等的 UI 通道：`hooks.json` 的注释明确写着 Stop hook 的标准输出被 host 忽略，只有 `statusMessage` 会短暂显示。要做到"可见提示"需要先验证 Claude Code 的 Stop hook 是否支持回传用户可见消息（例如 `decision: "block"` + `reason` 之类的机制），这块风险和收益都不确定，列为 **Phase B**，本文档不展开，需要先有人验证可行性再决定是否做。（v2 评审对 Phase B 的重新定位见 §8.2：收件箱审核流程本身已承担"提示而非强制"的职责，Phase B 收窄为仅剩 outcome 补录引导一个场景，验证前先降级为草稿内的固定建议文案，见 §9.3。）

## 2. 摩擦信号设计

### 2.1 数据来源

Claude Code 的 Stop hook payload 里的 `transcript_path` 指向一份 JSONL 文件。当前 `crates/memvault-core/src/transcript.rs` 的 `parse_turns`/`turn_texts` 只保留 `text` 类型的内容块，`tool_use`/`tool_result` 块被显式丢弃（见 `turn_texts` 里的 `filter` 逻辑）。摩擦信号恰恰藏在这些被丢弃的块里，所以需要在**拍扁成纯文本之前**、对原始 JSONL 做一次结构化解析。

### 2.2 三类信号

新增模块 `crates/memvault-core/src/friction.rs`，暴露：

```rust
pub struct FrictionScore {
    pub score: u32,
    pub signals: Vec<(&'static str, u32)>, // 信号名 -> 命中次数，供 debug 输出
}

pub fn score(raw_transcript: &str) -> FrictionScore
```

Phase 1 只消费 `score`；`signals` 明细的多阶段用途（v2）见 §8.1 与 §9.1。

三类信号及可靠性评估：

1. **工具调用重试出错**（可靠）：同一个 `tool_use.name` 连续出现 `tool_result.is_error == true`（≥2 次连续失败记 1 次"重试摩擦"）。`is_error` 是 Claude Code transcript 的公开字段，稳定。
2. **用户拒绝工具调用**（中等可靠，需要实现前用真实样本核实措辞）：`tool_result` 内容命中一份"拒绝措辞"短语表（例如包含 "doesn't want to proceed"、"rejected" 等）。这段文案不是公开契约，跨 Claude Code 版本可能变化——短语表做成可配置项，命中不到就自然退化为 0（只会漏报，不会误报，风险可控）。
3. **用户中途纠正/打断**（启发式，precision 优先于 recall）：首轮之后出现的用户话轮，命中一份精简的中英文纠正关键词表（如"不对/不是这样/停/等一下" / "no/wait/actually/revert"）。不引入语义判断、不调用 LLM，保持 Stop hook 快速退出的现有设计约束。

三个信号的权重、命中阈值都做成常量 + 环境变量可调，先给一版合理默认值，后续按实际误报率再调整，不追求一次到位。

### 2.3 汇总打分

`score = w1 * retry_hits + w2 * rejection_hits + w3 * correction_hits`，与 `MEMVAULT_HOOK_EXTRACT_MIN_FRICTION` 阈值比较，`>=` 阈值视为"有摩擦"。

## 3. 门控点：`memvault extract` 命令

### 3.1 现状

`crates/memvault-cli/src/lib.rs`：
- `Commands::Extract` 分支里，`hook_input` 被解析为 `HookInput`，`via_hook = hook.is_some()`。
- `resolve_extract_text()`（约第 470 行）读一次 transcript 文件，直接调用 `transcript::transcript_to_text()` 拍扁成纯文本返回——`tool_use`/`tool_result` 结构在这一步就已经丢失。
- 如果 `transcript_path` 是 `-`（读 stdin），文件/流只能读一次；现在的实现里"读原始内容"和"拍扁"是耦合在一起的一次性操作。

### 3.2 改法

把"读原始 transcript 内容"这一步从 `resolve_extract_text` 里拆出来，读一次原始内容后：

1. 若 `via_hook == true` 且 `source != "text"`（即经 Claude Code JSONL 路径）：先跑 `friction::score(&raw)`。
   - 低于 `MEMVAULT_HOOK_EXTRACT_MIN_FRICTION` 阈值：打印 debug 信息（复用现有 `mv_debug`/`MEMVAULT_HOOK_DEBUG` 通道），直接 `return Ok(())`，不跑抽取、不写库——与现有"hook 无事可做就安静退出"的模式（如 `resolve_extract_text` 里 `via_hook` 分支的处理）保持一致。
   - 达到阈值：继续走原有流程。
2. 拍扁成文本（`source == "text"` 则直接传入原文，否则走 `transcript_to_text`）→ `Extractor::extract_with_coverage` → 按现状存入审核收件箱。
3. **手动调用不受门控**：显式传 `--text`/`--transcript`（非经 `--hook-input` 触发）的场景，用户已经明确要抽取，不应该被摩擦分挡住。门控只作用于 Stop hook 自动触发的路径（`via_hook == true`）。

### 3.3 新增配置项

沿用 `crates/memvault-core/src/env_file.rs` 里既有的解析写法（参考 `MEMVAULT_CORROBORATION_MIN_AGENTS` 的数值型环境变量模式，`parse_bool` 的布尔型模式）：

| 变量 | 作用 | 默认值 |
|---|---|---|
| `MEMVAULT_HOOK_EXTRACT_MIN_FRICTION` | Stop hook 触发抽取所需的最低摩擦分；设为 `0` 等价于关闭门控，退回现状"每次都抽" | 待实现时定一个非 0 的默认值 |

`plugins/memvault/hooks/session-extract.sh` **不需要改动**——门控逻辑全部在 Rust 侧（`extract` 命令内部判断），符合仓库现有约定："parsing/decision 逻辑放 Rust，shell 脚本保持无逻辑"（`transcript.rs` 顶部注释即是这条原则的出处）。

## 4. 测试计划

- `friction.rs` 单元测试：
  - 连续 `tool_result.is_error` 失败 → 命中"重试摩擦"信号，计数正确。
  - 含拒绝措辞的 `tool_result` → 命中"拒绝"信号。
  - 含纠正关键词的用户话轮（非首轮）→ 命中"纠正"信号。
  - 纯顺畅对话（无工具错误、无拒绝、无纠正词）→ 总分为 0（负向案例，防止误报）。
- `memvault-cli/src/lib.rs`（参考现有 `test_extract_from_transcript_respects_inbox_and_approve` 的写法补充）：
  - `via_hook=true` + 低摩擦 transcript → 不落库，收件箱无新增记录。
  - `via_hook=true` + 高摩擦 transcript → 落库到收件箱（`human_reviewed=false`）。
  - 非 hook 的 `--transcript` 手动调用，即使摩擦分为 0 也照常抽取（验证门控不影响手动路径）。

## 5. 文档改动

- `README.md` / `README.zh-CN.md` 环境变量表补一行 `MEMVAULT_HOOK_EXTRACT_MIN_FRICTION`。
- `docs/INSTALL.md` Stop hook 相关段落补充门控行为说明，以及如何设为 `0` 关闭门控退回旧行为。

## 6. 验证方式（实现完成后）

1. `cargo test -p memvault-core friction` 与 `cargo test -p memvault-cli` 中受影响的 extract 测试。
2. 手动构造三份样例 transcript（顺畅 / 工具重试出错 / 用户纠正），通过 `memvault extract --hook-input <file> --save` 验证：顺畅的不落库，有摩擦的落库且能在 `memvault-cli list` / Dashboard 收件箱里看到草稿。
3. `cargo clippy --all-targets` 保持零告警，符合仓库 CI 门禁；`cargo fmt --all -- --check` 通过。

## 7. 未决问题

- **拒绝措辞匹配的可靠性**：需要在实现前用真实 Claude Code transcript 样本核实当前的拒绝措辞文案。如果验证下来太不稳定/太依赖版本，考虑直接砍掉这个信号，只保留"工具重试出错" + "纠正关键词"两个更稳的信号，代价是召回率降低但不会引入误报。
- **权重与阈值的初始值**：没有历史数据支撑，第一版只能给经验值，需要上线后观察一段时间的收件箱质量（有效经验占比 vs 噪音）再调整。
- **Phase B（可见提示）是否要做**：取决于 Claude Code Stop hook 能否回传用户可见的消息，需要先验证可行性。

---

# v2 整合（以下为评审新增，Phase 1 内容不变）

## 8. 摩擦信号的多端消费全景

摩擦信号解析（`friction.rs`，§2）是共享底座：三类信号解析一次，供多个消费端取用，而不是只喂给门控判断。

### 8.1 三个消费端

```text
                    ┌─→ A. extract 门控（Phase 1，§3：低分不抽，先落地）
摩擦信号 friction.rs ┼─→ B. 摩擦证据落库 + outcome 补录建议（Phase 2，§9）
                    └─→ C. 教训链路质量侧（Phase 3，§10：复发检测 / harmful→decay / contradicts 草稿）
```

整合后与 TeamAI 闭环的对照：

| TeamAI 环节 | MemVault 现状 | 整合方案补齐 |
|---|---|---|
| 摩擦信号检测触发 | `extract` 无门控，每次会话都抽 | Phase 1：`friction.rs` 门控 |
| 可见提示（人在环上） | Stop hook 输出被 host 忽略，无 UI 通道 | 见 §8.2：收件箱本身就是等价物 |
| 总结与入库 | `extract` → 收件箱（已有） | Phase 2：FrictionEvidence 进草稿 metadata |
| 防止再犯 | `record_outcome` 失败蒸馏教训（已有），但依赖 Agent 主动上报 | Phase 2：outcome 补录建议 |
| 经验复用 | 同 `task_type` 教训自动注入（已有） | — |
| 复发检测 / 经验更新 | escalation 按同类失败计数，粒度粗 | Phase 3：复发语义匹配细化 escalation |
| 治理（digest/maintenance） | `promote`/`decay`/`dedup`/`supersede`（已有） | Phase 3：harmful→decay、contradicts→更新草稿 |

其中 MemVault 有两块 TeamAI 没有的能力：`compliance.rs`（MUST 记忆遵循率）与 `effectiveness.rs`（注入记忆的 useful/harmful 裁决）——Phase 3 正是把后者从"只记录"接入治理链路。

### 8.2 "提示而非强制"在 MemVault 的等价物（Phase B 重新定位）

TeamAI 达标后给可见提示、建议手动运行分享命令，是因为它的入库目标是团队共享仓库——自动提交会影响他人。MemVault 的收件箱是本地未审核草稿（`human_reviewed=false`），自动落库零风险：不进正式库、不参与注入，直到人工 `review_memory`。因此 **Phase 1 的"达标静默抽取 → 落收件箱"本身就是"提示而非强制"的等价物**——"提示"发生在 review 队列里而非会话里，人工审核即许可。

由此 Phase B 的价值收窄为一个场景：引导用户对疑似失败任务补录 `memvault outcome --status failure`（教训链路的入口，Agent 忘报即漏）。在 Claude Code 验证出可行的可见消息通道之前，这个引导先降级为草稿里的一条固定建议文案（§9.3）。

## 9. Phase 2：摩擦证据落库 + outcome 补录建议（已实现）

Phase 2 挂在 Phase 1 已有的门控分支里（`via_hook == true && score >= 阈值`）。**与原草案不同：确认无法"不加列"后，实际是加了一个可空列**，见下。

### 9.1 evidence_note（实际实现，未采用原设想的 FrictionEvidence 结构）

原草案设想一个 `FrictionEvidence { score, signals: BTreeMap, summaries: Vec<String> }` 结构体，序列化后存进草稿。实现时简化为一个函数，直接产出人类可读的单行文本——反正唯一的消费者是审草稿的人，不是另一个程序，JSON 结构没有实际收益：

```rust
// crates/memvault-core/src/friction.rs
pub fn evidence_note(friction: &FrictionScore) -> String {
    // "Friction score 3 (retry×2, rejection×0, correction×1). If this task
    //  actually failed, consider recording the lesson: `memvault outcome
    //  --status failure --task <task> --cause <cause>`."
}
```

`FrictionScore.signals`（Phase 1 起就有的 `Vec<(&'static str, u32)>`）已经足够渲染这行文本，不需要额外的 `BTreeMap`/`summaries` 包装。

### 9.2 草稿存储：新增 `friction_evidence` 可空列

探查 `Memory` 结构体（`crates/memvault-core/src/models.rs`）发现：没有任何空闲的文本字段可复用——`instruction` 已被抽取器本身和 dashboard 编辑 UI 占用；`content` 是唯一保证在收件箱/dashboard 各处都会展示的字段，但把证据文本塞进 `content` 会污染记忆本身，一旦这条草稿被审核通过、晋升，证据文本会跟着被注入到未来所有引用它的会话里——不可接受的噪音。

所以 §1 的"不加表不加列"这条前提在 Phase 2 站不住，选择加一列：

- `storage/sqlite.rs`：migration 20，`ALTER TABLE memories ADD COLUMN friction_evidence TEXT`（可空，与 `occurred_at`/`skill_meta`/`source_trace_ids` 等历史上 17 次加列同一模式，`reconcile_legacy_schema` 已按 migration SQL 自动识别新列，无需特殊处理）。`row_to_memory`/`save`/`update`/`save_with_embedding` 均已加上这一列的读写。
- `models.rs`：`Memory.friction_evidence: Option<String>`，`#[serde(default, skip_serializing_if = "Option::is_none")]`，`Memory::new` 默认 `None`。
- `memvault-cli/src/lib.rs`：`resolve_extract_text` 原先只返回拍扁后的文本，改造为返回 `ResolvedExtract { text, friction: Option<FrictionScore> }`，让门控算出的 `FrictionScore` 能带出门控点、供 `Commands::Extract` 的保存循环设置 `mem.friction_evidence = friction.as_ref().map(friction::evidence_note)`。同时 `memvault review`（无参数的待审列表）打印时，若某条草稿有 `friction_evidence` 就额外打一行 `⚠ <note>`。
- 可见性：`memvault-mcp/src/server.rs::list_inbox` 的 JSON 输出、`rest_api.rs::memory_to_json`（REST 层唯一的 Memory→JSON 转换函数，dashboard 走这条路）都加上了 `friction_evidence` 字段；dashboard `api.ts::MemoryView` 加了对应可选字段，`App.tsx` 详情视图里紧邻 `skill_meta` 复用现有 `.detail-field` 样式展示。

价值：审草稿的人在收件箱/dashboard 详情页/`memvault review` 都能直接看到"为什么这个会话值得抽"（几次重试、拒绝过什么工具），不必回看 transcript；Phase 3 的复发检测也可以此为准入证据（届时视需要再决定是否需要更结构化的形式）。

### 9.3 outcome 补录建议（已实现，内嵌在 evidence_note 里，未做成独立提示）

教训链路（`record_outcome` → `reflection.rs`）的入口依赖 Agent 主动上报，Agent 忘报即漏——这是整条教训闭环最脆弱的一环，摩擦信号恰好是它的兜底信号。实现比原草案更简单：不是"证据文本 + 单独一条建议"两段式，而是 `evidence_note` 一次性生成的单行文本里已经内嵌了这句建议（见 §9.1 的示例输出）。

- **未做**"该 session 是否已有对应 outcome"的自动判断——session 与 episode 无外键关联，匹配逻辑不可靠，引入它得不偿失。建议一律附上，由审草稿的人判断。
- Phase B（会话内可见提示）验证通过之前，这条建议就停留在草稿文本里，符合 §1 非目标的结论——未额外做任何 Phase B 相关改动。

## 10. Phase 3：教训链路质量侧（已实现，与摩擦门控零冲突，三条并行推进）

Phase 3 不依赖摩擦信号，是教训库质量侧的三条改进，对应"防止再犯"的验证闭环与"适时重新提取"。**三条实现方式都与原草案有实质差异**，见各小节。

### 10.1 复发匹配细化 escalation（已实现，非语义检索）

现状（修复前）：`reflection.rs` 的 `LESSON_ESCALATION_THRESHOLD = 2` 按"同 task_type 失败次数"计数，粒度粗——同类任务失败可能是新坑，不代表旧教训失效。旧测试 `test_escalation_hint_after_repeated_failures` 明确写了两次失败即使 cause 文本不同（"cause 1" vs "cause 2"）也会升级，这正是待修的粗粒度行为。

**实际实现没有用"语义检索/复用现有检索索引"**（原草案设想），而是复用了 `crates/memvault-core/src/dedup.rs::Deduplicator` 已有的**词重叠（Jaccard）相似度**——`dedup.rs` 本来就用它判断"两条记忆是不是近重复"，这里直接拿来判断"两次失败的 cause 是不是同一个问题"，不需要 embedding、不需要 LLM 调用：

- 新增 `reflection.rs::is_recurrence(new_cause, past_cause) -> bool`，阈值常量 `RECURRENCE_SIMILARITY_THRESHOLD = 0.3`（比 dedup 自己的近重复阈值 0.7 低很多——失败原因的表述变体比记忆内容的近重复要大得多，第一版经验值，待真实数据校准）。
- escalation 逐条比较该 task_type 下"已有教训的历史失败"的 `cause` 文本与新失败的 `cause`，只有**真正相似的**才计入 `recurrence_count`；`recurrence_count >= LESSON_ESCALATION_THRESHOLD` 才触发 escalation_hint，同时记录命中的 `best_match`（供 10.3 使用）。
- **没有引入任何新列/新关系来存复发计数**——直接在内存里对 `list_episodes` 已经查出来的 `EpisodeRecord.cause`/`EpisodeRecord.lesson` 做比较，§12 曾经担心的"复发计数存储位置"问题在实现后发现根本不需要解决：`EpisodeRecord.lesson` 本身就是教训原文（不是指向 Memory 的引用），复发匹配和取用旧教训文本都不用额外查库。
- 旧测试保留未改（"cause 1"/"cause 2" 经分词器过滤掉末尾数字后变成同一个 token，恰好仍是"同一问题"的有效样例）；新增 `test_unrelated_causes_of_same_task_type_do_not_escalate`（证明修复：不相关 cause 不再误升级）与 `test_recurrence_escalates_and_proposes_lesson_update`（证明复发命中）。

### 10.2 harmful 裁决接入 decay（已实现，只降衰减速度，明确不碰 human_reviewed/confidence）

现状（修复前）：`effectiveness.rs` 已产出注入记忆的 useful / harmful / neutral 裁决，但只记录，不影响生命周期。

**与原草案"进 review inbox（标记待更新）"不同**：评审时确认，让 LLM 的 best-effort "harmful" 裁决去撤销一条人类已经审核通过的记忆（`human_reviewed: true → false`），是代码库里从未有过的操作，属于自动撤销人的决定——按用户明确选择，**只做衰减加速，绝不触碰 `human_reviewed` 或 `confidence` 字段**：

- `DecayConfig` 新增 `harmful_multiplier`（默认 `2.0`）与 `harmful_min_count`（默认 `2`，一次误判不足以移动衰减速度），组合进现有衰减速率公式，机制与已有的 `contradiction_multiplier` 完全一致——不是新发明的信号处理路径。
- **跨存储依赖，比原草案设想的"几乎纯连线"更重**：harmful 裁决存在 `ComplianceStore`（`compliance.rs`，独立 SQLite 连接/表 `compliance_events`），`DecayManager` 之前只持有 `MemoryStore`。新增 `ComplianceStore::harmful_count_for_memory(memory_id, since)` 查询，`DecayManager::with_compliance(Arc<ComplianceStore>)` 构建器方法（而非改 `new()` 签名，避免动 decay.rs 里已有的 ~15 处测试构造点）；未调用 `with_compliance` 时行为与改动前完全一致。
- 已在 CLI `memvault decay`、MCP `run_decay` 工具、REST `run_decay` 端点、proxy `run_decay` 工具这 4 个生产调用点接好，沿用仓库里"`.ok()` 优雅降级"的既有惯例。
- **依赖 LLM judge**（§12 已记录）：没配 LLM 时 harmful 裁决从不产生，这条链路自然是空的，不会报错也不会误加速。

### 10.3 contradicts → 更新草稿（已实现，依赖 10.1 的 best_match，不自动 supersede）

依赖 10.1：不是"复发计数达标后另起一次检索"，而是直接复用 10.1 escalation 时已经找到的 `best_match`（最相似的历史失败episode）：

- escalation 命中且 LLM 已配置时，`reflection.rs::propose_lesson_update` 用 `LlmExtractor::json_chat`（已有能力，`OpenAiChatExtractor` 有真实实现，非默认空实现）把旧教训原文 + 新失败的 task/cause 一起给 LLM，让它判断旧教训是否需要更新；回复 `NONE` 或为空则不生成草稿（best-effort，绝不编造，与 `effectiveness.rs` 的既有契约一致）。
- 有更新建议时，`save_recurrence_draft` 存一条新的未审核草稿（`human_reviewed=false`，tags 加 `lesson-update`），并调用 `add_evidence(..., EvidenceKind::Contradicts, Some(old_lesson_memory_id), ...)` 建立草稿→旧教训的 `contradicts` 关系。
- **不自动 `supersede`**——草稿只是建议，人工审核后仍需手动跑 `memvault supersede`，符合原设计"人工确认后走 supersede"的意图，没有扩大自动化范围。
- `LessonRecord` 新增 `recurrence_update: Option<RecurrenceUpdate>` 字段，CLI `memvault outcome`、MCP `record_outcome` 工具、REST 对应端点的输出都同步带上（与 `escalation_hint` 待遇一致）。

### 10.4 收件箱关系可见性（新增第四项，原草案 §10.3 隐含依赖但未单独列出）

探查发现一个原草案没意识到的缺口：生成的 `contradicts` 关系，在人工审核路径上其实**看不到**——`memvault review` 的待审列表和 MCP `list_inbox` 都只打印 content/priority/tags 等固定字段，从不查关系；只有搜索的 `expand_relations`（默认关闭）才会带上关系，且那是搜索场景不是审核场景。为了让 10.3 生成的证据真正对审核者可见，补了这一项：

- CLI `memvault review`（待审列表）：每条草稿后面追加打印其关系（复用已有的 `relations::collect_relations`/`relation_line`，为 search 的 `expand_relations` 而写的同一套函数）。
- MCP `list_inbox`：JSON 输出新增 `relations` 字段，同样复用这两个函数。
- **Dashboard 收件箱详情页的关系展示未做**——`expand_relations` 目前只在搜索结果里有对应 UI（`App.tsx` 的 `relations-list`），要在收件箱详情页加同样的展示需要额外的 `App.tsx` UI 工作，不是复用现有函数就能做到的，所以本轮明确不做，记录为后续项而非静默丢弃。

## 11. 整合后的落地顺序（已完成）

| # | 内容 | 依赖 | 状态 |
|---|---|---|---|
| 1 | `friction.rs` + extract 门控（Phase 1，§2–§3） | 无 | 已实现 |
| 2 | friction_evidence 列 + outcome 补录建议（Phase 2，§9） | 1 | 已实现 |
| 3 | 复发匹配细化 escalation（§10.1） | 无 | 已实现 |
| 4 | harmful verdict → decay（§10.2） | 无 | 已实现 |
| 5 | contradicts → 更新草稿（§10.3） | 3 | 已实现 |
| 6 | 收件箱关系可见性（§10.4） | 5（价值上依赖，代码上独立） | 已实现 |

## 12. 未决问题

Phase 1 的未决问题见 §7，继续有效。以下是 Phase 2/3 评审与实现过程中产生的记录：

- ~~FrictionEvidence.summaries 的边界~~：**已解决（简化）**。`evidence_note` 直接从 `signals` 渲染信号名+计数，不含任何对话原文，零脱敏负担。
- ~~草稿 metadata 的落点字段~~：**已解决**。加了 `friction_evidence` 可空列（§9.2）。
- ~~Phase 3 复发计数的存储位置~~：**已解决（简化）**。不需要任何存储——`EpisodeRecord.cause`/`.lesson` 已经是原文，10.1 直接在内存里比较，没有引入新列或新关系（§10.1）。
- **Phase B 的收窄**：唯一剩余场景是"outcome 补录的会话内提示"（§9.3）。若验证下来 Claude Code 无法回传用户可见消息，该场景长期停留在草稿建议文案形态，可接受。
- ~~§10.2 harmful→decay 依赖 LLM judge~~：**已确认并保留为已知限制**。没配 LLM 时 `harmful_flagged` 恒为 0，`run_decay` 不报错，只是这条信号不产生（`test_harmful_verdicts_without_compliance_attached_are_ignored` 覆盖了"未挂 compliance"的路径；"挂了 compliance 但没配 LLM"的路径行为等价，未单独测试，因为链路上游 `judge_recent_injections` 本身已经覆盖了这一点）。
- **RECURRENCE_SIMILARITY_THRESHOLD=0.3 是拍的经验值**：没有真实数据支撑，需要上线后观察 escalation/更新草稿的命中质量再调整，和 Phase 1 的摩擦权重一样的处境。
- **Dashboard 收件箱关系展示未做**（§10.4）：需要真正的 `App.tsx` UI 工作，本轮明确跳过，不是遗漏。
