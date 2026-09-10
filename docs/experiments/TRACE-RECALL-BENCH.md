# TRACE-RECALL-BENCH — handoff vs. compaction vs. recall cost

> **状态**：协议已落地，结果表为模板（待填）。离线可测部分（通道 3 的摄入与注入尺寸）已由 `crates/memvault-core/benches/trace_recall_cost.rs` 实现；通道 1–2 与「每成功任务」判定需要真实 LLM + judge，尚未自动化。
>
> **对应目标**：trace 摄入的成本/价值基准（G4——证明召回既有记忆不贵于重读会话）。
> **参考**：funes handoff-vs-recall benchmark — <https://huggingface.co/datasets/dacorvo/funes-handoff-recall-benchmark> · <https://huggingface.co/blog/funes>

## 假设（Hypothesis）

**H8**：在「答案依赖上一会话先验知识」的任务上，**召回（recall）通道的每成功任务加权 token ≤ 手写交接（handoff）通道**，且 ≤ 压缩续跑（compaction）通道；即 MemVault 的 `session_start` 注入 + `get_memory_evidence` 展开，比让 Agent/人类重写一遍交接文档、或压缩长会话续跑更省 token。

funes 报告 recall 比手写交接便宜 4–8 倍；本基准**不预设必达**，如实报告三通道结果，失败项照实保留。

## 二任务构造规则（Construction rule）

构造 **2 条**任务，每条必须满足：

1. **答案依赖上一会话的先验知识**——存在一个「不可凭空猜出」的项目专属事实（如内部 CDN 域名、必填 header、迁移回填步骤、私有环境变量名）。没有先验知识的模型无法猜出，因此控制组必然失败。
2. **单一客观判据**——任务答案中是否出现预注册的**唯一专名词干**（出现在计划/回答/产物中 = 成功）。这沿用 H5/H7 的主判定口径：客观关键词命中优先于 LLM 裁判。
3. **两条任务共享同一段先验会话历史**——这样三通道「载入信息」的成本可直接对比，而任务难度一致。
4. **成功可判定为二元**——不接受「部分成功」；judge 只用于通道 1–2 生成交接/压缩文本时的**参考**，主判定仍是专名命中。

参考 H5/H7 已有的场景库（`verify_hypotheses.py` / `verify_h7.py` 的 deploy / migrate / upgrade 坑），每条任务绑定一个专属事实。

## 三通道与测量步骤

每个通道在同一组 2 条任务上各跑 **N 轮**（建议 N ≥ 5），记录每轮的**加权 token**与**是否成功**。

### 通道 1 — 手写交接（handoff）

让一个 LLM（等同于「上一会话的 Agent」）阅读先验会话历史，产出一份交接文档；再让一个**全新会话的** LLM 仅凭该交接文档完成任务。

测量步骤：
1. 记录交接文档生成的 `prompt_tokens + completion_tokens`（若交接由人类撰写，按同等信息量的文档 token 计，并在结果表备注）。
2. 新会话：记录 `handoff_tokens + task_prompt_tokens + completion_tokens`。
3. 按二任务判据判定成功与否。

**该通道的 token 由 LLM provider 的 usage 字段给出**（生成 + 消费两段都要计入——交接不是免费的）。

### 通道 2 — 压缩续跑（compaction）

在同一个长会话里先做先验任务，触发压缩，然后继续做目标任务。

测量步骤：
1. 记录压缩前的会话 token、压缩调用本身的 token、以及压缩后续跑的 token。
2. 由于压缩会「压平」发现（funes），记录压缩后目标任务的失败情况——这正是该通道成本之外的价值信号。
3. 判定成功与否。

### 通道 3 — 召回（recall）— **可离线测量**

新会话启动时由 `session_start` 注入既有记忆；任务中若需依据，Agent 调用 `get_memory_evidence(memory_id)` 展开原始证据块（计划 §7 的只读工具）。

测量分两层：

**3a. 离线可测量（已实现，无需 LLM）** — `cargo bench -p memvault-core --bench trace_recall_cost`：
- 合成转录（`benches/trace_recall_cost.rs` 的 `SIGNAL_TURNS` + `FILLER_TURNS`，40 turn、4 条含可提取信号）→ `ingest_for_agent` 摄入；
- 报告 **ingest 吞吐**（turns/session）；
- 报告**注入载荷尺寸 vs 全转录尺寸**（bytes 与 tokens 的压缩比）。

