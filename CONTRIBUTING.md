# 贡献指南

感谢你对 MemVault 的关注！本文档说明如何参与本项目的开发。

## 开发流程

我们采用以 PR 为核心的协作模式：

1. **Fork** 本仓库并 clone 到本地
2. 从 `main` 拉取特性分支：`git switch -c feat/<short-desc>`
3. **先写测试**（TDD）：参见下文「开发约定」
4. 实现功能 / 修复 Bug
5. `cargo fmt` + `cargo clippy` + `cargo test` 全部通过
6. 推送分支并发起 PR

## 可用命令

<!-- AUTO-GENERATED: commands reference -->

### 核心 Rust Crate

| 命令 | 说明 |
|------|------|
| `cargo build --release` | 发布构建 CLI (`memvault-cli`) + MCP Server (`memvault-mcp`) |
| `cargo build -p memvault-cli` | 仅构建 CLI |
| `cargo build -p memvault-mcp` | 仅构建 MCP Server |
| `cargo build -p memvault-core` | 仅构建核心库 |
| `cargo test` | 运行全部测试（130 tests：118 unit + 12 E2E） |
| `cargo test -- --nocapture` | 运行测试并显示 println 输出 |
| `cargo test -p memvault-core` | 仅运行核心库测试 |
| `cargo clippy -- -D warnings` | Lint 检查（零 warning） |
| `cargo fmt` | 代码格式化 |
| `cargo llvm-cov --lib` | 覆盖率报告（core 89.17%） |
| `cargo audit` | 安全审计（依赖 CVE 扫描） |

### 运行 MCP Server

| 命令 | 说明 |
|------|------|
| `memvault-mcp --transport stdio` | (默认) stdio 模式，用于 Claude Desktop |
| `memvault-mcp --transport sse --port 8080` | SSE 网络模式，多客户端支持 |
| `memvault-mcp --transport http --port 3777` | REST API 模式 |

### memvault sync

| 命令 | 说明 |
|------|------|
| `memvault-cli sync` | 生成 CLAUDE.md / AGENTS.md 等指令文件 |
| `memvault-cli sync --watch` | 轮询模式，检测数据库变更后自动重新生成 |
| `memvault-cli sync --dir /path/to/project` | 指定项目目录 |

### Tauri Dashboard

| 命令 | 说明 |
|------|------|
| `cd dashboard && pnpm dev` | 启动 Vite 开发服务器 |
| `cd dashboard && pnpm build` | TypeScript 检查 + 生产构建 |
| `cd dashboard && pnpm tauri dev` | 启动 Tauri 桌面应用开发模式 |
| `cd dashboard && pnpm tauri build` | 打包桌面安装包（dmg/deb/msi） |
| `cd dashboard && pnpm preview` | 预览 Vite 生产构建 |
| `cd dashboard && pnpm tauri` | Tauri CLI 帮助 |

### VS Code 扩展

| 命令 | 说明 |
|------|------|
| `cd vscode-extension && npm run compile` | 编译扩展 |
| `cd vscode-extension && npm run watch` | 监视模式编译 |

### Obsidian 插件

| 命令 | 说明 |
|------|------|
| `cd obsidian-plugin && npm run build` | 插件构建 |
| `cd obsidian-plugin && npm run watch` | 监视模式编译 |

### Docker

| 命令 | 说明 |
|------|------|
| `docker build -t memvault:local .` | 构建本地 Docker 镜像 |
| `docker run --rm memvault:local --help` | 查看 CLI 帮助 |

<!-- AUTO-GENERATED -->

### 环境变量

| 变量 | 必需 | 说明 | 默认值 |
|------|------|------|--------|
| `OPENAI_API_KEY` | 语义搜索必需 | OpenAI API 密钥（未设置则降级为关键字搜索） | — |
| `OPENAI_API_BASE` | 否 | 自定义 Embedding API 端点 | `https://api.openai.com/v1` |
| `MEMVAULT_EMBEDDING_MODEL` | 否 | Embedding 模型名 | `text-embedding-3-small` |
| `MEMVAULT_EMBEDDING_DIM` | 否 | Embedding 维度 | `1536` |
| `MEMVAULT_DB` | 否 | 数据库路径 | `~/.memvault/data.db` |
| `RUST_LOG` | 否 | 日志级别 | `info` |

完整说明参见 [`.env.example`](.env.example)。

## 开发约定

### Rust 代码

- `cargo +stable fmt` 必须无 diff
- `cargo +stable clippy --all-targets --all-features -- -D warnings` 必须通过
- 测试覆盖：单元 + 集成（新增/修改模块 ≥80%）
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
- `docs/PLAN.md` 维护阶段路线图状态
- 新增 `docs/*.md` 需在 `README.md` 文档索引表中登记

## 提交 PR 前自检

- [ ] 通过 `cargo fmt + cargo clippy + cargo test`
- [ ] 在 `CHANGELOG.md` 的 `[Unreleased]` 区段添加条目
- [ ] 涉及破坏性变更时在「BREAKING CHANGE」footer 注明
- [ ] 在新领域写入前先开 Issue 讨论（降低返工风险）

## 行为准则

请阅读 [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md)，所有互动均受其约束。

## 联系方式

- Bug / 需求：[GitHub Issues](https://github.com/dreamor/memvault/issues)
- 安全问题：参见 [SECURITY.md](SECURITY.md)（**勿**通过公开 Issue 报告）
- 设计与讨论：[GitHub Discussions](https://github.com/dreamor/memvault/discussions)