# 借鉴 openTrinity/mycontext 的改进计划

> **来源**：对比分析 `docs/mycontext`（openTrinity/mycontext，TypeScript/Electron monorepo + Python 图库 kl-graph，Elastic License 2.0）与 MemVault 自身架构后整理。
> **定位**：mycontext 是「个人工作上下文采集平台」（IM/文档/会议 → 图谱 → 数字分身），MemVault 是「跨 Agent 共享记忆路由器」。两者数据源与产品形态不同，但**检索引擎、SQLite 存储工程、注入/发送的安全闸**这三块解决的是同一类问题，mycontext 在这三块上有大量带实测数据的工程沉淀，可独立落地点多。
> **许可警示**：mycontext 采用 **Elastic License 2.0**（源码可用，非开源许可），MemVault 是 MIT。本文所有条目只借鉴**设计**，不得移植其代码——抄代码会污染 MemVault 的许可。
> **日期**：2026-08-19

---

## 背景

| | mycontext | MemVault |
|---|---|---|
| 数据来源 | 主动采集 IM/文档/会议等工作系统（channels + ingest） | 用户/Agent 显式写入（save/extract/proxy 抽取） |
| Source of truth | SQLite vault（v2–v31 版本化迁移） | SQLite |
| 检索 | FTS5（CJK bigram）+ int8 向量 + 图谱，RRF 融合，两档词元降级 | LIKE '%word%' 扫描 + f32 向量暴力扫 + RRF + 多信号 rerank |
| 对外产物 | Electron 桌面端（搜索问答/数字分身/图谱浏览） | MCP + Proxy 透明注入 + CLI + Dashboard/VSCode/Obsidian |
| 特色 | 覆盖面记账、决策原因留痕、发送纵深防御、知识图谱社区 | MUST 指令化注入、Compliance 遵循追踪、权威分层 rerank |

MemVault 已经领先的点（本计划不涉及）：MCP Proxy 透明注入、Compliance tracking、指令化优先级体系（MUST/REFERENCE/NORMAL）、多信号 reranker、更轻的部署形态。

**阅读指引**：mycontext 代码库最值得学的其实有两层——具体技术方案（下面 P0/P1），以及它的**工程文化**：几乎每个文件头注释都用实测数据解释「为什么」，把「静默降级」当作头号敌人。后文引用的实测数字均来自其代码注释。

以下是值得吸收的点，按优先级排列。

---

## P0：直接可落地，收益/成本比最好

### 1. 用真 FTS5 + CJK bigram 替换 LIKE 关键词检索 ⭐ 本计划最重要的一条

**问题**：MemVault 的「BM25 关键词检索」实际实现是 `storage/sqlite.rs::search()` 里的多词 `LIKE '%word%'` OR 拼接 + `compute_relevance_score` 事后补分——没有倒排索引（全表扫描）、没有真 BM25 排序，且 README 第 118 行宣称的 "SQLite with bundled FTS5" 与代码不符（`grep -rn fts5 crates/` 零命中）。这是文档-代码落差，也是检索质量的实际短板。

**mycontext 的方案**（`packages/retrieval/src/`，三个文件一套组合拳）：
- `bigram.ts`：写入/查询共用一个分词器——CJK 切「单字 + 相邻二字」（`沙箱环境` → `沙 沙箱 箱 箱环 环 环境 境`），ASCII 词按词边界原样保留（不 bigram 化，避免 `deploy` 匹配 `epl` 噪音）。其实测依据：SQLite `unicode61` 不切 CJK（整句一个 token，`MATCH '沙箱'` 命中 0）；`trigram` 要 ≥3 字符，而中文两字词是最高频查询形态。单字必须保留，否则单字查询全灭——「单字召回偏多靠排序收敛，搜不到没法靠排序补救」。
- `match-expr.ts`：全仓库构造 FTS5 MATCH 的**唯一入口**，逐 token 用 `"…"` 包裹转义（FTS5 没有反斜杠转义）。实测未转义时用户输入 `环 OR mid:x`、`-沙箱`、`环*` 会直接 500 或被当成语法执行——中文输入法下这些字符很常见，是必然事故不是边缘情况。
- `recall.ts`：**两档词元降级**——严格档（bigram+单字，AND 组合）0 命中时才放宽到单字档；`relaxed` 标志必须随结果返回给调用方/UI（「悄悄放宽会让用户以为这就是精确结果」）。实测成本：严格档落空仅 ~0.1ms，不是 2× 成本。另有细节：`conversationIds: []`（空数组）= 限定在零个会话 = 必然空结果，不能退化成「不限定」——scope 过滤在 SQL 层做，不是拿回来再筛。

