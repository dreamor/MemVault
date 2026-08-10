# MemVault

> **AI Agent 时代的个人记忆路由器（Memory Router）**
> 不是让 Agent 学会查记忆，而是让记忆自动出现在 Agent 面前。

[![CI](https://img.shields.io/github/actions/workflow/status/user/memvault/ci.yml?style=flat-square&branch=main)](https://github.com/user/memvault/actions)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg?style=flat-square)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-stable-orange.svg?style=flat-square)](https://www.rust-lang.org)
[![MCP](https://img.shields.io/badge/MCP-compatible-blue.svg?style=flat-square)](https://modelcontextprotocol.io)
[![Status](https://img.shields.io/badge/status-beta-yellow.svg?style=flat-square)](#项目状态)

任何 MCP 兼容的 Agent 接入后,自动共享同一套用户记忆。

---

## 目录

- [核心能力](#核心能力)
- [项目状态](#项目状态)
- [快速开始](#快速开始)
- [MCP Server 接入](#mcp-server-接入)
- [MCP Tools / Resources](#mcp-tools--resources)
- [CLI 命令](#cli-命令)
- [架构概览](#架构概览)
- [文档索引](#文档索引)
- [项目结构](#项目结构)
- [路线图](#路线图)
- [贡献与社区](#贡献与社区)
- [许可](#许可)

## 核心能力

- **自动注入**：MCP Resource 启动时加载 + `session_start` 按 Agent 身份过滤 + SSE Auto-Injection
- **混合检索**：关键词 + 向量语义 + RRF 融合（3 种搜索模式），含词级分词/同义词扩展/相关性评分
- **MUST 保障**：MUST 级记忆永远不会被过滤或裁剪
- **多 Agent 差异化**：Agent Registry 按类型 / tag 软过滤（评分降权替代硬排除）
- **召回率优化**：词级分词 / 多字段搜索 / 同义词扩展 / 相关性评分 / 软意图过滤 / 跨 namespace 回填 / Embedding 自动回填
- **零入侵同步**：`memvault sync` 自动生成 CLAUDE.md / AGENTS.md 等指令文件，支持 `--watch` 轮询
- **智能管道**：自动提取 / 去重 / 衰减 / 归档
- **生态覆盖**：CLI + MCP Server(stdio + SSE) + Tauri Dashboard + VS Code + Obsidian

## 项目状态

> v0.1.0 — 核心 + 检索 + Dashboard + 智能管道 + 召回率优化 + MCP Proxy 已完成。

| 模块 | 状态 | 说明 |
|------|------|------|
| `memvault-core` | ✅ v0.1.0 | 15 模块,含存储/路由/检索/Embedding/去重/衰减/Sync/查询扩展 |
| `memvault-cli` | ✅ v0.1.0 | 12 个子命令(save/search/list/delete/session-start/resource/extract/dedup/decay/export/import/confirm-read/sync) |
| `memvault-mcp` | ✅ v0.1.0 | MCP Server(rmcp 3.1.1)8 tools + 2 resources + SSE 传输 |
| Dashboard (Tauri 2) | ✅ Alpha | 4 个页面可用 |
| VS Code Extension | ✅ Alpha | 侧边栏 + 搜索 + 右键保存 |
| Obsidian Plugin | ✅ Alpha | 侧边栏 + 双向 Markdown 同步 |
| 召回率优化(7 项) | ✅ 已完成 | 见 `docs/RECALL_PLAN.md` |
| 多 Agent 文件同步 | ✅ 已完成 | 见 `docs/SYNC_PLAN.md` |
| MCP Proxy (SSE + Auto-Injection) | ✅ 已完成 | `--transport sse` 网络传输 |
| Core 测试覆盖率 | ✅ 89.17% | 130 tests (118 unit + 12 E2E) |
| Compliance Tracker | 🕐 计划中 | 统计 Agent 遵循率 |
| Rerank / Inbox 审核 | 🕐 计划中 | 检索增强二期 |

## 快速开始

### 前置条件

- Rust 1.83+(`rustup install stable`)
- SQLite 3.x(系统附带即可,bundled-rusqlite 已启用)
- 可选:`OPENAI_API_KEY`(启用语义搜索)
- 可选:Node.js 20+(Dashboard / 扩展开发)

### 构建

```bash
git clone https://github.com/user/memvault.git && cd memvault
cargo build                --release
# 或使用 cargo-make(若已安装)
cargo make ci
```

二进制产物:`target/release/memvault-cli`、`target/release/memvault-mcp`。

### 30 秒上手

```bash
# 保存 MUST 级偏好(指令化注入,Agent 必须遵循)
memvault-cli save --content "用户偏好 Python" --priority MUST --type preference \
  --instruction "代码使用 Python,不用 Java" --tags "coding,python"

# 关键词 / 向量 / 混合 三种搜索模式
memvault-cli search --query "Python" --mode hybrid

# 查看某 Agent 启动时被注入的上下文
memvault-cli session-start --agent-id claude-desktop --context "帮我写代码"

# 从文本自动提取并保存记忆
memvault-cli extract --text "I prefer dark mode. Our project uses Rust." --save

# 去重扫描 + 衰减 + 归档
memvault-cli dedup && memvault-cli decay

# 确认记忆已读(更新 access_count)
memvault-cli confirm-read --ids <memory-id>

# 零入侵同步：生成所有 Agent 指令文件
memvault-cli sync
# 或监控模式(检测到数据库变化自动重新生成)
memvault-cli sync --watch

# 备份/恢复
memvault-cli export --format json --output ~/backup.json
memvault-cli import --format markdown --input ~/vault/memories/
```

> 完整安装指南(所有平台 / Docker / Dashboard / VS Code / Obsidian)见 **[`docs/INSTALL.md`](docs/INSTALL.md)**。

## MCP Server 接入

### 方式一：stdio（默认，用于 Claude Desktop / Claude Code）

`~/Library/Application Support/Claude/claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "memvault": {
      "command": "/path/to/memvault-mcp",
      "args": ["--db", "~/.memvault/data.db"],
      "env": { "OPENAI_API_KEY": "sk-..." }
    }
  }
}
```

### Claude Code

```bash
claude mcp add memvault /path/to/memvault-mcp -- --db ~/.memvault/data.db
```

### 方式二：SSE 网络传输（支持多客户端同时连接）

```bash
# 启动 MCP SSE Server
memvault-mcp --transport sse --port 8080

# 客户端通过 http://127.0.0.1:8080/mcp 连接
# 支持任意 MCP 兼容客户端（Claude Desktop、Cursor 等）
```

SSE 模式特性：
- **多客户端**：多个 MCP 客户端可同时连接同一实例
- **Auto-Injection**：客户端初始化时自动触发 embedding 回填
- **网络访问**：可通过 HTTP 远程连接（默认仅限 localhost）

### 环境变量

| 变量 | 作用 | 默认值 |
|------|------|--------|
| `OPENAI_API_KEY` | 启用语义搜索 | (无,纯关键词模式) |
| `OPENAI_API_BASE` | Embedding API 地址 | `https://api.openai.com/v1` |
| `MEMVAULT_EMBEDDING_MODEL` | Embedding 模型名 | `text-embedding-3-small` |
| `MEMVAULT_EMBEDDING_DIM` | 向量维度 | `1536` |
| `MEMVAULT_DB` | SQLite 数据库路径 | `~/.memvault/data.db` |

## MCP Tools / Resources

### 8 个 Tools

| Tool | 说明 |
|------|------|
| `save_memory` | 保存记忆(auto-embedding) |
| `search_memory` | 搜索(keyword / semantic / hybrid) |
| `session_start` | 按 Agent 身份返回注入上下文 |
| `review_memory` | 审核:approve / reject / edit |
| `delete_memory` | 删除记忆 |
| `extract_memories` | 从文本提取结构化记忆 |
| `run_dedup` | 去重扫描 |
| `run_decay` | 衰减 + 自动归档 |
| `confirm_read` | 确认记忆已读(更新 access_count + last_read_at) |

### 2 个 Resources

| URI | 说明 |
|-----|------|
| `memory://user-profile` | MUST 级强制规则(启动自动加载) |
| `memory://project-context` | REFERENCE 级项目上下文 |

## CLI 命令

`save` · `search` · `list` · `delete` · `session-start` · `resource` · `extract` · `dedup` · `decay` · `export` · `import` · `confirm-read` · `sync`

详细用法见 `memvault-cli <command> --help`,或参考 `docs/PLAN.md`。

## 架构概览

```
┌────────────────────────────────────────────────┐
│  Clients                                       │
│  ┌──────────────┐ ┌──────────┐ ┌────────────┐  │
│  │ Claude Code  │ │ Cursor     │ │ 其它 MCP   │  │
│  └──────────────┘ └────────────┘ └────────────┘  │
└──────────────────┬───────────────────────────────┘
                   │ MCP (stdio / SSE / HTTP)
┌──────────────────▼───────────────────────────────┐
│  memvault-mcp     (rmcp 3.1.1)                   │
│  ┌──────────────┐ ┌────────────────┐ ┌────────┐ │
│  │  9 tools     │ │  2 Resources    │ │ SSE    │ │
│  │   + REST API │ │  + Auto-Inject │ │ Server │ │
│  └──────┬───────┘ └──────┬─────────┘ └────────┘ │
│         └────────┬───────┘                        │
│              ┌───▼────────┐                      │
│              │ Agent       │ (Agent Registry     │
│              │ Router      │  按类型/tag 过滤)   │
│              └───┬────────┘                      │
├──────────────────┼────────────────────────────────┤
│  memvault-core    │                                │
│  ┌──────────┐  ┌─▼───────┐ ┌────────────┐ ┌───┐ │
│  │ storage  │  │retrieval│ │ pipeline   │ │sync│ │
│  │ SQLite   │  │BM25+Vec │ │extractor   │ │   │ │
│  │          │  │RRF+同义 │ │dedup/decay │ │   │ │
│  │Embed回填  │  │词扩展+  │ │Export/Import│ │   │ │
│  │          │  │评分+软过│ │            │ │   │ │
│  └──────────┘  └─────────┘ └────────────┘ └───┘ │
└────────────────────────────────────────────────┘
```

## 文档索引

| 文档 | 内容 |
|------|------|
| [docs/DESIGN.md](docs/DESIGN.md) | v0.3 产品与架构设计(唯一权威) |
| [docs/PLAN.md](docs/PLAN.md) | v0.3 实施计划(Phase 0–10) |
| [docs/RECALL_PLAN.md](docs/RECALL_PLAN.md) | 召回率提升 7 项改进 |
| [docs/SYNC_PLAN.md](docs/SYNC_PLAN.md) | 零入侵多 Agent 文件同步方案 |
| [docs/INSTALL.md](docs/INSTALL.md) | 安装手册(CLI/MCP/Docker/Dashboard/VS Code/Obsidian) |
| [docs/DOCKER.md](docs/DOCKER.md) | Docker 部署 |
| [CHANGELOG.md](CHANGELOG.md) | 变更日志 |
| [CONTRIBUTING.md](CONTRIBUTING.md) | 贡献指南 |
| [SECURITY.md](SECURITY.md) | 安全漏洞报告 |
| [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) | 社区行为准则 |
| [.env.example](.env.example) | 环境变量配置参考 |

## 项目结构

```
MemVault/
├─ crates/                      # Rust workspace
│  ├─ memvault-core/            # 核心库(12 模块)
│  ├─ memvault-cli/             # CLI
│  └─ memvault-mcp/             # MCP Server
├─ dashboard/                   # Tauri 2.0 桌面应用
├─ vscode-extension/            # VS Code 扩展
├─ obsidian-plugin/             # Obsidian 插件
├─ docs/                        # 设计 / 计划文档
├─ agents.example.yaml          # Agent Registry 示例配置
├─ mcp-config.json              # MCP 配置示例
└─ .github/                     # Issue 模板、PR 模板、CI
```

更多细节见 `docs/DESIGN.md` 第 5–9 章。

## 路线图

- [x] Phase 1 — Core Engine + MCP Server + CLI
- [x] Phase 2 — 混合检索(关键词 + 向量 + RRF)
- [x] Phase 3 — Tauri Dashboard 记忆管理 UI
- [x] Phase 4 — 自动 Embedding + 智能管道
- [x] Phase 5 — 多端生态(VS Code / Obsidian)
- [x] Phase 6 — 召回率 7 项优化(词级分词/多字段/同义词扩展/评分/软过滤/跨namespace/Embedding回填)
- [x] Phase 7 — 多 Agent 文件同步(`memvault sync --watch`)
- [x] Phase 8 — MCP Proxy(SSE Server + Auto-Injection)
- [ ] Phase 9 — 检索增强二期(Rerank / Inbox 审核 / 遵循度追踪)
- [ ] Phase 10 — Web App + CRDT 跨端同步

完整阶段说明见 [`docs/PLAN.md`](docs/PLAN.md)。

## 测试

```bash
cargo test                     # 130 tests (118 unit + 12 E2E)
cargo clippy --all-targets      # 静态检查(零 warning)
cargo fmt --all -- --check      # 格式检查
cargo llvm-cov --lib            # 覆盖率报告(core 89.17%)
```

## 贡献与社区

- 🐛 Bug: [Issue Tracker](https://github.com/user/memvault/issues/new?template=bug_report.yml)
- 💡 想法: [Feature Request](https://github.com/user/memvault/issues/new?template=feature_request.yml)
- 💬 讨论: [GitHub Discussions](https://github.com/user/memvault/discussions)
- 📖 详情: [CONTRIBUTING.md](CONTRIBUTING.md)
- 🔒 安全: [SECURITY.md](SECURITY.md)

## 许可

依据 [MIT License](LICENSE) 发布。