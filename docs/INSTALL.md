# 安装指南

本文档覆盖 MemVault **所有组件**的安装路径。请按需选择：

| 组件 | 作用 | 推荐安装方式 |
|------|------|--------------|
| CLI + MCP Server | 命令行工具 / MCP stdio Server | 从源码构建 / Docker |
| Web Dashboard | 浏览器管理界面 | 从源码构建 / Release 静态包(需要 Node.js ≥22.7) |
| VS Code 扩展 | 编辑器内存取记忆 | VS Code Marketplace |
| Obsidian 插件 | 笔记软件内管理 | BRAT(Beta Reviewers Auto-update) |

---

## 0. 一键安装(推荐,不需要 Rust 工具链)

从 GitHub Releases 下载对应平台预编译二进制并自动校验 SHA-256:

**Linux / macOS**:

```bash
curl -fsSL https://raw.githubusercontent.com/dreamor/memvault/master/scripts/install.sh | bash
export PATH="$HOME/.memvault/bin:$PATH"
```

**Windows(PowerShell)**:

```powershell
powershell -ExecutionPolicy Bypass -File scripts\install.ps1
# 默认安装到 %LOCALAPPDATA%\memvault\bin
```

> `install.sh` / `install.ps1` 从最新 Release 拉取,归档附带 `.sha256` 且 Release 提供
> 汇总 `SHA256SUMS`,安装时强制校验哈希。macOS 若遇 Gatekeeper 拦预编译二进制,
> 右键"打开"一次即可(与 `brew` 相同来源的未签名 GitHub 二进制行为一致)。
> **Intel Mac(macOS x86_64)没有预编译包**(内嵌 ONNX Runtime 无该平台产物),请走
> §1 源码构建;`install.sh` 在 Intel Mac 上会给出同样提示。

---

## 1. 核心:CLI + MCP Server

### 1.1 前置依赖

| 依赖 | 必需性 | 版本要求 | 说明 |
|------|--------|----------|------|
| **Rust 工具链** | 必需 | 1.85+ stable（edition 2024） | `rustup install stable` |
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
git clone https://github.com/dreamor/memvault.git
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

**Cross-compile**: 跨平台编译指南尚未整理。

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
docker pull ghcr.io/dreamor/memvault:latest
docker run --rm -it -v memvault-data:/home/memvault/.memvault ghcr.io/dreamor/memvault:latest --help
```

构建本地镜像:

```bash
git clone https://github.com/dreamor/memvault.git
cd memvault
docker build -t memvault:local .
```

> 数据卷 `/home/memvault/.memvault` 中保存 SQLite 与 Agent Registry。详细配置见 [`docs/DOCKER.md`](DOCKER.md)。

### 1.5 Homebrew(预告)

```bash
brew tap dreamor/tap
brew install memvault
```

> tap 仓库(`dreamor/homebrew-tap`)已创建并推送 formula,但发布资产所在的主仓库当前仍为 private——`brew install` 在主仓库转 public 前会 404。详见 [docs/DISTRIBUTION-TODO.md](DISTRIBUTION-TODO.md)。

### 1.6 验证安装

```bash
memvault-cli --version    # 应输出 memvault 0.3.0
memvault-mcp --version    # 应输出 memvault-mcp 0.3.0
memvault-cli list         # 列出已保存记忆(验证 DB 正常)
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

重启 Claude Desktop,在「设置 → 开发者」处能看到 `memvault` Server 列出 16 tools / 2 resources 即视为联通。

### 2.2 Claude Code

```bash
claude mcp add memvault -- /absolute/path/to/memvault-mcp --db ~/.memvault/data.db
```

验证: `claude mcp list`,应看到 `memvault`。

### 2.3 Cline / Continue / Cursor

Cline / Continue / Cursor 都支持标准 `mcpServers` JSON,与 §2.1 配置格式一致,写入各自配置文件即可。

### 2.4 SSE / HTTP 远程 MCP

将 `--transport sse --port 3777` 启动参数加入 Server,在客户端使用:

```json
{
  "mcpServers": {
    "memvault": {
      "url": "http://127.0.0.1:3777/mcp"
    }
  }
}
```

### 2.5 DeepSeek Harness (dsh)

[DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness)(`dsh`)是 DeepSeek 官方开源的 Agent Harness,基于 **Cordis** 插件元框架构建("一切皆插件")。下面两种接入方式都已经**对照真实 dsh 源码与真实运行环境验证过**(不是猜测——详见 [`docs/DSH-BRIDGE-DESIGN.md`](DSH-BRIDGE-DESIGN.md)),按需求选一种。

**方式 A:零代码,只要工具能被调用**

dsh 原生提供 MCP 客户端插件 `@deepseek-ai/dsh-mcp-client`。**每个上游 MCP server 对应一个独立的插件实例**(不是像 Claude Desktop 那样的一份 `mcpServers` 列表),工具会被注册成 `mcp__<serverName>__<原始工具名>` 这样的名字(例如 `mcp__memvault__save_memory`)。在 dsh profile 目录(`$DSH_HOME/profiles/<name>/cordis.patch.yml`)里加一条:

```yaml
- insert:
    - id: memvault-mcp
      name: '@deepseek-ai/dsh-mcp-client'
      config:
        transport: stdio
        serverName: memvault
        command: /absolute/path/to/memvault-proxy   # 或 memvault-mcp
        args: []
```

或者连接一个已经在跑的 SSE 实例:

```yaml
- insert:
    - id: memvault-mcp
      name: '@deepseek-ai/dsh-mcp-client'
      config:
        transport: streamable-http
        serverName: memvault
        url: http://127.0.0.1:3778/mcp
```

> 注意 `insert:` 这层包装不能省——裸的 `- id: memvault-mcp ...` 是"覆盖已存在条目"的语义,对一个还不存在的 `id` 会直接报错 `patch: entry "memvault-mcp" not found` 并被跳过。
> `transport` 只有 `stdio` / `streamable-http` 两个值,**没有** `sse` 这个名字(跟 MemVault 自己 `--transport sse` 里的 `sse` 是两个不同层面的命名,容易混)。

**方式 B:深度集成,要自动注入 + 自动抽取**

方式 A 只能让 agent"看到"MemVault 的工具,MUST 级记忆要不要读、每轮回复要不要调 `notify_response`,仍然取决于 agent 自己的判断。如果想要 MUST 记忆**自动**出现在 system prompt 里、每轮结束**自动**触发抽取(不依赖 agent 主动配合),用仓库根目录的 [`dsh-plugin/`](../dsh-plugin/README.md)(`@memvault/dsh-memvault`)——一个真正的 Cordis 插件,直接挂 `ctx.systemPrompt.section()` 和 `session/event` 监听。完整设计与四个真实排查出的坑(patch 语义、embedding provider 环境变量泄漏、启动竞态、连接失败后的记忆化 bug)记录在 [`docs/DSH-BRIDGE-DESIGN.md`](DSH-BRIDGE-DESIGN.md) §7。

**安装(装进指定 dsh profile)**:在 `dsh-plugin/` 目录下执行 `npm install && npm run build`,再 `npx @deepseek-ai/dsh plugin --profile <name> add "$PWD"`——`dsh plugin add` 会把包自动写进该 profile 的 `dsh.profile.bundles`,默认配置(含 `cordis.patch.yml`)随包提供,即 `mode: spawn` + `embeddingProvider: native`。本地开发想覆盖字段(如把 `binaryPath` 指向本机编译的二进制),需在 profile 的 `cordis.patch.yml` 手写一条不带 `insert` 的**裸 id 覆盖 patch**,且要重写整个 `config`(覆盖是整体替换,不逐字段合并)。完整步骤见 [`dsh-plugin/README.md`](../dsh-plugin/README.md) 的 **Install** 一节。

> **一个两种方式都会踩的坑**:如果用 `command`/`binaryPath` 方式 spawn `memvault-proxy`/`memvault-mcp`,它会继承 dsh 自己进程环境里的 `OPENAI_API_KEY`/`OPENAI_API_BASE`(如果你给 dsh 配置了 OpenAI 兼容模型,这两个变量很可能已经设置了)——MemVault 会把这当成*自己的* embedding provider 凭据去调 OpenAI,拿到 401。方式 A 的 `env` 字段或方式 B 的 `embeddingProvider` 配置项都可以显式设成 `native`(走内嵌 fastembed 模型,离线,不需要任何 key)来避免这个问题。**这不再是必须手动规避的坑**:未显式设置 `MEMVAULT_EMBEDDING_PROVIDER` 时,MemVault 启动阶段会先用一次 embed 调用校验继承到的 key 是否真的能用,校验失败会自动降级到 `native`;显式设置 `embeddingProvider` 仍然是更明确、跳过一次网络校验的方式,继续推荐。