**方案**：
- `memories` 表挂 FTS5 影子表（content/instruction/tags 三列），应用层写入时同步维护（MemVault 写入路径集中，不必用触发器）。分词器用 bigram 方案（rusqlite 侧可用 simple tokenizer 或自定义 `fts5_api` 注册；最简路径是应用层分好词后以空格连接写入 FTS 表）。
- `search()` 的关键词路径改走 FTS5 `bm25()`，RRF 融合（`hybrid.rs`）从此才名副其实；`compute_relevance_score` 里与关键词匹配相关的部分可以删。
- MATCH 构造收单一入口 + 两档降级 + `relaxed` 透明上报（可进 `SearchResult` 元信息）。
- 顺手修正 README 的 FTS5 宣称与实现的关系（落地后宣称才成立）。

**涉及模块**：`memvault-core/storage`、`memvault-core/hybrid`、`memvault-cli`/`memvault-mcp`（展示 relaxed）。

**收益**：中文检索召回率（现有 LIKE 分词 + 同义词表补不了「词在文中但查询切分对不上」的漏检）、真 BM25 排序、文档与代码对齐。

---

### 2. 向量 int8 量化（每行独立 scale）

**问题**：MemVault 的 embedding 以 f32 blob 存储（`sqlite.rs::embedding_to_blob`），`vector_search` 暴力扫描且有 scan cap——注释自己承认「大数据集下结果可能不全，需要过滤器或真正的向量索引」。

**mycontext 的方案**（`packages/retrieval/src/quantize.ts` + `knn.ts`）：
- L2 归一化后按 `127/max(|v|)` **每行独立 scale** 量化成 int8（每行 scale 单独存）。逐行而非全局 scale 的原因：全局 scale 被个别大范数向量拉低，其余向量精度白白损失。归一化后余弦=内积，检索侧只需点积。
- 实测（1024 维 × 5 万条，单查询）：float32 = 195MB/35.7ms → int8 = **49MB/38.6ms**。内存 1/4、耗时 +8%，同预算常驻上限 5 万 → 20 万。注释原话：「这不是『以后再优化』级别的差异」。
- 附带设计：**维度不同不报错而是跳过**——维度变化意味着换过 embedding 模型，跳过后重建可以渐进进行，而不是全库报错不可用。MemVault 换模型时同样会遇到。
- 性能细节：点积循环里逐字节读 buffer，不做数组转换（「这个函数会被调 20 万次，每次分配数组会让 GC 压力主导耗时」）——Rust 侧无此问题，但「保留 float32 原值以备二段精排」的字段设计值得照搬。

**方案**：`embedding_to_blob` 增加 int8 编码路径（1 byte 头标识量化格式 + scale f32 + int8 序列），`vector_search` 点积计算；已有 f32 行继续按原路径算（与「换模型渐进重建」同构，天然兼容）。

**涉及模块**：`memvault-core/storage`、`memvault-core/embedding`（重嵌入时的格式选择）。

---

### 3. 召回来源留痕（hitBy）与检索调试信息

**问题**：「为什么这条记忆排第一」在 MemVault 里无法回答——RRF 融合后各路来源信息丢失。用户排查「为什么注入了这条/为什么没召回那条」（对应 DESIGN.md 断裂 1）只能靠猜。

**mycontext 的方案**（`packages/retrieval/src/fuse.ts`）：RRF 的每个命中保留 `hitBy: [{source, rank}]`（哪一路、该路第几名），答案里的引用标注用它；另有 `recall_debug`（每路条数 + 耗时）。注释原话：「『为什么这条排在前面』是搜索里最常被问的问题」。还有两个可顺手抄的实现细节：同分先比「被几路命中」再比 id，**确定性排序**（否则同分项顺序随哈希表插入序漂移，测试间歇性失败看起来像真 bug）；RRF k=60 且明确注释为什么不用加权分数相加（各路分数量纲不可比，权重会随语料漂移）。

