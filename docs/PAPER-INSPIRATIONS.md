# 启发来源：Qwen3.8-Flash-Next 技术报告的记忆架构对 MemVault 的启发

> **来源**：[QwenLM/Qwen3.8-Flash-Next tech_report.pdf](https://github.com/QwenLM/Qwen3.8-Flash-Next/blob/main/tech_report.pdf)，Qwen Team，2026-08-26
> **原文一句话**：一份 125B-A6B 稀疏 MoE 模型的架构报告，没有专门的 "agent 记忆" 章节，但它的三个核心组件——GDN 压缩态记忆 + 周期性全注意力（§2.1.1）、QSA 两级稀疏检索（§2.1.2）、N-gram 条件记忆（§2.3，论文自称 "conditional memory"）——本质上都在回答"如何给一个系统设计和评测记忆"，与 MemVault 做的事情同构。
> **本文目的**：记录报告的记忆相关章节与 MemVault 现有设计的对照分析，说明哪些点值得吸收、哪些已经被 MemVault 部分覆盖、哪些不适用，并给出对应的落地改动。

## 对照表

| 论文的做法 | MemVault 现状 | 结论 | 落地 |
|---|---|---|---|
| GDN delta rule：写入前先估计该 key 已关联的值，**只写残差**；"repeated or similar keys update an existing association instead of accumulating unbounded outer products" | `dedup.rs` 已有 Skip/Merge/Conflict 三种动作，但只作为批量命令 `memvault dedup` 存在，`save` 写入路径默认仍是追加 | **采用** | Feature A |
| 衰减门 α_t 是**输入依赖**的：每条内容的寿命由内容本身决定，不是全局统一衰减率 | `decay.rs` 已有 `access_boost` 和 `contradiction_multiplier`（evidence-driven forgetting），且已有按 `MemoryType` 区分衰减速率的机制 | **已大部分覆盖** | 无需改动，见"明确不做" |
| Hybrid 结论："any finite-state memory cannot reproduce direct token-level retrieval exactly"——压缩态不能替代逐字检索，两者缺一不可 | `session_start` 按 `token_budget`/`max_memories` 注入 top-N 完整记忆，本质是"纯全注意力"策略，没有常驻的压缩摘要轨 | **采用（设计层）** | Feature G（双轨注入，列入远期） |
| QSA 两级检索：轻量 indexer 在 micro-block 粒度粗排 → top-k 选块 → 展开做精细注意力；indexing 成本随长度次线性 | `hybrid.rs` 的 RRF 融合是单程的：关键词 + 向量召回后直接截断到 `max_memories`，无重排 | **采用（分两步）** | Feature E（粗排+预算内重排，记忆规模上万后再启用） |
| QSA indexer 用全注意力分布**蒸馏**训练："廉价检索器向昂贵但正确的检索器学习" | `evidence.rs`/`promote.rs`/`outcome` 已在收集真实使用信号（访问、成败结果），但没有回流到检索排序 | **采用** | Feature B 的延伸（使用信号回流排序权重），见"后续方向" |
| N-gram embedding：检索以**局部上下文**为条件，而非单个符号的身份；仅此改动即全基准提升 | `session-start --context` 接收单个查询串；`query_expand.rs` 有查询扩展，但条件仍是一句话 | **采用** | Feature D |
| 确定性寻址 → 记忆表放主机内存、**异步预取**，与第一层计算重叠（放在 Layer 2 就是为了让预取重叠） | proxy 注入目前是同步阻塞：完整检索完成后才注入 | **采用** | Feature C |
| 单层记忆足够：把同样参数预算分散到多层**没有稳定收益**（Table 7） | MemVault 有三条注入通路：MCP `session_start`、proxy 拦截、`sync` 生成的指令文件，同一记忆可能经多条通路重复注入 | **采用** | Feature F |
| 记忆容量扩大时 **loss 单调下降但下游任务性能饱和**（Table 9）；固定预算下记忆与计算是不同角色，不能互相替代（Table 8） | 产品指标偏重"存了多少/召回了多少"，缺少"注入后 Agent 任务是否更成功"的度量；`outcome` 机制已有数据基础但没有形成评测闭环 | **采用** | Feature B |
| 方法论：每个改动沿**质量 / 成本 / 稳定性**三轴评测，并诚实报告负面结果（各种 n-gram 压缩技巧均无收益） | `docs/experiments/` 已有假设验证归档 | **借鉴** | 本文档及后续实验记录沿用该格式 |

## 关键洞察

### 为什么"检索召回率"不是 MemVault 应该追求的北极星指标

论文最反直觉的发现是 Table 9：n-gram 记忆词表从 20V 扩到 200V，训练 loss 单调下降，但下游基准**饱和甚至波动**；知识类基准（尤其中文 C-Eval/CMMLU）提升最稳定，推理类收益最小。

翻译到记忆产品的语境：**"检索召回率 ≠ Agent 任务成功率"**。召回率是论文的 loss——它证明系统"学到了"，但不证明"用上了、用对了"。竞品普遍在报 LongMemEval R@5 这类检索指标，而 MemVault 已经有 `outcome` 命令记录任务成败，具备做**任务级评测**的数据基础。这是差异化机会（Feature B）。

同一条结论的第二层含义：**"存得更多"不是产品指标**。记忆库无限增长只会让"存了但找不到"（断裂 1）更严重。固定注入预算（`token_budget` 对应论文的"参数预算"）+ 积极整合/归档，让记忆库总规模稳定，才是健康状态。Feature A 的 delta 写入正是服务于这个目标。

### 压缩与逐字缺一不可

§2.1.1 的消融（Table 1）显示：纯压缩（SWA 类局部方案）和纯全局（全注意力）都不如 hybrid。对注入策略的含义：既不能只做"常驻摘要"（会丢失细节），也不能只做"按需检索"（背景知识永远在场才能保证一致性）。当前 MemVault 偏后者，远期应补压缩轨（Feature G，见"明确不做"中的排期说明）。

## 落地功能

### Feature A — save 时 delta 写入（优先级 ★★★）

`dedup.rs` 的相似检索 + Merge 逻辑前移到写入路径：`save` 时（嵌入可用的前提下）先对候选做 top-k 相似检索，相似度超过阈值走 **Merge**——新信息并入已有记忆、刷新 `updated_at` 与强度，而不是 insert 新条目；中等相似度按现有规则走 Conflict 标记或 Skip。对外暴露 `--force`（CLI）/ `force_insert`（MCP/REST）开关保留强制新建的能力，默认行为从"追加"变为"先查再写"。

对应论文 §2.1.1："repeated or similar keys update an existing association instead of accumulating unbounded outer products"。纯追加式记忆库等价于论文否定的"无界外积累加性记忆"。

### Feature B — 任务级记忆效果评测基准（优先级 ★★★）

新增 `crates/memvault-core/src/bench/`（或独立 `memvault-bench` crate）：以 `outcome` 记录为数据源，度量"开/关记忆注入下 Agent 任务成功率"。v1 范围：

1. **数据集**：从已有 `outcome` + `evidence` 记录构造任务样本（任务描述 → 期望召回的记忆 → 任务成败）；
2. **两种模式**：`with-injection`（走完整 `session_start` 注入）与 `without-injection`（同任务裸跑），对同一 LLM 评测器跑批；
3. **报告指标**：任务成功率差值（主指标）、注入记忆被实际引用率、注入成本（token 数）——对应论文的三轴方法论（质量/成本/稳定性）。

对应论文 §2.3.2：记忆的价值必须由下游任务证明，而不是由检索指标证明。

### Feature C — proxy 确定性快速路径 + 异步预取（优先级 ★★）

`memvault-proxy` 的注入分两相：

1. **确定性相**（同步、零 embedding 调用）：MUST 级规则、namespace 精确匹配、`sync` 产物等**可纯规则判定**的内容先行注入；
2. **预取相**（异步）：语义检索在后台进行，赶在请求转发给上游（首次 LLM 调用）之前把结果补入上下文；若预取未在窗口内完成，则只带确定性部分放行，绝不阻塞请求。

对应论文 §2.3：n-gram 表能放主机内存并异步预取，前提是**寻址确定性**；放在浅层是为了让预取与第一层计算重叠。

### Feature D — 会话 n-gram 作为检索条件（优先级 ★★）

检索 key 从"当前一句话"升级为**最近 n 轮对话 + agent-id + namespace** 拼成的上下文条件（conversation n-gram）。proxy 模式天然持有完整请求流，取最近若干条 user 消息即可；`session-start` 模式允许 `--context` 传入多行文本并改进 `query_expand.rs` 的切分/加权策略。

对应论文 §2.3："conditioning memory retrieval on local context rather than token identity alone"——单符号查表升级为 n-gram 条件查表，是论文中单项收益最明确的改动之一。论文另发现中文知识类任务随记忆容量提升最稳定，与 MemVault 的主力使用场景（中文 + `bge-small-zh`）吻合。

### Feature E — 预算内两级检索（优先级 ★，规模触发）

记忆条数上万后启用：先按 namespace/项目/主题聚簇打粗分（对应 QSA 的 micro-block 压缩 indexer），选中 top 簇再展开到单条记忆，最后在 `token_budget` 内做一次精排（cross-encoder 或 LLM rerank），替换当前"单程召回 + 截断"。

对应论文 §2.1.2。**当前记忆规模下不做**——这是扩展性保险，不是当前瓶颈。

### Feature F — 注入通路去重：每个 agent 一条规范路径（优先级 ★）

`agents.yaml` 增加 `inject_channel` 字段（`mcp` / `proxy` / `sync`，缺省 `mcp`）。`session_start`、proxy 注入、`sync` 生成在装配前先查该 agent 的规范通路，非规范通路跳过注入（`sync` 仍生成文件但标注"此 agent 由 MCP 通路注入，本文件仅作备份"）。同时给 `session_start` 输出加幂等标记，防止同一会话多次调用导致重复注入。

对应论文 Table 7："Distributing the same parameter budget across multiple layers yields no consistent benefit"——同一份记忆预算分散到多条注入通路不会叠加收益，只会引入重复与冲突。

## 明确不做的事

- **不引入"压缩摘要轨"（Feature G）于本期**：双轨注入（常驻状态摘要 + 按需逐字检索）是对的方向，但涉及新增记忆形态（聚合摘要的生成与维护管道），超出本轮范围，记录为远期规划（同步更新 `docs/DESIGN.md` §16 远期规划）。
- **不改动 `decay.rs` 的衰减模型**：论文的"数据依赖衰减门"思想已被 `contradiction_multiplier` + 类型稳定性系数 + `access_boost` 覆盖，再加"预测式半衰期"属于过度设计，等真实使用数据积累后再评估。
- **不蒸馏/训练专用排序模型**（QSA 蒸馏思想的完整形态）：v1 只做使用信号统计回流（调整 `keyword_weight`/`vector_weight`），训练独立模型需要数据量支撑，留作观察项。
- **不追求"更多记忆"**：记忆条数增长不作为任何指标；`dedup` + `decay` + 归档维持库规模稳定是既定策略。
- **不做 n-gram 式记忆压缩**（把记忆内容压缩以省预算）：论文明确报告这类技巧（token normalization、non-uniform allocation、frequency-based partitioning）在其配方下无稳定收益；MemVault 在验证收益前不投入。

## 实施优先级

| 优先级 | 功能 | 依据章节 | 状态（2026-09-03） |
|---|---|---|---|
| ★★★ | Feature A — save 时 delta 写入 | §2.1.1 | ✅ 已实现（`writer` 模块 + 三通路接入） |
| ★★★ | Feature B — 任务级评测基准 | §2.3.2 | ✅ 已实现（`bench` 模块 + `memvault bench`） |
| ★★ | Feature C — proxy 快速路径 + 异步预取 | §2.3 | ✅ 已实现（两阶段注入 + `wait_full`） |
| ★★ | Feature D — 会话 n-gram 检索条件 | §2.3 | ✅ 已实现（`conversation_ngram` + `weight_turns_by_recency`） |
| ★ | Feature E — 两级检索（规模触发，暂不实施） | §2.1.2 | ⏸ 暂缓（规模未达触发条件） |
| ★ | Feature F — 注入通路去重 | Table 7 | ✅ 已实现（`InjectChannel` + 通路判定） |

> 落地时顺带修复了一个存量 bug：`SessionContext::get_project()` 返回的命名空间已带
> `project:` 前缀，而 `session_start` 等又叠加一层，产生 `project:project:*` 畸形命名空间；
> 现由 `router::project_namespace()` 统一归一化（见 Feature C 条目）。

## 方法论备忘

论文对每个候选改动都沿三轴评测：**质量**（loss + 下游基准）、**成本**（训练 / prefill / decode）、**稳定性**（超参与训练鲁棒性），并且诚实记录负面结果。MemVault 后续实验记录（`docs/experiments/`）沿用此格式：每个改动回答三个问题——任务成功率变了吗？注入/检索成本变了吗？换个配置还成立吗？
