<div align="center">

# MemVault

### 每个 AI Agent 的共享记忆层

> 不是让 Agent 学会查记忆,而是让记忆自动出现在 Agent 面前。

**MCP 原生 &nbsp;·&nbsp; 混合检索 &nbsp;·&nbsp; 自动注入 &nbsp;·&nbsp; 零配置同步**

**开源 &nbsp;·&nbsp; 自托管 &nbsp;·&nbsp; 私有 &nbsp;·&nbsp; MIT 许可**

[![CI](https://img.shields.io/github/actions/workflow/status/dreamor/memvault/ci.yml?style=flat-square&branch=main)](https://github.com/dreamor/memvault/actions) [![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg?style=flat-square)](LICENSE) [![Rust](https://img.shields.io/badge/Rust-stable-orange.svg?style=flat-square)](https://www.rust-lang.org) [![MCP](https://img.shields.io/badge/MCP-compatible-blue.svg?style=flat-square)](https://modelcontextprotocol.io) [![Status](https://img.shields.io/badge/Status-Beta-yellow.svg?style=flat-square)](#项目状态)

[English](README.md) &nbsp;·&nbsp; **简体中文**

```bash
cargo install memvault-cli memvault-mcp
```

</div>

---

每个 AI Agent 的会话都是从零开始的。Claude Desktop 不知道 Cursor 刚刚学会了什么;你的编程助手每次开启新对话都会忘记你的偏好。

你一直在手动重复上下文——项目约定、个人偏好、历史决策——而这些本应让已经认识你的 Agent 自动知晓。这并非模型的局限,而是一层缺失的基础设施。

MemVault 就是这一层。它是一个轻量、自托管的记忆路由器,位于你的 Agent 与它们的上下文之间。任何接入 MemVault 的 MCP 兼容 Agent 都会自动共享同一份持久记忆——无需 SDK、无需 API 集成、无需改动代码。

**适合谁用:**

- **Claude Code / Claude Desktop 用户**,希望偏好、项目上下文和历史决策跨会话持久保存,无需重复
- **多 Agent 进阶用户**,同时在 Claude、Cursor、VS Code 插件和 Obsidian 之间切换——全部共享同一份记忆,无需配置
- **平台团队**,部署需要一致性的 AI 辅助工作流:代码评审约定、架构决策、项目专属偏好
- **任何不想把同一件事告诉 AI 两遍的人**——MemVault 的工作方式如同大脑本应的工作方式:你说一次,需要时它就在那儿

**[快速开始](#快速开始)** &nbsp;·&nbsp; **[工作原理](#工作原理)** &nbsp;·&nbsp; **[MemVault 能给你什么](#memvault-能给你什么)** &nbsp;·&nbsp; **[为什么选择 MemVault](#为什么选择-memvault)** &nbsp;·&nbsp; **[MCP 服务接入](#mcp-服务接入)** &nbsp;·&nbsp; **[CLI 命令](#cli-命令)** &nbsp;·&nbsp; **[集成](#集成)** &nbsp;·&nbsp; **[架构](#架构)** &nbsp;·&nbsp; **[项目状态](#项目状态)** &nbsp;·&nbsp; **[测试](#测试)** &nbsp;·&nbsp; **[文档](#文档)** &nbsp;·&nbsp; **[贡献与社区](#贡献与社区)** &nbsp;·&nbsp; **[许可证](#许可证)**

---

## 快速开始

```bash
# 安装
cargo install memvault-cli memvault-mcp

# 保存一条 MUST 级偏好(以指令形式注入,Agent 必须遵守)
memvault save --content "用户偏好 Python" --priority MUST --type preference \
  --instruction "代码用 Python,不用 Java" --tags "coding,python"

# 跨全部记忆检索(混合检索,配置嵌入后可启用语义检索)
memvault search --query "Python"

# 查看某个 Agent 接入时会注入哪些上下文
memvault session-start --agent-id claude-desktop --context "帮我写代码"

# 从自由文本中抽取结构化记忆
memvault extract --text "我喜欢深色模式。我们的项目用 Rust。" --save

# 根据记忆自动生成 Agent 指令文件
memvault sync

# 一站式:去重、衰减、归档过期记忆
memvault dedup && memvault decay
```

**5 秒验证安装是否成功:**

```bash
memvault-cli --version
# memvault 0.2.0

# 冒烟检查:列出已保存记忆(验证数据库正常)
memvault-cli list
```

<div align="center">

如果 MemVault 确实帮你解决了实际问题,一颗 Star 就能帮到更多人。

**[⭐ 在 GitHub 上点亮](https://github.com/dreamor/memvault)** &nbsp;·&nbsp; **[报告 Bug](https://github.com/dreamor/memvault/issues/new)**

</div>

---

## 工作原理

MemVault 是一条流水线,而不是单个脚本。下面每一级都是一个已发布的模块:

```
Agent 连接 (MCP stdio/SSE)
        │
        ▼
┌───────────────────────┐
│  Agent 路由            │  ← 匹配 Agent 类型/标签 → 过滤相关记忆
│  (Agent 注册表)         │
└─────────┬────────────┘
          │
          ▼
┌───────────────────────┐
│  记忆检索               │  ← 关键词 (BM25) + 向量 (嵌入) + RRF 融合
│  (3 种搜索模式)         │     同义词扩展 · 打分 · 软过滤
└─────────┬──────────────┘
          │
          ▼
┌───────────────────────┐
│  自动注入               │  ← MUST 级 → 指令提示词
│                        │     REFERENCE → 上下文资源
│                        │     NORMAL    → 搜索结果
└─────────┬──────────────┘
          │
          ▼
  Agent 收到上下文 ──→ 做出更好的决策
```

- **存储:** SQLite,内置 FTS5(全文搜索);embedding 以 int8 量化存储(约为 f32 的 1/4 体积且排序质量几乎不变,旧 f32 行仍可读取)
- **检索:** 基于 FTS5 的 BM25 关键词搜索,带 CJK bigram 分词(中文两字词可正确命中)与三档匹配降级(严格→放宽单字→同义词 OR,放宽必上报、绝不静默);本地优先的 embedding(默认 Ollama,可改用任意 OpenAI 兼容模型)、RRF 融合(每条结果附召回来源 kw#2/vec#5)、同义词扩展、相关度打分、软意图过滤
- **流水线:** 自动实体抽取、语义去重、基于时间的衰减、过期记忆归档
- **同步:** 零入侵文件生成——`memvault sync` 直接从数据库内容生成 CLAUDE.md、AGENTS.md 等

---

## MemVault 能给你什么

- **自动注入上下文:** 会话开始即按 Agent 身份自动拉取相关记忆——MUST 级规则以指令形式落地,而非仅作为聊天历史
- **混合检索:** BM25 + 向量 + RRF 融合,带同义词扩展、相关度打分与逐条召回来源留痕(哪一路、第几名召回了它)——可通过 CLI、MCP 工具或 REST API 调用
- **可解释注入:** 注入链路上每一条被丢弃的候选都记录原因(预算/上限/意图与类型惩罚)——「为什么这条记忆没进 Agent 上下文」永远有答案
- **MUST 强制约束:** MUST 优先级的记忆永不被过滤或截断。始终在上下文中,始终被遵守
- **多 Agent 感知:** Agent 注册表提供基于类型/标签的软过滤(降分,而非硬排除)
- **MCP 代理:** 透明代理,可向**任意**上游 MCP 服务器的响应注入记忆——客户端零改动
- **合规追踪:** `inject_session_id` 记录注入了什么,并度量指令遵守率
- **跨平台:** CLI + MCP Server(stdio 与 SSE)+ Tauri Dashboard + VS Code 插件 + Obsidian 插件
- **零侵入同步:** 按需从记忆生成 AGENTS.md / CLAUDE.md——无需为每个 Agent 改配置
- **历史与回滚:** 每次更新/删除都会快照进 `memory_history`——`memvault checkpoints` + `memvault restore` 即可单条回滚,不影响其它记忆
- **能力自检:** `memvault status` 明确列出未配置 embedding provider 时哪些功能会降级,并输出 schema 指纹(迁移版本+checksum)便于跨库比对
- **数据属于你:** 单一 SQLite 文件,完整导出/导入,无云端依赖。你的数据,在你的机器上

---

## 为什么选择 MemVault

| 能力 | 纯 CLAUDE.md | 向量库 + RAG | **MemVault** |
|---|---|---|---|
| **上下文注入** | 手工编辑 | 仅查询时 | 会话开始自动 |
| **多 Agent 共享** | 复制粘贴 | 各自索引 | 单一共享存储 |
| **MUST 强制执行** | 无 | 无 | 指令层注入 |
| **搜索模式** | 文件 grep | 仅嵌入 | BM25 + 向量 + 混合 |
| **同义词扩展** | 无 | 无 | 内置 |
| **去重** | 无 | 无 | 语义去重流水线 |
| **衰减 / 归档** | 无 | 无 | 基于时间 + 自动归档 |
| **MCP 原生** | 无 | 无 | stdio + SSE + Proxy |
| **Agent 区分** | 全局文件 | 查询过滤 | 类型/标签注册表 |
| **合规追踪** | 无 | 无 | inject_session_id + 完成率 |
| **自托管** | 是 | 视情况 | 单一二进制,无需云端 |

与任何 Agent 现有设定共存,而非替代。你的 LLM、你的 IDE、你的工作流保持原样。MemVault 只是在底层补上记忆层。

---

## MCP 服务接入

### stdio(Claude Desktop / Claude Code)

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

### SSE(多客户端、可网络访问)

```bash
memvault-mcp --transport sse --host 127.0.0.1 --port 3777
# 客户端连接地址: http://127.0.0.1:3777/mcp
```

SSE 特性:多客户端同时连接、初始化时自动触发嵌入向量回填、HTTP 远程访问。

### 13 个 MCP 工具

| 工具 | 说明 |
|------|------|
| `save_memory` | 保存并自动生成嵌入向量 |
| `search_memory` | 关键词 / 语义 / 混合 |
| `session_start` | 按 Agent 身份注入上下文 |
| `review_memory` | 批准 / 拒绝 / 编辑 |
| `delete_memory` | 删除一条记忆 |
| `extract_memories` | 从文本中结构化抽取 |
| `run_dedup` | 去重扫描 |
| `run_decay` | 衰减 + 自动归档 |
| `confirm_read` | 标记已读(更新 access_count) |
| `list_inbox` | 列出待人工审核的记忆 |
| `run_promote` | 提升流水线(L1→L2→L3),把来源归档到 L0 |
| `report_compliance` | 上报某次注入会话的遵循/违规状态 |
| `get_compliance_report` | 按会话或汇总的合规率 |

### 2 个 MCP 资源

| URI | 内容 |
|-----|------|
| `memory://user-profile` | MUST 级规则,连接时自动加载 |
| `memory://project-context` | REFERENCE 级项目上下文 |

### 环境变量

| 变量 | 用途 | 默认值 |
|------|------|--------|
| `MEMVAULT_EMBEDDING_PROVIDER` | 提供商:`native`(进程内推理,默认)、`ollama`/`local`、`openai`、`openai-compatible`(任意 OpenAI 兼容端点) | `native` |
| `OPENAI_API_KEY` / `MEMVAULT_EMBEDDING_API_KEY` | 远端提供商的 API Key(本地 Ollama 不需要) | (无,仅关键词) |
| `OPENAI_API_BASE` / `MEMVAULT_EMBEDDING_API_BASE` | 任意 OpenAI 兼容端点(OpenAI / Azure / vLLM / 网关…) | `https://api.openai.com/v1` |
| `MEMVAULT_EMBEDDING_MODEL` | 嵌入模型:默认 `bge-small-zh`(中文,~95MB)、`multilingual`/`e5-base` 多语言;API 提供商填具体模型名 | `bge-small-zh`(native)/ `text-embedding-3-small`(API) |
| `MEMVAULT_EMBEDDING_DIM` | 向量维度 | `768`(本地)/ `1536`(API) |
| `MEMVAULT_DB` | 数据库路径 | `~/.memvault/data.db` |
| `RUST_LOG` | 日志级别 | `info` |

---

## CLI 命令

`save` · `search` · `list` · `delete` · `session-start` · `resource` · `extract` · `dedup` · `decay` · `promote` · `backup` · `export` · `import` · `confirm-read` · `sync` · `checkpoints` · `restore` · `status`

```bash
memvault <命令> --help   # 每个命令的详细用法
```

### 常用命令

| 命令 | 作用 |
|---------|--------------|
| `save` | 保存一条记忆,支持优先级、类型、可选指令 |
| `search` | 混合检索 + 相关度打分,参数:`--query`、`--top-k`、`--namespace` |
| `session-start` | 模拟 Agent 接入时会收到的上下文 |
| `extract` | 解析自由文本,抽取结构化记忆 |
| `sync` | 根据记忆生成 AGENTS.md / CLAUDE.md(带 `--watch`) |
| `dedup` | 扫描并合并语义重复的记忆(配置了 embedding provider 时启用向量辅助去重) |
| `checkpoints` | 列出记忆历史快照(单条或全局);参数:`--memory-id`、`--limit` |
| `restore` | 按历史快照回滚单条记忆(`--history-id`) |
| `status` | 显示 embedding provider 就绪状态,以及缺失时哪些功能会降级 |
| `decay` | 基于访问新鲜度归档过期记忆 |
| `backup` | 创建一致的 SQLite 时间点备份 |
| `export` / `import` | 备份与恢复(JSON / Markdown) |
| `confirm-read` | 标记记忆已读(更新 access_count) |

---

## 集成

| 载体 | 状态 | 说明 |
|------|------|------|
| **Claude Desktop** | ✅ | 简单配置 MCP stdio,会话开始自动注入 |
| **Claude Code** | ✅ | `claude mcp add` 一行搞定 |
| **Cursor** | ✅ | MCP stdio 配置,与 Claude 共享记忆 |
| **任意 MCP 客户端** | ✅ | SSE 传输,多客户端同时连接 |
| **Tauri Dashboard** | ✅ Alpha | GUI 记忆管理(4 个页面) |
| **VS Code 插件** | ✅ Alpha | 侧边栏 + 搜索 + 右键保存 |
| **Obsidian 插件** | ✅ Alpha | 侧边栏 + 搜索 + 新建/编辑/删除 + 单向同步(DB→笔记) |
| **MCP Proxy** | ✅ | 透明代理,把记忆注入任意上游服务器 |
| **DeepSeek Harness (dsh)** | ✅ | 标准 MCP stdio 配置 — 详见 [docs/INSTALL.md §2.5](docs/INSTALL.md#25-deepseek-harness-dsh) |

---

## 架构

```
┌────────────────────────────────────────────────┐
│  客户端                                          │
│  ┌──────────────┐ ┌──────────┐ ┌────────────┐  │
│  │ Claude Code  │ │ Cursor   │ │ 其它 MCP   │  │
│  └──────────────┘ └──────────┘ └────────────┘  │
└──────────────────┬───────────────────────────────┘
                   │ MCP (stdio / SSE / HTTP)
┌──────────────────▼───────────────────────────────┐
│  memvault-mcp     (rmcp 3.1.1)                    │
│  ┌──────────────┐ ┌────────────────┐ ┌────────┐  │
│  │  13 个工具    │ │  2 个资源      │ │ SSE    │  │
│  │   + REST API │ │  + 自动注入     │ │ Server │  │
│  └──────┬───────┘ └──────┬─────────┘ └────────┘  │
│         └────────┬───────┘                        │
│              ┌───▼────────┐                       │
│              │ Agent      │ (Agent 注册表         │
│              │ 路由        │  类型/标签过滤)       │
│              └───┬────────┘                       │
├──────────────────┼────────────────────────────────┤
│  memvault-core    │                               │
│  ┌──────────┐  ┌─▼───────┐ ┌────────────┐ ┌───┐ │
│  │ 存储      │  │ 检索     │ │ 流水线      │ │同步│ │
│  │ SQLite   │  │BM25+向量│ │抽取器       │ │   │ │
│  │          │  │RRF+同义 │ │去重/衰减    │ │   │ │
│  │ 嵌入      │  │词       │ │导出/        │ │   │ │
│  │ 回填      │  │打分     │ │导入         │ │   │ │
│  └──────────┘  └─────────┘ └────────────┘ └───┘ │
└────────────────────────────────────────────────┘
```

---

## 项目状态

> v0.2.0 — 核心 + 检索 + Dashboard + 流水线 + 召回优化 + MCP Proxy + 合规 + 分层注入 + promote + 抽取。

| 模块 | 状态 | 说明 |
|------|------|------|
| `memvault-core` | ✅ v0.2.0 | 21 个模块: 存储、路由、检索、嵌入、去重、衰减、同步、查询扩展、鉴权、重排、提升、合规、能力报告 |
| `memvault-cli` | ✅ v0.2.0 | 18 个子命令(含 promote、backup、status) |
| `memvault-mcp` | ✅ v0.2.0 | MCP Server(rmcp 3.1.1)13 个工具 + 2 个资源 + SSE + REST API |
| `memvault-proxy` | ✅ v0.2.0 | 透明代理 + 注入 + 抽取闭环 + 合规 |
| Dashboard (Tauri 2) | ✅ Alpha | 4 个页面 |
| VS Code 插件 | ✅ Alpha | 侧边栏 + 搜索 + 右键保存 |
| Obsidian 插件 | ✅ Alpha | 侧边栏 + 搜索 + 新建/编辑/删除 + 单向同步(DB→笔记) |
| 召回优化(7 项) | ✅ 已完成 | 词级分词、向量扩展、打分、软过滤、跨命名空间、嵌入回填 |
| 同步(`--watch`) | ✅ 已完成 | 零侵入 Agent 文件生成 |
| 重排 / Inbox / 鉴权 | ✅ 已完成 | 多信号重排、REST inbox 端点、SHA-256 API Key 鉴权 |
| 合规追踪器 | ✅ 已完成 | `inject_session_id` 追踪 + 完成率 |
| 分层注入 (L0-L3) | ✅ 已完成 | MemoryLayer 枚举、溢出摘要、提升流水线 (L1→L2→L3) |
| 结构化技能 | ✅ 已完成 | SkillMeta: 触发 / 步骤 / 验证 / 版本 |
| 抽取闭环 | ✅ 已完成 | 代理 `notify_response` 工具、白名单抽取进 Inbox |
| 历史与回滚 | ✅ 已完成 | update/delete 快照进 `memory_history` + `checkpoints` / `restore` 命令 |
| 能力自检 | ✅ 已完成 | `memvault status` —— 无 embedding provider 时的降级自诊断 |
| 权威分层重排 | ✅ 已完成 | L2/L3 层 + `decision`/`procedure`/`gotcha` 标签加分;软提升非过滤,MUST 不受影响 |
| 核心测试覆盖率 | ✅ 90%+ | 427 个测试(核心 292 + MCP 57 + proxy 56 + CLI 22) |

### 路线图

- [x] 阶段 1 — 核心引擎 + MCP Server + CLI
- [x] 阶段 2 — 混合检索(关键词 + 向量 + RRF)
- [x] 阶段 3 — Tauri Dashboard
- [x] 阶段 4 — 自动嵌入 + 流水线
- [x] 阶段 5 — VS Code / Obsidian 生态
- [x] 阶段 6 — 召回优化
- [x] 阶段 7 — 多 Agent 同步(`memvault sync --watch`)
- [x] 阶段 8 — MCP Proxy(透明代理 + 前置注入 + 动态资源)
- [x] 阶段 9 — 鉴权 / 重排 / Inbox / 合规 / 基准
- [x] 阶段 9.5 — 分层注入 / MemoryLayer / SkillMeta / Promote / Extraction
- [x] 阶段 9.6 — 记忆历史(`memory_history`)+ `checkpoints`/`restore` + `status` 能力自检

---

## 测试

```bash
cargo test                      # 427 个测试
cargo clippy --all-targets      # 零告警
cargo fmt --all -- --check      # 格式检查
cargo llvm-cov --lib            # 覆盖率(核心 90%+)
```

---

## 文档

| 文档 | 内容 |
|------|------|
| [docs/DESIGN.md](docs/DESIGN.md) | 产品与架构设计 |
| [docs/INSTALL.md](docs/INSTALL.md) | 安装指南(全平台) |
| [docs/DOCKER.md](docs/DOCKER.md) | Docker 部署 |
| [docs/RUNBOOK.md](docs/RUNBOOK.md) | 部署 / 健康检查 / 回滚手册 |
| [docs/RELEASING.md](docs/RELEASING.md) | 发布流程——CI 自动化范围 vs. 需要手动完成的步骤(Marketplace 发布、Obsidian 插件提交、macOS 签名) |
| [CHANGELOG.md](CHANGELOG.md) | 版本历史 |
| [CONTRIBUTING.md](CONTRIBUTING.md) | 贡献指南 |
| [SECURITY.md](SECURITY.md) | 安全公告 |
| [.env.example](.env.example) | 环境变量参考 |

---

## 贡献与社区

- 🐛 **Bug:** [提交 Issue](https://github.com/dreamor/memvault/issues/new)
- 💡 **想法:** [功能建议](https://github.com/dreamor/memvault/issues/new)
- 📖 **指南:** [CONTRIBUTING.md](CONTRIBUTING.md)
- 🔒 **安全:** [SECURITY.md](SECURITY.md)

---

## 许可证

MemVault 基于 [MIT 许可](LICENSE) 发布。