**方案**：`hybrid.rs` 的 `merge()` 给 `SearchResult` 加 `hit_sources` 字段（keyword/vector + 各自 rank）；`memvault search --debug` 与 MCP `search_memory` 输出可选带出；rerank 后可再附最终信号分解（`rerank.rs` 已有各信号权重，只差输出）。

**涉及模块**：`memvault-core/hybrid`、`memvault-core/models`、`memvault-cli`、`memvault-mcp`。

**协同**：与 Compliance 打通——注入时的 compliance evidence 可以直接引用 hitBy（「这条记忆因为 keyword 路第 2、vector 路第 5 被召回并注入」），把「为什么注入」变成可审计记录。

---

## P1：需要设计但收益明确

### 4. 决策原因强制留痕（condition → reason 编译期映射）

**问题**：MemVault 的注入链路（router 过滤、soft intent filtering、token 截断、MUST 豁免）和 Compliance 的 `Unknown` 状态都不记录「为什么」。用户开了注入却没看到某条记忆时，唯一能做的就是翻日志猜——mycontext 把这种状态定义为**静默降级**，并认为是「最贵的 bug」，其 CLAUDE.md 第 4 节专门立规「报告事实，不要报告愿望」。

**mycontext 的方案**（`packages/persona/src/policy.ts`）：自动发送需 9 个条件全过，任一不过 → 拒绝且**必须**记录原因；`CONDITION_TO_REASON` 用 `Record<PolicyCondition, DecisionReason[]>` 让「新增条件忘了配原因」成为**编译错误**而非测试遗漏。注释原话：「如果不告诉他命中了哪条……他唯一能做的就是放弃这个功能」。

**方案**：
- Rust 等价物：`enum InjectSkipReason`（TokenBudgetExceeded / IntentFiltered / NamespaceMismatch / BelowScoreFloor / …）+ 穷尽 match 强制覆盖；注入决策路径每个分支返回原因而不是 `debug!` 一行。
- 落库到 session 注入记录（proxy 的 `InjectionState` 已有 `injected_memory_ids`，补 `skipped: Vec<(memory_id, reason)>`），`memvault session-start --explain` 输出。
- Compliance `Unknown` 带 reason 字段（「agent 未提及」与「无法判定」是两回事）。

**涉及模块**：`memvault-core/router`、`memvault-core/compliance`、`memvault-proxy/injection`。

---

### 5. 覆盖面记账（coverage accounting）

**问题**：MemVault 的批量操作（`extract` 从长文本抽取、`sync` 生成各 agent 指令文件、proxy 会话内增量抽取）只有「成功/失败」二值结果，没有「覆盖了多少」的记账。extract 处理长文本时静默漏掉一段，与 mycontext 的「91 个会话里 90 个齐了就报已采完」是同构问题。

**mycontext 的方案**（`packages/store/src/repositories/coverage-base.ts`，v27/v29 迁移）：按 (分区, 天) 记 `local_count`（累加不是覆盖）/ `listed_total`（COALESCE 保留旧值）/ `drained`（本轮结论覆盖）；按天聚合用 **MIN(drained)** 不是 MAX——「有一个分区没齐，这一天就不能说齐了」；`markDaysDrained` 只 UPDATE 不 INSERT——「这天没数据」与「这天采完了 0 条」不能混同。五条判据全是踩过的坑，抽基类是为了不让第二张表再踩五遍。

**方案**（轻量版，先不做通用框架）：`extract` 输出结构带 coverage 字段（输入分片数 / 产出记忆数 / 未覆盖分片及原因）；`sync` 报告各 agent 文件的生成/跳过原因；CLI 以「跳过 N 个，原因：…」展示（mycontext 的 `filterDistillable` 要求返回被拒计数给进度页，同一思路）。

**涉及模块**：`memvault-core/extractor`、`memvault-core/sync`、`memvault-cli`。

---

### 6. 防自我强化漂移：Agent 产出内容不得回流为记忆

**问题**：MemVault 的 `extract` 和 proxy 抽取面对的内容里混有 **Agent 自己的输出**。若不加区分地存为记忆，记忆库会在几轮迭代后坍缩成模型的口吻与模型的错误——mycontext 称之为自我强化漂移：「这个过程是渐进的，没有任何一刻会『报错』」。

