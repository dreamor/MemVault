# MemVault Dashboard

> Tauri 2 + React + TypeScript 桌面端控制台。
> 配套 [`memvault-mcp`](../../crates/memvault-mcp) 使用：本地 SQL 数据 + SSE / stdio 接入 MCP Server，提供 Memory 浏览、检索、审核、可视化。

## 工程位置

```
dashboard/
├── src/                   # React 前端（Vite + TS）
├── src-tauri/             # Tauri Rust 后端（含 tray / 菜单 / IPC）
│   └── Cargo.toml         # crate name = memvault-dashboard
├── public/                # 静态资源
├── vite.config.ts
├── package.json           # name = "dashboard"
└── README.md              # 本文件
```

源码 import 相对路径以 `dashboard/` 为锚点；外部文档（[INSTALL.md](../../docs/INSTALL.md) §3）使用 `pnpm tauri dev` 工作目录。

## 前置依赖

| 依赖 | 版本 | 说明 |
|------|------|------|
| **Node.js** | ≥ 20 | 前端构建 |
| **pnpm** | ≥ 8 | 包管理（与 Cargo 区分） |
| **Rust** | ≥ 1.83 | Tauri Rust 后端编译 |
| **Tauri CLI** | 2.x | 由 `@tauri-apps/cli` 自动安装 |
| **WebView2**（Win10/11） | 系统自带 | Win11 自带；Win10 1809 以下需手动安装 |

**Linux 额外运行时**（Tauri WebKit）：

```bash
# Debian / Ubuntu
sudo apt install libwebkit2gtk-4.1-dev build-essential curl wget file \
                 libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev

# Fedora
sudo dnf install webkit2gtk4.1-devel openssl-devel curl wget file \
                 libappindicator-gtk3-devel librsvg2-devel
```

详见 [`docs/INSTALL.md` §3](../../docs/INSTALL.md)。

## 开发

```bash
cd dashboard
pnpm install
pnpm tauri dev
```

启动后，Vite 在 `http://localhost:1420` 提供 HMR；Tauri 启动原生窗口，第一个 build 会预热 Rust 端约 1–3 分钟。

## 配置 Backend 连接

Dashboard 不直接打开 SQLite，而通过 MCP Server 与 Core 通信。窗口左上角 **Settings → Server Connection** 填入：

- **Mode**: `stdio`（默认，spawn `memvault-mcp` 子进程）或 `sse`（连接到 独立运行的 MCP Server）
- **stdio command**: `cargo run -p memvault-mcp -- --db ~/.memvault/data.db`
- **sse endpoint**: `http://127.0.0.1:8765/sse`

配置文件等价于（参见 [INSTALL.md §2.4](../../docs/INSTALL.md)）：

```json
{
  "mcpServers": {
    "memvault": {
      "command": "memvault-mcp",
      "args": ["--db", "~/.memvault/data.db"]
    }
  }
}
```

## 打包

```bash
# 当前平台发布包
pnpm tauri build

# 产物路径
dashboard/src-tauri/target/release/bundle/
├── macos/         # .app
├── dmg/           # macOS 安装包
├── msi/           # Windows 安装包
├── deb/           # Debian / Ubuntu
├── appimage/      # Linux AppImage
└── rpm/           # Fedora / RHEL  (如启用)
```

跨平台构建需在对应 OS / 架构机器上分别执行（详见 `tauri-action` 集成参见 `docs/DOCKER.md`）。

## 4 个核心页面

| 页面 | 路由 | 功能 |
|------|------|------|
| **Memory List** | `/` | 卡片网格，按 namespace / priority / tag 过滤 |
| **Search** | `/search` | 关键词 / 向量 / 混合三种检索模式切换，结果高亮 |
| **Review Queue** | `/review` | 待审记忆审批:approve / reject / edit（与 CLI `memvault-cli review` 等价） |
| **Stats** | `/stats` | 记忆数、Embedding 缓存命中、按 Agent 拆分、衰减曲线 |

## 架构

```text
┌───────────────────────────── Browser Tab (WebView2 / WKWebView) ─────────────┐
│  React 19 + TanStack Query + Vite                                              │
│  ── 4 routes ──   MemoryList  Search  Review  Stats                            │
│                      │  invoke('mcp://tool_name', args)                         │
└──────────────────────┼────────────────────────────────────────────────────────┘
                       │ window.__TAURI_INTERNALS__.invoke
┌──────────────────────▼────────────────────────────────────────────────────────┐
│  Tauri Rust Backend (memvault-dashboard crate)                                  │
│  ── IPC commands: list_memories / search / save / approve / decay ──             │
│  ── 通过 memvault-core 客户端调用本地 memvault-mcp 或标准 SDK ──                │
└────────────────────────────────────────────────────────────┬────────────────────┘
                                                              │ stdio / SSE
                                                              ▼
                                                  memvault-mcp ─── memvault-core ─── SQLite + LanceDB
```

## 测试

```bash
pnpm test              # Vitest 单元
pnpm tauri dev         # 集成（手测）
```

前端类型检查：

```bash
pnpm tsc --noEmit
```

## 故障排查

参见根目录 [`docs/TROUBLESHOOTING.md` §6](../../docs/TROUBLESHOOTING.md)（Tauri Dashboard 启动、WebView 缺失、HMR 端口冲突等）。