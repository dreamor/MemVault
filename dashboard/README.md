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

Dashboard **直接链接 `memvault-core` 并打开本地 SQLite 文件**，不通过 MCP Server 或 REST API。这是有意的取舍：作为单用户桌面查看/管理工具，直连本地文件比维护一套"本地 + 远程"双路径数据访问逻辑更简单。

窗口顶部 **Settings** 页可以修改要打开的数据库路径（默认 `~/.memvault/data.db`），但改动只在**重启应用后**生效——`AppState` 里的 `SqliteStore` 是启动时一次性打开的，不支持热切换。

**跨设备 / 多客户端共享记忆**（Claude Desktop、VS Code、Obsidian 等）请走 MCP Server：

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

VS Code / Obsidian 需要的是 REST API（`--transport http`），详见 [INSTALL.md §2.6](../../docs/INSTALL.md#26-rest-apivs-code--obsidian-客户端专用)。

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

## 5 个核心页面

| 页面 | Tab | 功能 |
|------|------|------|
| **Memories** | Memories | 卡片网格，按 namespace 过滤 + 分页；支持新建 / 编辑 / 删除 |
| **Search** | Search | 关键词 / 向量 / 混合三种检索模式切换，结果高亮 |
| **Review Queue** | Review | 待审记忆审批:approve / reject（与 CLI `memvault-cli review` 等价） |
| **Stats** | Stats | 记忆数、按 Layer/Agent 拆分、Pipeline 操作（promote/decay/dedup）、Compliance 汇总 |
| **Settings** | Settings | 本地数据库路径查看/修改(需重启生效) |

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