**mycontext 的方案**（`packages/distill/src/guards.ts`）：蒸馏准入守卫集中一个文件（「这个判断只要有一处漏了，画像就被污染，而且不可逆——污染后的结论会作为下一轮的基线继续放大」）；枚举式拒绝原因 `identity_unconfirmed / self_generated / bot_channel / empty_content / distill_disabled`；其中 `is_self = NULL`（未判定）**拒绝**而不是默认任何一方——「未判定 ≠ 否定」是数据层契约，bool 建模会让误判不可恢复。

**方案**：
- `extractor` 增加来源角色感知：输入可标注 `source: user | agent | mixed`，`agent` 产出默认不进记忆（或降级为低权威、待人工确认）；mixed 未标注时按保守策略处理并告警。
- proxy 抽取已能区分 user/assistant 轮次，确认只从 user 轮抽取，写成测试锁住。
- 借鉴三态建模：凡是「判定不出」的身份/来源用 `Option<>` 而非默认值。

**涉及模块**：`memvault-core/extractor`、`memvault-proxy/extraction`。

---

### 7. 迁移校验的正确语义（schema checksum ≠ 全文 checksum）

**问题**：MemVault 的 schema 还在演进（`memory_history` 刚加，#3 还规划了 workspace 列迁移），一旦引入「迁移内容哈希校验」或用户多机同步 DB，会遇到 mycontext 已踩过两次的问题：**改一行 SQL 注释就等于改迁移**，已迁移的库启动即失败，且历史版本含脱敏差异时无解。

**mycontext 的方案**（`packages/store/src/migration-checksum.ts`）：校验的不变式是「schema 没被偷偷改过」，注释不是 schema——`schemaChecksum`（**按词法**剥注释+折叠空白后哈希，不能用正则，因为 SQL 字符串字面量里可能含 `--`）是判据；`rawChecksum` 仅作「完全没变」的快速路径保留。另配套 CI 脚本扫全历史迁移 blob 验证不变式。

**方案**：MemVault 若加迁移完整性校验（或 `memvault status` 报 schema 指纹），直接采用双语义设计；词法剥注释的坑提前避开。

**涉及模块**：`memvault-core/storage`。

---

## P2 / 远期：方向性启发，不主动排期

### 8. 事实级冲突建模（对应现有远期计划 #7 curator）

kl-graph 把 **Fact 作为一等节点**，显式建 `ENTAILS` / `CONTRADICTS` 边（`kl_graph/models/types.py`）——「用户偏好变了」不是靠向量相似度发现，而是沿 CONTRADICTS 边直接定位冲突、沿 ENTAILS 链取最新版本。MemVault 现有远期计划 #7（curator 定期 lint 矛盾）是**周期性发现**，mycontext 是**写入时建模**。若 #7 重启，可考虑折中：dedup 流水线（已在跑语义比对）顺手产出 `contradicts` 关系表，curator 只读表生成提案。另注意其 `ExtractionProjection`（提取粒度与检索 chunk 粒度解耦，证据带 primary/supporting 角色）——若远期计划 #5（entity-assisted 召回）重启，这是现成的设计参考。

### 9. 社区/主题级注入（GraphRAG 思路）

kl-graph 用自研增量 Leiden（HIT-Leiden，静态与增量同一算法保证一致，hub guard 防超级节点）维护 L0–L3 社区，查询侧有 GraphRAG-style local search（token 预算分配：社区 15% / 文本 50% / 关系 35%）。对 MemVault 的远期意义：注入预算紧张时，注入「主题社区摘要」而不是 N 条孤立记忆。依赖 #5/#8 的实体与关系层先行，暂不排期。

### 10. 注入侧的纵深防御清单（send-guard 类比）

mycontext 的发送守卫四层、每层失效原因互不相关：应用层短路 → **重读库比对 contentHash**（防「批准了 A 发出去 B」的竞态）→ 幂等 UUID（崩溃重发不重复）→ 宿主授权门（代码不可绕过）。另有边界设计：**agent 手上没有发送工具**（prompt injection 无处借力）。MemVault proxy 的类比物：注入文本生成后可计算 hash 留档，compliance 比对时能确认「agent 实际收到的 = 系统批准注入的」；`inject_session_id` 已有幂等雏形。此项收益偏运维审计，列 P2。

