# Changelog

All notable changes to this project will be documented in this file.

格式基于 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [Semantic Versioning](https://semver.org/lang/zh-CN/)。

## [Unreleased]

### Added
- **DeepSeek Harness (dsh) 接入**:作为标准 MCP 客户端接入 MemVault
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
- **Obsidian 插件功能补全**:
  - 单向 Vault 同步(`obsidian-plugin/src/sync.ts` + `MemVaultPlugin.syncVaultFromServer`):按 `memvault_id` frontmatter 匹配,`memvault_updated_at` 判断创建/覆盖/跳过,孤儿笔记默认不自动删除(`syncDeleteOrphans` 开关)
  - 接线此前从未被调用的 `deleteMemory()` 死代码到侧边栏删除按钮
  - 新增编辑 Modal(`MemVaultEditModal`)、完整新建 Modal(`MemVaultCreateModal`,替换原来硬编码的 2 种预设)
  - 新增 Dedup / Decay / Promote 命令,新增 API Key 设置项
- **VS Code 扩展**:新增 `memvault.apiKey` 配置项
- **三端测试基建**:Dashboard(vitest + @testing-library/react)、VS Code(抽出 `format.ts` 纯函数 + vitest)、Obsidian(抽出 `sync.ts` 纯函数 + vitest),三端各自新增 `npm test`
- **CI**:`.github/workflows/ci.yml` 新增 `dashboard`/`vscode-extension`/`obsidian-plugin` 三个独立 job(build + test,dashboard 额外跑 `cargo check/clippy/fmt`)
- **Release**:`.github/workflows/release.yml` 新增 `tauri-bundle`(macOS/Linux)、`vscode-package`(`.vsix`)、`obsidian-package`(`.zip`)三个 job,产物汇总进同一个 GitHub Release;新增 `docs/RELEASING.md` 记录 Marketplace 发布 / Obsidian 插件目录提交 / macOS 签名公证等手动步骤
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
- **dashboard/**：Tauri 2.0 桌面应用（React + TypeScript，4 个页面：Memory List / Search / Review Queue / Stats）
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