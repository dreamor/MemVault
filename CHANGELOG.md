# Changelog

All notable changes to this project will be documented in this file.

格式基于 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [Semantic Versioning](https://semver.org/lang/zh-CN/)。

## [Unreleased]

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

[Unreleased]: https://github.com/user/memvault/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/user/memvault/releases/tag/v0.1.0