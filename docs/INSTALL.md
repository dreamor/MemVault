# 安装指南

本文档覆盖 MemVault **所有组件**的安装路径。请按需选择：

| 组件 | 作用 | 推荐安装方式 |
|------|------|--------------|
| CLI + MCP Server | 命令行工具 / MCP stdio Server | 从源码构建 / Docker |
| Tauri Dashboard | 桌面管理界面 | 从源码构建(需要 Node.js 20+) |
| VS Code 扩展 | 编辑器内存取记忆 | VS Code Marketplace |
| Obsidian 插件 | 笔记软件内管理 | BRAT(Beta Reviewers Auto-update) |

---

## 1. 核心:CLI + MCP Server

### 1.1 前置依赖

| 依赖 | 必需性 | 版本要求 | 说明 |
|------|--------|----------|------|
| **Rust 工具链** | 必需 | 1.83+ stable | `rustup install stable` |
| **C 编译器** | 必需 | C11 | macOS 自带 Xcode CLT,Linux `build-essential` / Debian `build-essential`,Windows MSVC |
| **pkg-config** | 必需 | 任意 | Linux 用于定位 OpenSSL |
| **OpenSSL 开发库** | 推荐 | 1.1+ / 3.x | Linux `libssl-dev`,macOS `brew install openssl`,Windows vcpkg |
| **SQLite** | 可选 | 3.x | `rusqlite` 已启用 `bundled` 特性,系统无 SQLite 也可编译 |

> **macOS(Apple Silicon)**:首次构建建议先 `xcode-select --install`。
> **Linux 发行版速查**:
> - Debian / Ubuntu: `sudo apt install build-essential pkg-config libssl-dev`
> - Fedora / RHEL: `sudo dnf install gcc gcc-c++ pkgconfig openssl-devel`
> - Arch / Manjaro: `sudo pacman -S base-devel openssl pkgconf`
> - Alpine: `sudo apk add musl-dev pkgconfig openssl-dev`(需 MUSL 兼容补丁)

### 1.2 从源码构建(推荐)

```bash
git clone https://github.com/user/memvault.git
cd memvault
cargo build --release
```

> 二进制位置
> - `target/release/memvault-cli`
> - `target/release/memvault-mcp`

**加速构建**(重用本地已编译产物):

```bash
# 使用 mold 链接器(macOS,Linux)
cargo install mold --locked
RUSTFLAGS="-C link-arg=-fuse-ld=mold" cargo build --release

# 使用 sccache 编译缓存
cargo install sccache --locked
export RUSTC_WRAPPER=sccache
cargo build --release
```

**Cross-compile**: 见 `docs/cross.md`(附录)。

### 1.3 安装到 PATH

```bash
# macOS / Linux
install -m 0755 target/release/memvault-{cli,mcp} ~/.local/bin/

# 或用 cargo install(直接安装到 ~/.cargo/bin/)
cargo install --path crates/memvault-cli --locked
cargo install --path crates/memvault-mcp --locked
```

确保 `~/.local/bin` 或 `~/.cargo/bin` 在 `$PATH` 里(参考 PATH 配置)。

### 1.4 Docker 镜像

```bash
docker pull ghcr.io/user/memvault:latest
docker run --rm -it -v memvault-data:/home/memvault/.memvault ghcr.io/user/memvault:latest --help
```

构建本地镜像:

```bash
git clone https://github.com/user/memvault.git
cd memvault
docker build -t memvault:local .
```

> 数据卷 `/home/memvault/.memvault` 中保存 SQLite 与 Agent Registry。详细配置见 [`docs/DOCKER.md`](DOCKER.md)。

### 1.5 Homebrew(预告)

```bash
brew tap user/memvault
brew install memvault
```

> 当前尚未发布 tap,跟踪 `docs/PLAN.md` Phase 5。

### 1.6 验证安装

```bash
memvault-cli --version    # 应输出 memvault-cli 0.1.0
memvault-mcp --version    # 应输出 memvault-mcp 0.1.0
memvault-cli doctor       # 内置健康检查:DB / Embedding / 路径 / 权限
```

---

## 2. MCP Client 集成

CLI 与 MCP Server 安装完成后,**任选 1 节**配置你常用的 MCP 客户端。

### 2.1 Claude Desktop

编辑 `~/Library/Application Support/Claude/claude_desktop_config.json`(macOS)/
`%APPDATA%\Claude\claude_desktop_config.json`(Windows)/ `~/.config/Claude/claude_desktop_config.json`(Linux):

