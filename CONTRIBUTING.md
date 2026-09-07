# 贡献指南

感谢你对 MemVault 的关注！本文档说明如何参与本项目的开发。

## 开发流程

我们采用以 PR 为核心的协作模式：

1. **Fork** 本仓库并 clone 到本地
2. 从 `master` 拉取特性分支：`git switch -c feat/<short-desc>`
3. **先写测试**（TDD）：参见下文「开发约定」
4. 实现功能 / 修复 Bug
5. `cargo fmt` + `cargo clippy` + `cargo test` 全部通过
6. 推送分支并发起 PR

## 可用命令

<!-- AUTO-GENERATED: commands reference -->

### 核心 Rust Crate

| 命令 | 说明 |
|------|------|
| `cargo build --release` | 发布构建 CLI (`memvault-cli`) + MCP Server (`memvault-mcp`) + MCP Proxy (`memvault-proxy`) |
| `cargo build -p memvault-cli` | 仅构建 CLI |
| `cargo build -p memvault-mcp` | 仅构建 MCP Server |
| `cargo build -p memvault-core` | 仅构建核心库 |
| `cargo test` | 运行全部测试（~849 tests:core 580 + e2e 19, MCP 120, proxy 82, CLI 48） |
| `cargo test -- --nocapture` | 运行测试并显示 println 输出 |
| `cargo test -p memvault-core` | 仅运行核心库测试 |
| `cargo clippy --all-targets --all-features -- -D warnings` | Lint 检查（零 warning） |
| `cargo fmt` | 代码格式化 |
| `cargo llvm-cov --workspace --all-features` | 覆盖率门禁（CI:line ≥92% / region ≥90% / function ≥85%） |
| `cargo audit` | 安全审计（依赖 CVE 扫描） |

### 运行 MCP Server

| 命令 | 说明 |
|------|------|
| `memvault-mcp --transport stdio` | (默认) stdio 模式，用于 Claude Desktop |
| `memvault-mcp --transport sse --port 3777` | SSE 网络模式，多客户端支持（默认端口 3777） |
| `memvault-mcp --transport http --port 3777` | REST API 模式 |

### memvault sync

| 命令 | 说明 |
|------|------|
| `memvault-cli sync` | 生成 CLAUDE.md / AGENTS.md 等指令文件 |
| `memvault-cli sync --watch` | 轮询模式，检测数据库变更后自动重新生成 |
| `memvault-cli sync --dir /path/to/project` | 指定项目目录 |

### Web Dashboard（浏览器端）

| 命令 | 说明 |
|------|------|
| `cd dashboard && npm ci && npm run dev` | 启动 Vite 开发服务器（代理 /api → 127.0.0.1:3777） |
| `cd dashboard && npm run build` | TypeScript 检查 + 生产构建（输出 `dist/`） |
| `cd dashboard && npm run preview` | 预览 Vite 生产构建 |
| `cd dashboard && npm test` | 前端单元测试（Vitest） |

### VS Code 扩展

| 命令 | 说明 |
|------|------|
| `cd vscode-extension && npm run compile` | 编译扩展 |
| `cd vscode-extension && npm run watch` | 监视模式编译 |
| `cd vscode-extension && npm test` | 运行扩展单测（vitest） |

### Obsidian 插件

| 命令 | 说明 |
|------|------|
| `cd obsidian-plugin && npm run build` | 插件构建 |
| `cd obsidian-plugin && npm run watch` | 监视模式编译 |
| `cd obsidian-plugin && npm test` | 运行插件单测（vitest） |

### DeepSeek Harness 桥接插件（dsh-plugin）

| 命令 | 说明 |
|------|------|
| `cd dsh-plugin && npm install --legacy-peer-deps` | 安装依赖（peer deps 为 0.0.1-rc.1，需 `--legacy-peer-deps`） |
| `cd dsh-plugin && npm run build` | 构建桥接插件（`tsc --strict`，输出 `dist/`） |
| `cd dsh-plugin && npm run watch` | 监视模式编译 |
| `cd dsh-plugin && npm test` | 运行插件单测（vitest） |

> 该插件将 MemVault 记忆自动注入 dsh system prompt、并在回合结束时自动抽取。设计与真实 dsh 源码对照见 [docs/DSH-BRIDGE-DESIGN.md](docs/DSH-BRIDGE-DESIGN.md)。

### Docker

| 命令 | 说明 |
|------|------|
| `docker build -t memvault:local .` | 构建本地 Docker 镜像 |
| `docker run --rm memvault:local --help` | 查看 CLI 帮助 |

<!-- AUTO-GENERATED -->

### 环境变量