### 2.6 REST API(VS Code / Obsidian 客户端专用)

**VS Code 扩展和 Obsidian 插件不使用 MCP 协议**,而是通过 HTTP REST API(`/api/*`)与后端通信。这意味着它们对 transport 模式有一个容易被忽略的硬性要求:

| Transport | 挂载的端点 | VS Code / Obsidian 能用吗 |
|-----------|-----------|---------------------------|
| `stdio`(默认) | 无 HTTP 端点 | ❌ |
| `sse` | 仅 `/mcp`(MCP-over-HTTP) | ❌ |
| `http` / `rest` | 完整 REST 路由(`/api/*`) | ✅ |

启动方式:

```bash
memvault-mcp --db ~/.memvault/data.db --transport http --port 8080
```

VS Code(`memvault.serverUrl`)与 Obsidian(设置里的 Server URL)都默认指向 `http://127.0.0.1:8080`,与上面的启动参数对应。

**关键端点**(完整列表见根 README「MCP Server」章节):

- `GET /api/memories`、`POST /api/memories`(新建):embedder 可用时默认生成 int8 向量,响应含 `"embedded":bool`;`PUT /api/memories/{id}`(通用编辑,支持 content/priority/tags/namespace/layer/skill_trigger 等字段的部分更新)、`DELETE /api/memories/{id}`
- `POST /api/search`:支持 `mode`=`keyword`(默认)/`semantic`/`hybrid`;逐条返回 `search_mode` 与 `hit_sources`(如 `["kw#1","vec#1"]`,与 MCP `search_memory` 一致)
- `POST /api/extract`:响应 `{ memories, coverage }`,`coverage` 含 `input_lines`/`empty_lines`/`extracted_lines`/`no_signal_lines` 四桶(互斥且总和=输入行数)
- `GET/POST /api/inbox/*`(审核队列)
- `POST /api/dedup`、`POST /api/decay`、`POST /api/promote`
- `GET /api/compliance/session|summary`

**Admin 鉴权(可选)**:除了 `save_memory`/`search`/`session_start` 按各自的 `agent_id` 鉴权外,其余管理类接口(列表/删除/编辑/审核队列/dedup/decay/promote/compliance)统一按一个"admin" agent 身份鉴权,通过请求头传递:

```
X-MemVault-Agent-Id: admin      # 可省略,默认就是 "admin"
X-MemVault-Api-Key: <your-key>
```

如果 `agents.yaml` 里没有给 `admin` 配置 `api_key`,这些接口保持无鉴权(向后兼容现有部署)。要开启鉴权,在 `agents.yaml` 里加:

```yaml
agents:
  - id: admin
    agent_type: general-assistant
    description: "Dashboard / VS Code / Obsidian 管理操作"
    api_key: "your-secret-key"
```

VS Code 的 `memvault.apiKey` 设置项、Obsidian 设置里的 API Key 字段,都会作为 `X-MemVault-Api-Key` 发送。

**注入通路去重(可选)**:同一 Agent 可能同时经多条通路获得记忆——MCP `session_start` 工具、`memvault-proxy` 透明注入、`sync` 生成的指令文件——造成重复注入。可在 `agents.yaml` 里用 `inject_channel`(`mcp` / `proxy` / `sync`)指定该 Agent 的**唯一规范注入通路**,其余通路的自动注入会被跳过(设计依据见 `docs/PAPER-INSPIRATIONS.md` Feature F):

```yaml
agents:
  - id: claude-code
    agent_type: coding-assistant
    inject_channel: proxy   # 仅 proxy 透明注入对该 Agent 自动注入
    inject_rules:
      max_memories: 8
      token_budget: 1500
      priority_order: ["MUST", "REFERENCE"]
      namespace_filter: ["global", "project:*"]
```

省略 `inject_channel` 时所有通路均不限制(默认行为,完全向后兼容)。

---

## 3. Web Dashboard(可选)

### 3.1 前置依赖

| 依赖 | 版本 | 说明 |
|------|------|------|
| Node.js | ≥22.7 | 前端构建（vitest 4 要求） |

