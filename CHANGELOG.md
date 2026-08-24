# Changelog

All notable changes to this project will be documented in this file.

格式基于 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [Semantic Versioning](https://semver.org/lang/zh-CN/)。

## [Unreleased]

### Added
- **Dashboard 检索增强**:Search 页支持「关键词 / 语义 / 混合」三种检索模式切换(`mode` 透传后端),结果命中词高亮(`<mark>`),并展示相关度得分与召回来源标签(`kw#n` / `vec#n`)。
- **CLI 新增 `review` 子命令**:无参列出待审队列,`--approve <id>` 批准、`--reject <id>` 删除,与 Dashboard Review 页等价(此前仅 REST/MCP 有审核能力)。
- **Web Dashboard(替代桌面 Tauri App):**`memvault-mcp` 新增 `--serve-web <dist>` 参数,将前端静态产物与 REST API 在同一端口托管(`--transport http` + `--serve-web ./dashboard/dist`,浏览器开 `http://127.0.0.1:3777`);REST 新增 `GET /api/stats` 聚合端点、`GET /api/memories?offset=` 分页参数;`POST /api/memories` 支持 `human_reviewed`/`ai_generated` 覆盖(手动新建记忆跳过待审)。
- Dashboard 前端移除 Tauri 依赖(`@tauri-apps/*`),新增 `src/api.ts` 统一数据层(`fetch` + 信封解包 + 字段映射);`vite.config.ts` 开发代理 `/api` → `127.0.0.1:3777`;Settings 页改为后端连接状态 + API Key 配置。
- 移除 `dashboard/src-tauri/`、根 workspace `exclude`、release.yml 的 `tauri-bundle` job;release 换为 `dashboard-web` job 产出 `memvault-dashboard-<tag>.tar.gz`。
- **FTS5 全文索引 + CJK bigram 分词**(`crates/memvault-core/src/fts.rs`、`storage/sqlite.rs`):
  - 关键词检索从 `LIKE '%word%'` 全表扫描升级为真 FTS5 + `bm25()` 排序(README 宣称的 FTS5 至此落地);新增 `memories_fts` 影子表随 save/update/delete 同事务维护,启动时行数不一致自动重建(覆盖旧库升级路径)
  - 中文「单字+相邻二字」分词:bundled SQLite 的 unicode61 不切 CJK、trigram 漏两字词(均已实测),bigram 方案让「沙箱」能命中「沙箱环境部署完成了」;写入与查询共用同一分词器
  - **三档匹配降级**:严格(bigram+单字 AND)→ 放宽(仅单字)→ 兜底(同义词 OR);降级档位随 `SearchOutcome.keyword_tier` 上报,CLI search 打印 relaxed 提示——放宽不静默
  - MATCH 构造收唯一入口(`build_match_expr`),用户输入中的 `-x`/`OR`/`"`/`*` 等 FTS5 语法字符一律变字面量,不再 500 或改变语义
- **向量 int8 量化存储**(`embedding.rs`):新写入 embedding 为「每行独立 scale 的 int8」,体积约为 f32 的 1/4,排序质量实测余弦 >0.99;`embedding_fmt` 列区分新旧格式,混存可共存;维度不匹配视为换过模型,跳过该行而非报错,支持渐进重建
- **召回来源留痕(hitBy)**:`SearchResult.hit_sources` 记录每路召回及名次(kw#2/vec#5);hybrid 融合同分次序确定化(分数→命中路数→id);rerank 保留来源;CLI search 与 MCP `search_memory` 输出均带来源标注
- **注入跳过原因全程留痕**:`InjectSkipReason` 闭合枚举 + `session_start` 返回 `SessionInjection { results, skipped }`;类型/意图软惩罚按归因定因,预算截断与数量上限逐条记账——候选被丢弃必有原因;`session_start_layered`/proxy InjectionState/CLI session-start/REST `/session/start` 全链路透传;compliance 增加 `reason` 列与 `report_with_reason`
- **抽取覆盖面记账**:`Extractor::extract_with_coverage` 返回 input/extracted/no_signal/empty 四桶计数(互斥且总和=输入行数);CLI extract 与 MCP `extract_memories` 输出覆盖统计;`SyncReport.files_skipped` 让 sync 的每个目标「写入或带原因跳过」
- **来源角色守卫(防自我强化漂移)**:`SourceRole` + `Extractor::extract_guarded`——Agent 产出整体拒绝(SelfGenerated),Mixed/Unknown 产出打 `review:required` 并降置信;proxy 新增 `AssistantExtractionPolicy`(默认降级保留兼容,`MEMVAULT_EXTRACT_ASSISTANT=off` 可整体关闭)
- **迁移 schema checksum**(`storage/schema_checksum.rs`):每个已应用迁移记录「词法剥注释+空白归一」后的 SHA-256;注释增删不改 checksum、语义改动必改、字面量内 `--` 不误剥;启动校验不一致即 fail-closed 报 `SchemaDrift`;`memvault status` 输出 schema 指纹
- **DeepSeek Harness (dsh) 接入**:作为标准 MCP 客户端接入 MemVault
- **LLM 上下文记忆提取**(`crates/memvault-core/src/llm_extractor.rs`):新增 `LlmExtractor` trait + `OpenAiChatExtractor`(任意 OpenAI 兼容 chat/completions 端点,提示词含 prompt-injection 防护);`ResponseExtractor::extract_and_save_contextual`(`memvault-proxy/src/extraction.rs`)把 user_text + response_text 作为一个上下文一起交给 LLM(而非逐行独立扫描关键词),LLM 调用失败自动回退规则提取;`notify_response` 改走该路径。MCP `extract_memories` 新增 `mode`(`rule`/`llm`)与 `assistant_text` 参数,便于手动验证。
  - **本地优先**:`MEMVAULT_LLM_EXTRACTION_PROVIDER` 未设置或设为 `auto` 时,自动探测本机 Ollama(`http://localhost:11434`),探测到即零配置启用(默认模型 `qwen2.5:7b`,免费、不出本机);未探测到则保持纯规则提取,行为与加这个功能之前完全一致。远程提供商(`openai`/`openai-compatible`)必须显式设置才启用,不因别处配了 `OPENAI_API_KEY` 自动打开;`off`/`disabled`/`none` 强制关闭,即使本机有 Ollama 也不用。

### Changed
- **dsh 插件包名更名**:`@memvault/dsh-plugin` → `@memvault/dsh-memvault`——dsh 设置→插件→插件列表的显示名由 `plugin` 变为 `memvault`(列表名由 npm 包名派生);MCP 握手 `clientInfo.name` 同步改为 `memvault-dsh-memvault`。已安装的 profile 需重新 `pnpm install`(bundles 已同步更新),新装请用新包名。
- **文档归档**:核心设计假设验证实验从仓库根 `experiments/` 移至 `docs/experiments/`,标注「历史验证」(H1-H4 全部 CONFIRMED,结果与复现方式见 `docs/experiments/README.md`)
- **仓库卫生**:移除误提交的个人 MCP 客户端配置 `mcp-config.json`(含本机绝对路径),改用 gitignore + 新增 `mcp-config.example.json` 模板(仓库相对路径)
- **行为变化**:`MemoryStore::search` 返回 `SearchOutcome { results, keyword_tier }`;`MemoryRouter::session_start` 返回 `SessionInjection`;`trim_to_budget` 返回被截断尾部;新写入 embedding 为 int8 格式(旧 f32 行照常读取);MCP `extract_memories` 响应改为 `{ coverage, memories }` 结构;CLI `save` 与 `POST /api/memories` **默认生成子向量**(embedder 可用时,响应加 `"embedded":bool`;`MEMVAULT_EMBEDDING_PROVIDER=off` 关闭);REST `/api/search` 新增 `mode` 参数(keyword/hybrid/semantic)并逐条返回 `search_mode`+`hit_sources`;REST `/api/extract` 响应改为 `{ memories, coverage }` 结构
  - `docs/INSTALL.md` §2.5:dsh 的 MCP stdio 配置片段 + Cordis 插件机制背景说明
  - `agents.example.yaml`:新增 `deepseek-harness` Agent Registry profile
  - README / README.zh-CN 集成表格新增条目
- **通用记忆编辑接口**: `PUT /api/memories/{id}`(`crates/memvault-mcp/src/rest_api.rs`)
  - 全字段 `Option` patch 语义,支持 content/instruction/priority/type/tags/namespace/layer/skill_trigger/skill_steps/skill_verification 的部分更新
  - 复用已有的 `MemoryStore::update`,不影响 `human_reviewed`(区别于 `/api/inbox/{id}/edit`)
- **REST Admin 鉴权**:`list/delete/update` memories、inbox 全部接口、`dedup/decay/promote`、compliance 接口统一接入 `AgentAuth`
  - 通过 `X-MemVault-Agent-Id`(默认 `admin`)+ `X-MemVault-Api-Key` 请求头鉴权
  - 未在 `agents.yaml` 配置 `admin` key 时保持无鉴权,向后兼容现有部署
  - `docs/INSTALL.md` 新增 §2.6 说明 REST API 的 transport 要求与鉴权配置

- **Dashboard 功能补全**:新建/编辑 Memory 表单、Settings 页(本地 DB 路径)、Stats 页新增 Compliance 汇总视图、Memories 列表支持 namespace 过滤 + 分页
- **Dashboard 导航刷新**:头部徽标(Review / Memories 计数)与命名空间下拉增加 30s 轮询 + 窗口聚焦即时刷新——外部 CLI/MCP 写入后无需手动刷新或切 tab 即可同步计数(复用既有 stale-guard 保护)
- **Obsidian 插件功能补全**:
  - 单向 Vault 同步(`obsidian-plugin/src/sync.ts` + `MemVaultPlugin.syncVaultFromServer`):按 `memvault_id` frontmatter 匹配,`memvault_updated_at` 判断创建/覆盖/跳过,孤儿笔记默认不自动删除(`syncDeleteOrphans` 开关)
  - 接线此前从未被调用的 `deleteMemory()` 死代码到侧边栏删除按钮
  - 新增编辑 Modal(`MemVaultEditModal`)、完整新建 Modal(`MemVaultCreateModal`,替换原来硬编码的 2 种预设)
  - 新增 Dedup / Decay / Promote 命令,新增 API Key 设置项
- **VS Code 扩展**:新增 `memvault.apiKey` 配置项
- **三端测试基建**:Dashboard(vitest + @testing-library/react)、VS Code(抽出 `format.ts` 纯函数 + vitest)、Obsidian(抽出 `sync.ts` 纯函数 + vitest),三端各自新增 `npm test`
- **CI**:`.github/workflows/ci.yml` 新增 `dashboard`/`vscode-extension`/`obsidian-plugin` 三个独立 job(build + test,dashboard 额外跑 `cargo check/clippy/fmt`)
- **Release**:`.github/workflows/release.yml` 用 `dashboard-web`(产出 `memvault-dashboard-<tag>.tar.gz`)替代此前的 `tauri-bundle`;`build-binaries` 产出 Linux x86_64 / macOS arm64+x86_64 三个 Rust 二进制,另有 `vscode-package`(`.vsix`)、`obsidian-package`(`.zip` + 未压缩的单文件资产)、`docker`(ghcr.io)与 `github-release` 汇总;新增 `docs/RELEASING.md` 记录手动步骤——VS Code Marketplace 发布(`vsce publish`)与 Obsidian 社区插件目录 PR,无桌面 App 故**无需 macOS 签名/公证**
- VS Code 扩展新增 `repository` 字段 + `.vscodeignore` + `LICENSE`,清理 `.vsix` 打包警告与内容(不再打包 src/测试文件)
- **记忆历史与单条回滚**(`crates/memvault-core/src/storage/sqlite.rs`):
  - 新增 `memory_history` 表(整行 JSON 快照,不逐列镜像,避免未来 `memories` 加列时同步改历史表 schema)+ `idx_memory_history_memory_id` 索引
  - `update` / `delete` 改为事务内先快照旧行再写库;新增 `list_checkpoints` / `restore_checkpoint`(更新可回滚、删除可重建,回滚本身再记一条新快照,「撤销的撤销」免费获得)
- **CLI 能力自检与历史命令**(`crates/memvault-cli/src/lib.rs`):
  - `memvault status`:输出 embedding provider 状态 + 无它时各功能是否降级(新增 `crates/memvault-core/src/capabilities.rs` 的 `capability_report`)
  - `memvault checkpoints [--memory-id X] [--limit N]` / `memvault restore --history-id N`
  - `memvault dedup` 接线 `build_embedder_from_env()`,与 mcp/proxy 对齐的向量辅助去重,不再始终退化为纯关键词
- **Rerank 权威信号**(`crates/memvault-core/src/rerank.rs`):新增第 6 信号 `RerankConfig.authority_weight`(默认 0.15)
  - `decision` / `procedure` / `gotcha` 标签(大小写不敏感)或 L2/L3 层获得有界加权,软提升而非硬过滤;MUST 绝对置顶逻辑不受影响
  - `memvault-mcp` 的 `search_memory` 现在同样经过 rerank(`crates/memvault-mcp/src/server.rs`),此前仅 `session_start` 重排

### Fixed
- **dsh-plugin 端口覆盖生效**:`process-manager.ts` spawn `memvault-proxy` 时显式传 `--port <config.port>`,避免二进制 CLI 默认端口 (3778) 无条件覆盖 `~/.memvault/proxy.yaml` 端口的问题——此前同一台机器的第二个 dsh 实例会因 3778 被占而无法拉起 proxy,注入/抽取静默失效。

- **`memvault-proxy` 上游连接两个真实 bug**(`crates/memvault-proxy/src/upstream.rs`,由新增集成测试暴露):
  - `connect_one` 中 `RunningService` 在分支结束被 drop,peer 立即收到 `TransportClosed`,导致生产环境下上游转发**一直不可用**;`UpstreamConnection` 新增 `_service` 字段保活
  - `connect_all` 原先按 `defs` 的 enumerate 下标注册索引,若前序 def 连接失败,后续连接索引越界;改用 `connections.len()` 修正
- **`save_with_embedding` 写库遗漏 `layer`/`skill_meta` 列**:`SqliteStore::save_with_embedding` 的 INSERT 未包含 storage 已迁移出的这两列,导致 CLI/MCP 显式指定 layer 或保存 skill 类型记忆时走向量分支会静默丢弃这些字段(读回默认 L1/None)。已与 `save` 对齐补上两列,并新增 `memvault-cli review` 相关回归覆盖。
- **隐式选中的 `OPENAI_API_KEY` embedding provider 会先校验再信任**:`build_embedder_from_env()` 未显式设置 `MEMVAULT_EMBEDDING_PROVIDER` 时,仅凭环境里存在 `OPENAI_API_KEY`/`OPENAI_API_BASE` 就向后兼容猜成 `openai`——但这只是猜测,该 key 常常是别的工具(如 dsh)留在进程环境里的,和 MemVault 自己的 embedding 凭据完全无关,导致每次 hybrid search 都对 OpenAI 打一次注定失败的 401 请求再降级关键词。现在这条隐式路径在启动时会先用一次 embed 调用校验 key 是否真的可用(5s 超时),校验失败自动降级到内嵌 `native` 模型;显式设置 `MEMVAULT_EMBEDDING_PROVIDER` 的行为不受影响,继续被无条件信任、不做校验。见 `crates/memvault-core/src/embedding.rs` 新增的 `validate_remote_embedder`。
- **测试覆盖审查驱动的一批修复**(含回归测试):
  - `promote.rs`: `consolidate_l1` 截断改为 UTF-8 字符边界,修复 CJK 超长内容合并时的 panic
  - `rest_api.rs`: `UpdateRequest` 以 double-option 区分「缺失 / null 清空 / 更新」,修复编辑时无法清空 `instruction` 等字段;非法 `priority`/`type`/`layer` 不再静默降级,而是返回 `400`
  - `query_expand.rs`:ASCII 短键仅按完整词匹配,消除 `prefer` 命中 `pr` 等假阳性
  - `rest_api.rs`:`http_error` 将领域错误映射到正确状态码(鉴权失败 `401`、Not Found `404`,此前一律 `500`)
  - `sqlite.rs`:LIKE 通配符 `%`/`_` 转义;`top_k` 钳制到 `[1, 1000]`
  - `proxy/injection.rs`:`refresh()` 返回成败,不再虚报已刷新
  - `proxy/upstream.rs`:同名工具/资源注册改为首者优先 + 警告,不再静默覆盖
  - `agent_adapt.rs`:`format_xml` 对内容做 XML 转义;`format_markdown` 保留 Background 级记忆为 Notes 段;裸 `claude` agent_id 归入 claude-code
  - `extractor.rs`:`to_instruction` 前缀剥离改为大小写不敏感
  - 新增 CORS 三态测试、PUT/DELETE 鉴权失败路径测试;新增 `dashboard/src-tauri` 首个单测;CI REST 冒烟补充 `PUT`/inbox/compliance/dedup/decay 端点
- **Dashboard 运行时崩溃修复**:`dashboard/src/App.tsx` 已经在调用 `run_promote`/`run_decay`/`run_dedup`,并读取 `layer`/`skill_meta`/`stats.layers`/`stats.skills`,但 `dashboard/src-tauri/src/lib.rs` 从未注册对应 command 或字段,导致点击这几个按钮时报 "command not found"。现已补上 `run_promote`/`run_decay`/`run_dedup`/`create_memory`/`update_memory` command,并给 `MemoryView`/`StatsView` 补上缺失字段。
- **严重 REST 客户端 bug**:VS Code 扩展与 Obsidian 插件的请求封装函数(`apiRequest` / `MemVaultPlugin.api`)从未解开 REST API 统一返回的 `{ok, data, error}` 外层,导致两端所有 REST 调用(list/search/save/approve/reject/delete/stats...)在真实后端下都拿到错误的数据形状——`search` 结果因此在两端都会直接抛出运行时异常。现已在两处请求函数内统一解包 `data` 并在 `ok:false` 时抛出 `error`。
- **`/api/search` 响应形状不匹配**:REST 返回的是扁平字段,但两端客户端一直按 `{ memory: {...}, score }` 嵌套结构解析——即使解包 `data` 后仍会因 `result.memory` 为 `undefined` 而抛错。已修正 `rest_api.rs` 的 `search_memories` 返回嵌套结构。
- **`/api/memories` 与 `/api/search` 字段不全**:两个接口此前都缺 `layer`/`skill_meta`/`access_count`/`decay_score`/`created_at`/`updated_at` 等字段,但两端客户端的 UI 早就在读取这些字段(界面上一直显示 `undefined`)。新增共享的 `memory_to_json` helper,统一返回完整字段。
- **Obsidian `getInbox()` / VS Code inbox tree 数据错位**:`/api/inbox` 返回 `{memories, total}`,但两端此前直接把整个对象当 `Memory[]` 用。已修正为解构 `.memories`。
- README / README.zh-CN 关于 Obsidian 插件"双向 Markdown 同步"的描述与实际实现不符(从未实现),已更新为准确描述当前的单向同步能力。
- **dsh-plugin 依赖版本过期**:`dsh-plugin/package.json` 的 devDependencies(`@deepseek-ai/dsh-llm`/`dsh-session`/`dsh-system-prompt`/`dsh-scope`)仍锁在早期 `^0.0.1-rc.1`,而 dsh 已发布到 `0.1.1-rc.2`,semver range 完全不匹配,导致本地 `npm install` 一直解析到过期版本。已把四个包(连同 `cordis`/`schemastery`)精确锁定到当前 npm 最新版本;`npm run build`/`npm test`(含真实 `Context` 挂载的 smoke test)针对真实新版本包全部通过,`dsh-plugin/src/*` 代码本身无需改动(API 面未变,新增的多模态 `image` content block 已被现有的 text-only 过滤逻辑安全忽略)。详见 `docs/DSH-BRIDGE-DESIGN.md` §7.5。
- **README / README.zh-CN 过时数据与措辞修正**:测试总数 496 → 517(core 356 + MCP 72 + proxy 63 + CLI 26,反映本轮新增的 LLM 提取相关测试),`memvault-core` 模块数 22 → 23(补 `llm_extractor`);"Why MemVault" 表格补一行「记忆提取」对比。同时把 README 里偏 Claude Code 专属的措辞("stdio (Claude Desktop / Claude Code)"、Integrations 表格逐个列 Claude/Cursor)改成"任意标准 MCP 客户端"的通用框架,Integrations 表格新增「任意其它 MCP 客户端」行并明确标注哪些是实际验证过的、哪些只是"理论可用"(不虚报未测试过的具体产品);DeepSeek Harness (dsh) 条目从"标准 MCP stdio"升级为同时列出零代码插件与 `dsh-plugin/` 深度集成两种方式。

### Test
- **测试缺口一次性补齐**(详见 `docs/TEST-GAP-ANALYSIS.md`,llvm-cov 行覆盖 90.95% → 92.25%,region 88.15% → 94.11%):
  - Rust:`memvault-proxy` `upstream.rs`/`handler.rs`/`main.rs`(HTTP 往返集成测试:fake MCP server → `UpstreamManager`、资源/提示词/工具转发、`resolve_path`/`/mcp` 路由);`memvault-mcp` `server.rs`(资源往返)、`main.rs`(CLI Args)、`sse_server.rs`(`/mcp` 挂载);`memvault-core` `native_embedding.rs` 抽 `resolve_model_dir` 纯函数
  - TypeScript:obsidian-plugin `client.test.ts`(+17,9 个 REST 方法 + settings + `syncVaultFromServer`);vscode-extension `extension.test.ts`(+10,真实 HTTP server 覆盖 activate/全部命令);dsh-plugin `config`/`mcp-client`/`process-manager`(+14);dashboard `api.test.ts` 补齐 6 个未测函数、`App.test.tsx` 补 stats/管线按钮/approve+reject 交互
  - `dsh-plugin/src/process-manager.ts`:`startProxy` 增加可选 `timeoutMs` 参数以支持超时路径测试

## [0.2.0] — 2026-08-11

### Added
- **分层注入策略（Phase 9.5a）**: `session_start_layered()` + `format_layered_instructions()`
  - MUST 记忆全文注入，超出 Token Budget 的 REFERENCE 以摘要展示
  - 末尾追加 "还有 N 条相关记忆可通过 search_memory 查询" 提示
  - Proxy injection 同步使用 layered 格式
- **MemoryLayer 分层记忆（Phase 9.5b）**: L0(raw) / L1(atom) / L2(scenario) / L3(persona)
  - 新增 `MemoryLayer` 枚举，自动从 priority 推导默认值
  - SQLite migration + CLI `--layer` + MCP `layer` 参数
  - `serde(default)` 确保向后兼容
- **Promote 自动提炼管线（Phase 9.5c）**: `promote.rs` 模块
  - L1→L2：按 tag 分组，满足阈值时合并为场景级记忆
  - L2→L3：高频/偏好类记忆自动提升为 MUST 级 persona
  - 源记忆归档为 L0，CLI: `memvault promote [--min-l1 N] [--min-l2 N]`
- **Skill 结构化（Phase 9.5d）**: `SkillMeta` 结构体
  - 可选字段：trigger / steps / verification / version
  - CLI: `--skill-trigger` / `--skill-steps` / `--skill-verification`
  - MCP: `skill_trigger` / `skill_steps` / `skill_verification` 参数
- **Extraction 闭环（Phase 9.5e）**: Proxy 回复自动提取
  - 新增 `extraction.rs` 模块 + `notify_response` MCP 工具
  - 白名单策略（preference/fact/skill），置信度阈值 + 限流
  - 提取的记忆写入 Inbox（human_reviewed=false）
- **假设验证实验**: `experiments/` 目录
  - `verify_hypotheses.py`: 自动化 A/B 测试脚本
  - `REPORT.md`: 4 个设计假设全部验证通过 (H1-H4 CONFIRMED)
- **Agent 身份验证机制（Phase 9a）**: 新增 `auth` 模块，基于 SHA-256 API Key 验证
- `crates/memvault-core/src/auth.rs`: `AgentAuth` / `AgentCredentials`，支持可选 API Key 认证
- `AgentProfile` 新增 `api_key` 字段，YAML 加载时自动哈希，不留存明文
- MCP Server（stdio/SSE）工具参数支持 `api_key`，调用前自动认证
- REST API 端点支持 `api_key` 认证
- Proxy handler 支持 API Key 传递
- 向后兼容：未配置 `api_key` 的 Agent 无需认证
- **Rerank 多信号二次排序（Phase 9b）**: 新增 `rerank` 模块，提升 top-k 精度
- `crates/memvault-core/src/rerank.rs`: `MultiSignalReranker` 基于 5 信号加权排序（混合分 35% + 查询重叠 25% + 时效 15% + 优先级 15% + 访问频率 10%）
- 集成到 `MemoryRouter::session_start()` 中，在混合搜索之后、软过滤之前执行
- 可通过 `RerankConfig.enabled=false` 禁用以实现完全向后兼容
- **Inbox 审核面板（Phase 9c）**: 新增 pending 审核系统
- `list_pending` 存储方法：按 `human_reviewed = 0` 过滤，按 `created_at ASC` 排序
- REST API：`GET /api/inbox`（列表）+ `POST /api/inbox/{id}/approve`（批准）+ `POST /api/inbox/{id}/reject`（拒绝）+ `POST /api/inbox/{id}/edit`（编辑）
- MCP Tool：`list_inbox` — 列出待审核的记忆
- **Compliance Tracker 遵循度追踪（Phase 9d）**: 扩展 REST API 遵循度端点
- `GET /api/compliance/session?session_id=X` — 查询单次注入会话的遵循报告
- `GET /api/compliance/summary?agent_id=X&limit=N` — 聚合统计遵循率
- `session_start` REST API 端点返回 `inject_session_id`，支持后续报告
- 基于已有 `ComplianceStore`（SQLite），MCP Server HTTP 模式可选启用
- **CLI/MCP 集成测试（Phase 9e）**: 新增 5 个端到端集成测试（namespace 过滤、搜索过滤、更新、跨 namespace 回退、Agent profile 匹配）
- 集成测试从 12 增至 **17 个**
- **安全审计（Phase 9f）**: `cargo audit` 扫描 285 个依赖，**0 漏洞**发现
- 无硬编码密钥、秘密或凭据；API Key 通过环境变量或可选 YAML 配置处理
- **性能基准测试（Phase 9g）**: 新增 Criterion 基准测试套件
- `search_keyword_500`: **~29µs**（500 条记忆中关键词搜索）
- `search_empty_query_500`: **~10.5µs**（空查询返回 top-N）
- `list_500`: **~23µs**（列出 100 条记录）
- `session_start_basic_500`: **~18µs**（500 条记忆中基础 session start）
- 所有基准测试均远低于 PLAN 要求的 50ms 目标
- **`--transport sse`**: MCP SSE Server — 通过网络 HTTP/SSE 传输接受多个 MCP 客户端连接
- `memvault-mcp/src/sse_server.rs`: 基于 rmcp StreamableHttpService + axum，监听在 `/mcp` 端点
- **Phase 8b**: Auto-Injection on Connect — 客户端初始化时自动触发 embedding 回填
- 项目文档体系重构：统一 GitHub 标准布局（`docs/` + `.github/` + 顶层 LICENSE 等）
- `docs/RECALL_PLAN.md` 召回率提升 7 项改进（词级分词 / 多字段搜索 / 查询扩展 / 相关性评分 / 软意图过滤 / 跨命名空间回填 / Embedding 自动回填）
- `docs/SYNC_PLAN.md` 零入侵多 Agent 同步方案（`memvault sync` 生成 `CLAUDE.md` / `AGENTS.md` 等指令文件）
- `docs/PLAN.md` v0.3 实施计划（Phase 0–5）
- `docs/DESIGN.md` v0.3 产品与架构设计（13 章 + 多 Agent 共享记忆设计）
- **RECALL_PLAN #7**: Embedding 自动回填 — `session_start` 异步检测缺失 embedding 的记忆并生成

### Changed
- `PLAN.md` 移动并重命名为 `docs/PLAN.md`
- `RECALL_PLAN.md` 标准化为 `docs/RECALL_PLAN.md`（移除 `` 00 阶段性符号）
- `SYNC_PLAN.md` 标准化为 `docs/SYNC_PLAN.md`

- **`memvault sync --watch`**: 轮询模式 — 检测数据库变化后自动重新生成指令文件
- `MemoryStore::sync_state_hash` 轻量变更检测接口

### Fixed
- 修复 5 个编译 warnings（`tool_router`、`agent_type`、`run_stdio_server` 等）

## [0.1.0] - 2026-08-09

### Added
- **memvault-core**：12 模块（storage / router / intent / embedding / hybrid / extractor / dedup / decay / io / models / config / error）
- **memvault-mcp**：MCP Server（rmcp 3.1.1，stdio，8 tools + 2 resources）
- **memvault-cli**：11 个子命令（save / search / list / delete / session-start / resource / extract / dedup / decay / export / import）
- **dashboard/**：Web Dashboard（React + TypeScript，浏览器端 4 个页面：Memory List / Search / Review Queue / Stats，由 `memvault-mcp --transport http --serve-web` 托管）
- **vscode-extension/**：VS Code 扩展（侧边栏记忆列表、搜索、右键保存选中文本）
- **obsidian-plugin/**：Obsidian 插件（侧边栏面板、搜索、双向 Markdown 同步）
- **Agent Registry**（YAML 配置）按 Agent 类型 / tag 过滤注入
- **MUST / REF 指令化格式**：MUST 级记忆不被裁剪
- **RRF 融合**：关键词 + 向量语义 + 混合检索 3 种模式
- 多 Agent 共享：单 MCP Server 实例服务多 Agent 客户端，共享 SQLite + LanceDB
- ``**自动注入**：MCP Resource 启动时加载 + `session_start` 按 Agent 身份过滤` ``
- `agent_adapt.rs` 多 Agent 适配层
- LLM 智能提取意图 / 去重 / 衰减 / 归档管道

[Unreleased]: https://github.com/dreamor/memvault/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/dreamor/memvault/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/dreamor/memvault/releases/tag/v0.1.0