| 变量 | 必需 | 说明 | 默认值 |
|------|------|------|--------|
| `MEMVAULT_EMBEDDING_PROVIDER` | 否 | 提供商：`native`（进程内推理，默认）/ `auto`（优先本地 Ollama，未运行则回退 native）/ `ollama` / `local` / `openai` / `openai-compatible` / `none` | `native` |
| `MEMVAULT_EMBEDDING_MODEL` | 否 | 模型：native 可写 `zh`(默认) 或 `multilingual`；API 提供商填具体模型名 | `bge-small-zh`(native) / `text-embedding-3-small`(API) |
| `MEMVAULT_EMBEDDING_DIM` | 否 | Embedding 维度（native 自动探测，无需设置） | 自动 |
| `OPENAI_API_KEY` / `MEMVAULT_EMBEDDING_API_KEY` | 否 | 远端 API 的密钥（native 本地推理不需要） | — |
| `OPENAI_API_BASE` / `MEMVAULT_EMBEDDING_API_BASE` | 否 | 任意 OpenAI 兼容端点（OpenAI / Azure / vLLM / 网关） | `https://api.openai.com/v1` |
| `MEMVAULT_LLM_EXTRACTION_PROVIDER` | 否 | LLM 提取（`extract_memories(mode=llm)`/反思/关系抽取）：不设/`auto` 自动探测本机 Ollama，否则纯规则回退；`ollama`/`local`；`openai`/`openai-compatible`；`off`/`disabled`/`none` 强制纯规则 | 自动探测 |
| `MEMVAULT_RELATIONS` | 否 | `on` 时 `extract_memories(mode=llm)` 额外持久化 `supports`/`contradicts`/`sourced_from` 关系三元组 | 关闭 |
| `MEMVAULT_DELTA_WRITE` | 否 | save 时同命名空间先查重（近重复跳过、相似项合并残差），`off`/`0`/`false`/`disabled` 关闭；单次旁路用 `--force`/`force_insert` | 开启 |
| `MEMVAULT_CONTEXT_NGRAM_WINDOW` | 否 | 会话 n-gram 检索窗口：proxy 注入用最近多少轮观察构造按新近度加权的检索键 | `5` |
| `MEMVAULT_DB` | 否 | 数据库路径 | `~/.memvault/data.db` |
| `MEMVAULT_DB_POOL_SIZE` | 否 | SQLite 连接池大小 | `5` |
| `MEMVAULT_HOME` | 否 | 覆盖基础数据目录（模型缓存、DB 所在目录） | `~/.memvault` |
| `MEMVAULT_CORS_ORIGIN` | 否 | REST 模式 CORS 允许来源：逗号分隔 origin，或 `*` 放行所有（仅限可信网络） | 仅本机（localhost-only） |
| `RUST_LOG` | 否 | 日志级别 | `info` |

完整说明参见 [`.env.example`](.env.example)。

## 开发约定

### Rust 代码

- `cargo +stable fmt` 必须无 diff
- `cargo +stable clippy --all-targets --all-features -- -D warnings` 必须通过
- 测试覆盖：单元 + 集成（workspace 门禁:line ≥92% / region ≥90% / function ≥85%）
- 错误处理：可恢复错误走 `anyhow`，领域错误走自定义 `thiserror`，**禁止 `unwrap()`**（除非在测试或不可达分支）
- 公开 API 变更需同步 `docs/DESIGN.md` 对应章节

### Commit Message

遵循 [Conventional Commits](https://www.conventionalcommits.org/zh-hans/)：

```
<type>(<scope>): <description>

[optional body]

[optional footer(s)]
```

常用 `type`：`feat` / `fix` / `refactor` / `docs` / `test` / `chore` / `perf` / `ci` / `build`

### 文档

- `docs/DESIGN.md` 是唯一权威设计文档；架构/接口变更需同步更新
- 新增 `docs/*.md` 需在 `README.md` 文档索引表中登记

## 提交 PR 前自检

- [ ] 通过 `cargo fmt + cargo clippy + cargo test`
- [ ] 在 `CHANGELOG.md` 的 `[Unreleased]` 区段添加条目
- [ ] 涉及破坏性变更时在「BREAKING CHANGE」footer 注明
- [ ] 在新领域写入前先开 Issue 讨论（降低返工风险）

## 行为准则

请阅读 [CODE_OF_CONDUCT.md](.github/CODE_OF_CONDUCT.md)，所有互动均受其约束。

## 联系方式

- Bug / 需求：[GitHub Issues](https://github.com/dreamor/memvault/issues)
- 安全问题：参见 [SECURITY.md](SECURITY.md)（**勿**通过公开 Issue 报告）
- 设计与讨论：[GitHub Discussions](https://github.com/dreamor/memvault/discussions)