---

## 工程文化层面（不落地为代码，但值得吸收）

- **注释写「为什么」+ 实测数字**：mycontext 几乎每个文件头都有实测依据（「实测 1024 维 5 万条 = 35.7ms」），且明说「注释里的实测结论有保质期，与当前行为冲突时重新实测」。MemVault 注释质量已不错，但带实测数字的比例可以更高。
- **fail-closed 而非 fail-open**：「忘了注入密码必须是启动失败，而不是跑起来了但没鉴权」——MemVault 的 auth/embedder 配置缺失时的默认行为值得按这条过一遍（`capabilities.rs` 已是这个方向）。
- **单一入口纪律**：MATCH 构造、脱敏、scope 判定都收唯一入口，「一处漏了就等于没做」。MemVault 的 `escape_like` 已修过同类注入（test_search_escapes_like_wildcards），若引入 FTS5，务必同样收口。
- **确定性输出**：同分项按稳定次序排（测试间歇性失败看起来像真 bug）——MemVault rerank 同分场景可自查。

---

## 不建议做的事

- **不要**引入 IM/文档/会议采集链路（channels/ingest）——那是 mycontext 的核心但属于「工作数据采集平台」定位，MemVault 是轻量记忆层，采集面一旦铺开，隐私边界、渠道维护成本都会改变产品性质。除非明确决定扩展到工作上下文采集。
- **不要**移植数字分身（persona）产品形态——理由同上，且其发送安全体系是为其宿主 IM 场景设计的。
- **不要**复制任何 mycontext 代码进 MemVault——ELv2 与 MIT 不兼容，本文全部条目应为 clean-room 重新实现。
- **不要**为了对齐 kl-graph 而引入独立图数据库（Kuzu/FalkorGraph）——MemVault 单文件 SQLite 是核心卖点，远期若做关系层，用 SQLite 表建邻接关系即可（mycontext 自己的备选后端之一也是纯 SQLite）。

---

## 实施计划（执行范围：P0 + P1，共 7 项）

> 原则：**只借鉴思路，clean-room 实现**（mycontext 为 ELv2，代码不可移植）。
> 每项给出改动文件、关键点与验收测试；按 A → B → C 顺序执行。

### 阶段 A：检索引擎（P0-1/2/3）

**A1. FTS5 + CJK bigram（对 P0-1）**
- 新模块 `memvault-core/src/fts.rs`：
  - `tokenize(text) -> Vec<String>`：单趟扫描；CJK 连续段内产出「单字+相邻二字」（跨标点不组合），ASCII 词按词边界整词保留并小写化；**写入与查询共用同一函数**（两侧不一致 = 静默检索失效）。
  - `query_token_tiers(query) -> Vec<Vec<String>>`：三档——①严格（bigram+单字，AND）②放宽（仅单字，AND，仅在① 0 命中时启用）③兜底（严格词元+同义词 OR）。命中档位必须随结果上报。
  - `build_match_expr(tokens) -> String`：构造 FTS5 MATCH 的**唯一入口**，逐 token `"…"` 包裹（内部 `"` 双写），AND 组合；空 token 列表返回明确错误而非 SQL 语法错。
- `storage/sqlite.rs`：
  - schema 增加 `memories_fts` FTS5 表（content/instruction/tags + `memory_id UNINDEXED`）。
  - `save`/`save_with_embedding`/`update`/`delete` 同事务维护 FTS 行；启动时若 FTS 空而 memories 非空则一次性回填（`rebuild_fts_index`）。
  - `search()` 关键词路径改走 FTS5：JOIN memories 保留 namespace/type/priority 过滤，`bm25()` 提供真实排序；`compute_relevance_score` 保留（下游 rerank 用其分数）。
  - `MemoryStore::search` 返回类型改为 `SearchOutcome { results, keyword_tier }`（tier 枚举：Strict/RelaxedUnigram/SynonymFallback），全部调用点跟随。
- 测试：CJK 两字词命中（unicode61 原生命中的反例）、跨标点不组伪词、MATCH 注入安全（`-x`、`x OR y`、`"`）、三档降级路径、回填幂等。