> 后端仍需按 §1 用 Rust 构建 `memvault-mcp`;Web Dashboard 是一个纯静态前端,无桌面壳、无按平台打包/签名/公证环节。

### 3.2 启动开发模式

先起 REST 后端:

```bash
cargo build --release -p memvault-mcp
./target/release/memvault-mcp --db ~/.memvault/data.db --transport http --port 3777
```

再起前端开发服务器(自带 `/api`、`/health`、`/metrics` 到 `127.0.0.1:3777` 的代理):

```bash
cd dashboard
npm install
npm run dev             # 打开 http://localhost:1420
```

### 3.3 生产运行:后端直接托管前端

构建前端静态产物,再由 `memvault-mcp --serve-web` 在 REST 端口直接托管(同源、免 CORS):

```bash
cd dashboard && npm ci && npm run build     # 产物: dashboard/dist/
./target/release/memvault-mcp --db ~/.memvault/data.db \
  --transport http --port 3777 --serve-web ./dashboard/dist
```

浏览器打开 `http://127.0.0.1:3777` 即可使用 Dashboard。GitHub Release 附带的
`memvault-dashboard-<版本>.tar.gz` 就是 `dist/` 的打包,可直接解压后作为 `--serve-web`
的目录。

---

## 4. VS Code 扩展

