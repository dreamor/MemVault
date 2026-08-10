# Changelog

All notable changes to this project will be documented in this file.

格式基于 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [Semantic Versioning](https://semver.org/lang/zh-CN/)。

## [Unreleased]

### Added
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