**A2. 向量 int8 量化（对 P0-2）**
- `embedding.rs` 增加：L2 归一化 + 每行独立 scale 的 int8 量化/反量化、int8 点积余弦（两侧归一化后点积=余弦，scale 相乘还原量纲）。
- `sqlite.rs`：migration 5 加 `embedding_fmt INTEGER NOT NULL DEFAULT 0`（0=f32 旧格式，1=int8）；新写入一律 int8（blob = fmt 头 + scale f32 + int8 序列）；读取按列分派，**两种格式混存可共存**（换模型/渐进重建场景）。
- `vector_search`：维度不匹配 = 换过模型 → 跳过该行并 debug 记录，不报错。
- 测试：量化往返误差上界（dequant 与原向量余弦 ≥ 0.99）、混格式检索、维度不匹配跳过。

**A3. 召回来源留痕 hitBy（对 P0-3）**
- `models.rs`：`SearchResult` 增加 `hit_sources: Vec<HitSource { source: Keyword|Vector, rank }>`（serde default，空则不序列化）。
- `hybrid.rs`：融合时记录每路 rank；同分次序确定化：分数 → 命中路数 → id。
- 输出端：CLI `search` 与 MCP `search_memory` 的结果行附 `[kw#2 vec#5]` 标注。
- 测试：重叠命中双路留痕、确定性排序。

### 阶段 B：决策留痕与守卫（P1-4/5/6/7）

**B1. 注入跳过原因强制留痕（对 P1-4）**
- `router.rs`：`InjectSkipReason` 闭合枚举（NamespaceMismatch / TypeExcluded / IntentFiltered / TokenBudgetExceeded / MaxMemoriesExceeded）；`session_start` 每个过滤/截断分支记录 `SkippedMemory { id, reason }`；`format.rs::trim_to_budget` 改为返回被截断项。
- `SessionStartOutput` 增加 `skipped` 字段（serde default）。
- proxy `InjectionState` 记录 skipped；CLI `session-start` 打印跳过摘要；`ComplianceEvent` 增加 `reason: Option<String>`（compliance_events 表按列存在性幂等 ALTER）。
- 测试：每个 reason 变体至少一条产生路径；「无静默丢弃」——被过滤的记忆必然出现在 skipped 里。

**B2. 覆盖面记账（对 P1-5）**
- `extractor.rs`：新增 `extract_with_coverage() -> ExtractionOutcome { memories, coverage: { input_lines, extracted, empty, no_signal } }`（`extract()` 保留为薄封装）。CLI `extract` 打印覆盖统计。
- `sync.rs`：`SyncReport` 增加 `files_skipped: Vec<(target, reason)>`（如「无匹配记忆，跳过生成」）。
- 测试：覆盖计数正确（含空行/无信号行分类）。

**B3. 来源角色守卫，防自我强化漂移（对 P1-6）**
- `extractor.rs`：`SourceRole { User, Agent, Mixed, Unknown }` + `extract_guarded(text, role)`：`Agent` → 拒绝产出并返回原因 `SelfGenerated`；`Mixed/Unknown` → 产出但打 `review:required` 标签且降置信。
- proxy `extraction.rs`：assistant 响应路径默认**降级**（tag `review:required`、human_reviewed=false、置信打折），可用 `MEMVAULT_EXTRACT_ASSISTANT=off` 整体关闭（默认保持开启以兼容现状，但显式可关）；user 路径走 `User`。
- 测试：Agent 角色零产出+原因；降级路径标签存在；开关关闭时零写入。

**B4. 迁移 schema checksum（对 P1-7）**
- 新模块逻辑（放 `storage/sqlite.rs` + `auth` 复用 sha2）：
  - `strip_sql_comments(sql)`：**词法状态机**剥 `--`/`/* */` 注释，字符串/标识符字面量内部的注释符当普通字符（正则方案会截断含 `--` 的字面量）。
  - `schema_checksum(sql)` = sha256(剥注释+字面量外空白归一)。