> **前置条件**:VS Code 扩展通过 REST API 通信,必须先按 [§2.6](#26-rest-apivs-code--obsidian-客户端专用) 启动 `memvault-mcp --transport http`,否则侧边栏会一直显示空列表 / 连接错误。

### 4.1 推荐方式:从 Marketplace 安装

1. 在 VS Code `扩展` 面板搜索 `memvault`
2. 点击安装 → 启用
3. 如果 REST 服务不在默认地址 `http://127.0.0.1:8080`:打开 VS Code 设置(⌘/Ctrl+,)搜索 `MemVault`,修改 `memvault.serverUrl`(以及需要 admin key 时的 `memvault.apiKey`)

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

### 4.4 常用命令

- **命令面板**(⌘/Ctrl+Shift+P,输入 `MemVault:`):Search Memories、Create Memory、Extract Memories from Selection、Show Stats、Export/Import Memories、Download Backup、Browse Checkpoint History、Run Dedup/Decay/Promote
- **编辑器右键菜单**(需先选中文本):Save Selection as Memory、Extract Memories from Selection
- **侧边栏树节点右键菜单**:Approve/Reject(仅 Inbox 待审条目)、Quick Edit(仅 Inbox)、Edit、Delete、Supersede with…、History(仅 Memories 列表)

---

## 5. Obsidian 插件

> **前置条件**:同 VS Code 扩展,Obsidian 插件也通过 REST API 通信,必须先按 [§2.6](#26-rest-apivs-code--obsidian-客户端专用) 启动 `memvault-mcp --transport http`。

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

### 5.4 常用命令

- **命令面板**(⌘/Ctrl+P,输入 `MemVault:`):Open Memory Panel、Search Memories、Search and Insert Memory、Save/Extract from Selection、Create Memory、Show Stats、Review Inbox、Sync Memories to Vault、Export/Backup/Import(Export Memories to Vault、Backup MemVault Database to Vault、Import Memories from Vault File)、Browse Checkpoint History、Run Dedup/Decay/Promote
- **侧边栏面板**(Memories / Inbox 两个 Tab):每条记忆的操作按钮 Approve/Reject(仅 Inbox)、Quick Edit(仅 Inbox)、Supersede、Edit、History、Delete
- Export/Backup 写入 vault 内 `<syncFolder>/_exports`、`<syncFolder>/_backups` 子目录(Obsidian 无系统级文件对话框);Import 通过文件选择器从 vault 内选取 `.json`/`.md` 文件

---

## 6. 升级与卸载

### 6.1 升级

```bash
# 从
cd memvault && git pull && cargo build --release
# Docker 用户
docker pull ghcr.io/dreamor/memvault:latest
```

升级前建议先 `memvault-cli backup` 备份,升级后 `memvault-cli list` 检查数据可正常读取。

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

## 7. v0.2.0 新功能使用指南

### 7.1 分层记忆 (MemoryLayer)

记忆现在有 L0-L3 四个层级：

| 层级 | 含义 | 自动分配 |
|------|------|---------|
| L3 | 核心画像(Persona) | MUST 级记忆 |
| L2 | 场景归纳(Scenario) | REFERENCE 级记忆 |
| L1 | 原子事实(Atom) | BACKGROUND / extract 产出 |
| L0 | 原始归档(Raw) | promote 后的源记忆 |

```bash
# 保存时指定 layer
memvault-cli save --content "用户偏好 Python" --priority MUST --layer L3

# 列表显示 layer
memvault-cli list
# [Must|L3] mem_xxx — 用户偏好 Python
```

### 7.2 Promote 自动提炼管线

将低层记忆自动归纳提升到高层：

```bash
# 运行 promote（L1→L2, L2→L3）
memvault-cli promote

# 自定义阈值（默认：3 个 L1 合并为 L2，2 个 L2 提升为 L3）
memvault-cli promote --min-l1 5 --min-l2 3
```

### 7.3 结构化 Skill

Skill 类型记忆支持 trigger/steps/verification：

```bash
memvault-cli save --content "部署流程" --type skill \
  --skill-trigger "deploy,发布,上线" \
  --skill-steps "build,test,push,verify" \
  --skill-verification "健康检查通过"
```

MCP Tool 调用：
```json
{
  "tool": "save_memory",
  "arguments": {
    "content": "部署流程",
    "type": "skill",
    "skill_trigger": "deploy",
    "skill_steps": ["build", "test", "push"],
    "skill_verification": "health check passes"
  }
}
```

### 7.4 Proxy Extraction 闭环

MCP Proxy 增加了 `notify_response` 工具，Agent 每轮回复后调用，自动提取记忆到 Inbox：

```json
{
  "tool": "notify_response",
  "arguments": {
    "response_text": "好的，我记住了你偏好使用 FastAPI 框架",
    "agent_id": "claude-code"
  }
}
```

提取策略：
- 白名单：仅提取 preference / fact / skill 类型
- 置信度阈值：≥ 0.6
- 单次上限：5 条
- 保存为 `human_reviewed=false`（需在 Inbox 审核）

### 7.5 分层注入

`session_start` 现在使用分层注入策略：
- MUST 记忆：全文注入（不变）
- REFERENCE 记忆：Token Budget 内全文注入，超出部分显示摘要
- 末尾提示："还有 N 条相关记忆可通过 search_memory 查询"

---

## 8. 故障排查(v1.1 滚动)

详见 [`docs/TROUBLESHOOTING.md`](TROUBLESHOOTING.md)。常见快速覆盖:

| 症状 | 章节 |
|------|------|
| `failed to bind` | §1.1 端口 / 权限 |
| `OPENAI_API_KEY invalid` | §2 Embedding |
| MCP Server 连不上但二进制能跑 | §2.1 stdio 配置路径
## 3. Agent 插件接入（第一批）

完整矩阵见 `docs/AGENT-PORTABILITY.md`。除上文的通用 MCP 配置外，现在支持一键安装的原生插件：

- **Claude Code**（推荐，满配）：`/plugin marketplace add dreamor/memvault`，然后 `/plugin install memvault@memvault`（两条分开发送）。SessionStart hook 自动注入记忆；`MEMVAULT_HOOK_EXTRACT=1` 开启会话结束自动抽取（草稿进 Review Inbox）；skills（recall/save/review/sync）与 `/memvault-review`、`/memvault-sync`、`/memvault-doctor` 命令随插件带出。环境变量：`MEMVAULT_AGENT_ID`（默认 `claude-code`）、`MEMVAULT_HTTP_URL`（默认 `http://127.0.0.1:3777`）、`MEMVAULT_BIN`（PATH 不可达时显式指到 `~/.memvault/bin/memvault-cli`）。
- **OpenCode**：把 `integrations/opencode/opencode.json` 模板合并进项目 `opencode.json`（`plugin` 指向 `integrations/opencode/plugins/memvault.mjs` 绝对路径）。
- **Codex**：`∩integrations/codex/README.md` 三步（config.toml MCP + `memvault sync` + custom prompts）。
- **Gemini CLI**：`gemini extensions install https://github.com/dreamor/memvault`。
- **Cursor / Windsurf / Cline / Continue / Zed / JetBrains / VS Code / Claude Desktop**：粘贴 `integrations/mcp-clients/` 对应片段；各引擎用 `MEMVAULT_AGENT_ID` 区分身份、共享同一记忆库。