```json
{
  "mcpServers": {
    "memvault": {
      "command": "/absolute/path/to/memvault-mcp",
      "args": ["--db", "~/.memvault/data.db"],
      "env": { "OPENAI_API_KEY": "sk-..." }
    }
  }
}
```

> 路径**必须**为绝对路径,Claude Desktop 不解析 `~`。

重启 Claude Desktop,在「设置 → 开发者」处能看到 `memvault` Server 列出 8 tools / 2 resources 即视为联通。

### 2.2 Claude Code

```bash
claude mcp add memvault -- /absolute/path/to/memvault-mcp --db ~/.memvault/data.db
```

验证: `claude mcp list`,应看到 `memvault`。

### 2.3 Cline / Continue / Cursor

Cline / Continue / Cursor 都支持标准 `mcpServers` JSON,与 §2.1 配置格式一致,写入各自配置文件即可。

### 2.4 SSE / HTTP 远程 MCP

将 `--transport sse --bind 127.0.0.1:8765` 启动参数加入 Server,在客户端使用:

```json
{
  "mcpServers": {
    "memvault": {
      "url": "http://127.0.0.1:8765/sse"
    }
  }
}
```

---

## 3. Tauri Dashboard(可选)

### 3.1 前置依赖

| 依赖 | 版本 | 安装 |
|------|------|------|
| Node.js | 20+ | `nvm install 20` |
| pnpm | 8+ | `npm i -g pnpm` |
| Rust | 1.83+ | 同 §1.1 |
| WebView2 | Windows 10/11 | 系统自带 |
| WebKitGTK | Linux | `sudo apt install libwebkit2gtk-4.1-dev` |

### 3.2 启动开发模式

```bash
cd dashboard
pnpm install
pnpm tauri dev          # 启动 Vite + Tauri,首次会编译 Rust 端
```

应用窗口打开后,在「设置 → Server Connection」填入 `memvault-mcp` 的地址(默认 `http://127.0.0.1:8765/sse` 或 stdio)。

### 3.3 打包发布包

```bash
pnpm tauri build        # 输出: dashboard/src-tauri/target/release/bundle/{dmg,deb,msi,appimage}
```

---

## 4. VS Code 扩展

### 4.1 推荐方式:从 Marketplace 安装

1. 在 VS Code `扩展` 面板搜索 `memvault`
2. 点击安装 → 启用
3. 命令面板(⌘/Ctrl+Shift+P)执行 `MemVault: Set Server Path`

### 4.2 开发模式:从源码安装

```bash
cd vscode-extension
npm install
npm run compile
# VS Code 中按 F5 启动调试
# 或:
code --install-extension ./memvault-vscode-*.vsix
```

### 4.3 验证

打开 VS Code 侧边栏的 MemVault 图标,能列出记忆即视为联通。

---

## 5. Obsidian 插件

### 5.1 通过 BRAT 安装(推荐 Beta 渠道)

1. 在 Obsidian 社区插件中安装 `BRAT`
2. BRAT Settings → Add Beta Plugin → 填入仓库地址与版本号
3. 启用 `MemVault` 插件

### 5.2 从源码安装

```bash
cd obsidian-plugin
npm install
npm run build
mkdir -p <你的 vault>/.obsidian/plugins/memvault
cp main.js manifest.json styles.css <你的 vault>/.obsidian/plugins/memvault/
```

### 5.3 验证

Obsidian 设置 → Community plugins → 启用 `MemVault` → 侧边栏应出现图标。

---

## 6. 升级与卸载

### 6.1 升级

```bash
# 从
cd memvault && git pull && cargo build --release
# Docker 用户
docker pull ghcr.io/user/memvault:latest
```

升级前建议先 `memvault-cli export` 备份,升级后 `memvault-cli doctor` 检查 schema 兼容性(如有 breaking change,执行 `memvault-cli migrate`)。

### 6.2 卸载

```bash
# 二进制安装
cargo uninstall memvault-cli memvault-mcp
rm -rf ~/.memvault        # 数据
rm ~/.local/bin/memvault-{cli,mcp}

# Docker
docker rm -f memvault
docker volume rm memvault-data
```

VS Code / Obsidian 扩展在各自的扩展面板卸载。

---

## 7. 故障排查(v1.1 滚动)

详见 [`docs/TROUBLESHOOTING.md`](TROUBLESHOOTING.md)。常见快速覆盖:

| 症状 | 章节 |
|------|------|
| `failed to bind` | §1.1 端口 / 权限 |
| `OPENAI_API_KEY invalid` | §2 Embedding |
| MCP Server 连不上但二进制能跑 | §2.1 stdio 配置路径