- 新表 `schema_migrations(version, checksum, algorithm, applied_at)`：应用迁移时记录；启动校验已应用版本——不一致报 `SchemaDrift` 错误（fail-closed）；算法版本变化时重打基线并 warn。
- CLI `status` 输出 schema 指纹（user_version + 最新迁移 checksum 前缀）。
- 测试：注释增删不改 checksum、语义改动必改 checksum、字面量含 `--` 不误剥、drift 检测触发。

### 阶段 C：收尾
- `cargo test`（全 workspace）+ `cargo clippy` 全绿。
- 更新 CHANGELOG.md、README（检索章节：真 BM25/FTS5、两档降级、int8、hitBy；status 输出示例）、本文档各项补「落地记录」。
- 分阶段提交（A1/A2+A3、B1、B2+B3、B4+docs）。

---

## 落地记录（2026-08-19，全部完成）

**A1（FTS5+CJK bigram）**：与计划一致。`fts.rs` 新模块（tokenize/query_token_tiers/build_match_expr，均为 clean-room）；`memories_fts` 影子表 + 同事务维护 + 启动回填；`search()` 改 FTS5 bm25，`MemoryStore::search` 返回 `SearchOutcome`。落地时把三档中的兜底档实现为「严格词元+同义词展平 OR」，与计划一致。实测先验证了两个前提：bundled SQLite 3.53.2 自带 FTS5；unicode61 对中文整句不切分（`MATCH "沙箱"` 命中 0）。新增 12+5+3 条相关测试。

**A2（int8 量化）**：与计划一致。量化函数放 `embedding.rs`（quantize_int8/dequantize_int8/cosine_int8），migration 5 加 `embedding_fmt` 列；`save_with_embedding`/`set_embedding` 写 int8，`vector_search` 按 fmt 分派、维度不匹配跳过；`get_embedding` 反量化返回。量化往返误差测试（随机向量余弦 ≥0.99、int8 与 float 余弦差 <0.02）通过。

**A3（hitBy）**：与计划一致，另补一处计划外缺口——`rerank.rs` 重建结果会丢来源，已改为透传 `hit_sources`。

**B1（跳过原因）**：与计划一致。归因实现：exclude_types/intent 惩罚各记一个 HashSet，落入 score floor 时按「intent 惩罚 > type 惩罚 > 通用 floor」定因。`ComplianceEvent` 未加独立字段到所有查询路径，仅新增 `reason` 列 + `report_with_reason()`（report() 保持兼容）。

**B2（覆盖面）**：与计划一致。`extract_with_coverage` 四桶计数；MCP extract 响应结构从数组改为 `{coverage, memories}`（破坏性，见 CHANGELOG Changed）；sync 的 files_skipped 覆盖「配置关闭」这一种跳过原因。

**B3（来源守卫）**：与计划一致，映射方式：assistant 路径按策略选 SourceRole——Disabled→Agent（整体拒绝），Downgraded→Mixed（借用「未确认来源」分支实现降级打标），避免新增守卫分支。

**B4（迁移 checksum）**：与计划一致。未存 rawChecksum（MemVault 此前没有 checksum 存量，无兼容负担，只存 schema 语义哈希）；首次启用即对所有已应用迁移打基线（当前迁移文本为准，历史漂移一次性赦免——与 mycontext 的重基线策略同构）；算法版本列 `algorithm` 支持未来归一化规则变更时重打基线。

**测试与质量**：全 workspace 475 条测试通过（core 321），clippy 零警告。提交：`83732e7`（A 阶段）、`a9feb6f`（B1）、`9145b49`（B2+B3）、B4+docs（本次）。

---

## 附：mycontext 仓库速览（供后续查阅）

- 结构：`apps/desktop`（Electron+React）、`packages/*`（store/ingest/distill/retrieval/persona/agent-runtime/channels/knowledge-feed/kernel/llm 等 14 包）、`kl-graph/`（Python 图库 + FastAPI 服务）、`vendor/`（第三方，不看）。
- 本地克隆为 partial clone（`--filter=blob:none`），工作树文件按需拉取；若需全量 checkout，`git checkout HEAD -- .` 首次会下载全部 blob（vendor/python 约 1 万文件，较慢）。
- 阅读入口：各 `packages/*/src/*.ts` 的文件头注释；`kl-graph/docs/graph-design.md`；`docs/design/`（dashboard/persona-forge/channel 数据面设计记录）。