**3b. 端到端（需 LLM，同 3a 的判据）** — 驱动真实 `memvault-mcp` 子进程：
1. 先验会话的产物（L0 evidence + 蒸馏候选）已在库中；
2. 新会话：记录 `session_start` 返回注入文本的 token；
3. 记录任务过程中 `get_memory_evidence` 展开的 token（仅在实际调用时计入）；
4. 统计 = `注入 tokens + 展开 tokens + task/completion tokens`；
5. 判定成功与否。

## Tokenizer / 记账口径

- **统一口径**：所有通道的「加权 token」= 该通道为一个任务**总共**注入/生成的文本 token 之和，成功任务按成功轮平均；失败轮也计入分母（失败任务同样烧 token）。
- **通道 1–2**：直接用 LLM provider 返回的 `usage.prompt_tokens` / `usage.completion_tokens`（真实 tokenizer，优先）。
- **通道 3a（离线）**：使用仓库既有助手 `MemoryRouter::estimate_tokens`（`crates/memvault-core/src/router/format.rs`）：ASCII ≈ 4 字符/token、CJK ≈ 1.5 字符/token；对 ASCII 文本即 `bytes / 4`，与 funes 的粗估一致。**不要**在 bench 里另造估算器。
- **通道 3b**：若走真实模型，用 provider usage（真实 tokenizer）；离线部分仍用 `estimate_tokens` 交叉核对。
- 权重：本基准不做优先级加权（每条注入记忆计 1 份 token），与 funes 的「weighted tokens per successful task」保持可解释的最小定义。

## 已知局限（Known limitation）

- **通道 1–2 无法离线自动化**：产出交接文档、执行压缩都需要真实 LLM（且压缩行为随 host 而异），因此这两个通道的数字**必须由外部脚本 + LLM provider usage 填充**，不能由 Rust bench 生成。
- **通道 3a 只测成本，不测成功**：离线部分能证明「注入载荷 ≪ 全转录」，但不能证明任务成功；成功判定需要 3b 或通道 1–2 的 LLM 环节。
- 单机、合成转录；真实会话更长、更杂，压缩比会变。
- js/人类交接的 token 估算口径可能与 provider usage 有偏差，需在结果表备注来源。
- **不得伪造数字**：本表在拿到真实运行结果前保持为空模板。

## 结果表（模板 — 待填）

占位命令（离线部分，自动打印通道 3a 的尺寸比；把输出粘到下表）：

```bash
cargo bench -p memvault-core --bench trace_recall_cost 2>&1 | tee /tmp/trace_recall_cost.txt
```

占位命令（端到端，**尚未实现**，需要 LLM 端点；实现后按此形状调用）：

```bash
# TODO: docs/experiments/verify_trace_recall.py 尚未实现 —— 需要 provider usage 才能填 1/2/3b
export VERIFY_BASE_URL=http://127.0.0.1:11434/v1
export VERIFY_MODEL=qwen2.5:1.5b-instruct
python docs/experiments/verify_trace_recall.py --channels handoff,compaction,recall --rounds 5
```

### 每成功任务加权 token

| 任务 | 通道 | 加权 tokens / 成功任务 | 成功率 | 备注 |
|------|------|------------------------|--------|------|
| Task 1 | handoff | _待填_ | _待填_ | provider usage |
| Task 1 | compaction | _待填_ | _待填_ | provider usage |
| Task 1 | recall | _待填_ | _待填_ | 3b；离线比见下 |
| Task 2 | handoff | _待填_ | _待填_ | |
| Task 2 | compaction | _待填_ | _待填_ | |
| Task 2 | recall | _待填_ | _待填_ | |

### 通道 3a 离线尺寸比（本次 bench 输出）

| 指标 | 值 | 来源 |
|------|----|------|
| 合成会话 turn 数 | 40（4 条含信号） | `trace_recall_cost.rs` |
| ingest 吞吐 | _待填_ turns/session | `trace_ingest_synthetic_session` |
| 注入载荷 bytes / tokens | _待填_ | `report_payload_ratio` 的 `[trace-recall] recall payload` |
| 全转录 bytes / tokens | _待填_ | `[trace-recall] full transcript` |
| 压缩比（full / recall） | _待填_x bytes / _待填_x tokens | `[trace-recall] compression ratio` |

> 示例（本机一次运行，仅作对照，重跑以实际输出为准）：40 turn 合成会话 → ingest 3 evidence / 3 candidates；注入载荷 189 B / 47 tokens vs 全转录 1747 B / 437 tokens → **9.24x bytes / 9.30x tokens**。比值随会话长度增长，小样本不代表线上。

### 结论

| 假设 | 预期 | 实测 | 结论 |
|------|------|------|------|
| H8: recall ≤ handoff（每成功任务 token） | ≤ | _待填_ | _待填_ |
