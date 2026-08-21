# 故障排查（Troubleshooting）

本指南按 **症状 → 原因 → 解决** 模式组织，标注错误码、版本与严重程度。如果按步骤仍无法解决，请在 GitHub Issues 附上 `memvault-cli doctor --json` 的输出。

相关文档：

- 安装与构建：[INSTALL.md](INSTALL.md)
- 产品与架构：[DESIGN.md](DESIGN.md)

---

## 0. 紧急速查

| 症状 | 跳转 |
|------|------|
| `failed to bind` / `Address already in use` | §1.1 端口冲突 |
| `OPENAI_API_KEY invalid` / Embedding 失败 | §2 Embedding 配置 |
| MCP Server 连不上但 CLI 能跑 | §3 stdio 协议 |
| `permission denied` on `data.db` | §4 文件权限 |
| 升级后 `schema mismatch` | §5 数据库迁移 |
| Dashboard 连接失败 / 列表空 | §6 Web Dashboard |
| `agent_memory too large` 控制台告警 | §7 Token 预算 |
| `memvault sync` 后 AGENTS.md 未生效 | §8 零入侵同步 |
| `doctor` 提示 sqlite3 / openssl 缺失 | §9 编译/链接依赖 |

---

## 1. 启动与绑定

### 1.1 `failed to bind`（端口占用）

**症状**

```
memvault-mcp[ERROR] failed to bind 127.0.0.1:3777
thread 'main' panicked at ... Os { code: 98, kind: AddrInUse }
```

**原因**：默认端口 3777 被占用，或上一次进程处于 `TIME_WAIT`。

**定位**

```bash
# macOS / Linux
lsof -iTCP:3777 -sTCP:LISTEN
sudo lsof -nP -i:3777

# Windows
netstat -ano | findstr :3777
```

**解决**

```bash
# A. 换端口（SSE / REST 仅支持用 --port 调整，无 --bind 参数）
memvault-mcp --transport sse --port 9876 --db ~/.memvault/data.db

# B. 杀掉残留进程
kill $(lsof -t -i:3777)        # macOS / Linux
taskkill /PID <pid> /F          # Windows（管理员）

# C. 等待 60 秒让 TIME_WAIT 过期后重启

# D. 反向代理暴露（SSE / REST 固定监听 127.0.0.1，无法改绑 0.0.0.0）
# 容器或远程访问时，用反向代理把 127.0.0.1:3777 暴露出去，而不是修改绑定地址
```

### 1.2 Windows：`link.exe not found`

**症状**：首次 `cargo build` 报

```
error: linker `link.exe` not found
note: the msvc targets require a linker
```

**解决**

1. 安装 [Visual Studio Build Tools 2022](https://visualstudio.microsoft.com/visual-studio-build-tools/)，勾选 **C++ build tools** + **Windows 10/11 SDK**
2. 启动「x64 Native Tools Command Prompt for VS 2022」，在该 shell 中执行 `cargo build`
3. 切到 GNU 工具链（免装 Visual Studio）：

```bash
rustup default stable-x86_64-pc-windows-gnu
pacman -S mingw-w64-x86_64-gcc          # MSYS2
cargo build --release
```

### 1.3 Linux：`libssl.so.1.1 not found`

**症状**：运行时报 `libssl.so.1.1: cannot open shared object file`。

**解决**：取决于发行版 glibc 版本。

```bash
# Debian 11 / Ubuntu 20.04
sudo apt install libssl3

# Debian 9 / Ubuntu 18.04 / 旧 RHEL
sudo apt install libssl1.1
# 或者下载 libssl1.1 deb 包手动安装:
# wget http://archive.ubuntu.com/ubuntu/pool/main/o/openssl/libssl1.1_1.1.1f-1ubuntu2_amd64.deb
# sudo dpkg -i libssl1.1_*.deb

# Alpine
sudo apk add openssl libssl1.1
```

MemVault 已在 `Cargo.toml` 把 `openssl` 标记为 `vendored`，下一版本起默认不再依赖系统 OpenSSL。上游版本升级前可临时设置环境变量强制走 vendored：

```bash
OPENSSL_STATIC=1 OPENSSL_VENDORED=1 cargo build --release
```

### 1.4 macOS：`dyld: Library not loaded: @rpath/libssl.3.dylib`

**症状**：运行 `memvault-mcp` 时 dyld 错误。

**解决**

```bash
brew install openssl@3
# 让 binary 能找到它
export DYLD_FALLBACK_LIBRARY_PATH="$(brew --prefix openssl@3)/lib:$DYLD_FALLBACK_LIBRARY_PATH"
```

需要持久化请把上面那行加到 `~/.zshrc`，Apple Silicon 路径在 `/opt/homebrew/opt/openssl@3/lib`，Intel 在 `/usr/local/opt/openssl@3/lib`。

### 1.5 Docker：`permission denied writing data.db`

参见 §4 — `/home/memvault/.memvault` 的所有权问题。

---

## 2. Embedding / OpenAI 接入

### 2.1 `OPENAI_API_KEY invalid`

**症状**

```
[embedding] request failed: 401 Unauthorized
error: OPENAI_API_KEY invalid
```

**核对清单**

1. `echo $OPENAI_API_KEY` 是否为空？是否含多余空白/换行
2. Key 是否已过期？Key 是否被吊销？
3. Key 是否为 OpenAI 直连？是否使用 Azure / 自托管？若是，见 §2.3
4. 余额是否耗尽（OpenAI 控制台 `Usage`）

**解决**

```bash
# 临时设置
export OPENAI_API_KEY="sk-proj-..."
memvault-cli search --query "Python" --top-k 5

# 持久化(写入 shell rc)
echo 'export OPENAI_API_KEY="sk-proj-..."' >> ~/.zshrc
```

如果使用本地代理（Zed/Cline 等不会传 env）：

```jsonc
// ~/Library/Application Support/Claude/claude_desktop_config.json
{
  "mcpServers": {
    "memvault": {
      "command": "/path/to/memvault-mcp",
      "args": ["--db", "~/.memvault/data.db"],
      "env": { "OPENAI_API_KEY": "sk-proj-..." }
    }
  }
}
```

### 2.2 `insufficient_quota`

**症状**：HTTP 429 + `You exceeded your current quota`。

**解决**：OpenAI 余额耗尽。

- 在 <https://platform.openai.com/account/billing> 充值
- 或切到纯关键词模式：`MEMVAULT_EMBEDDING_PROVIDER=none memvault-cli search --query "<关键词>"`

### 2.3 使用任意 OpenAI 兼容 provider（自托管 / Azure / vLLM / 网关等）

Embedding 默认本地 Ollama（`MEMVAULT_EMBEDDING_PROVIDER=ollama`）。要切换任意远端 API，统一走 OpenAI 兼容协议 `POST {base}/embeddings`：

```bash
export MEMVAULT_EMBEDDING_PROVIDER=openai-compatible   # 其他任意标识名亦可
export MEMVAULT_EMBEDDING_API_BASE=https://your-host/v1 # OpenAI / Azure / vLLM / 网关的兼容端点
export MEMVAULT_EMBEDDING_API_KEY=<key>                 # 无鉴权的服务可省略
export MEMVAULT_EMBEDDING_MODEL=text-embedding-3-small  # 或该 provider 的模型名
export MEMVAULT_EMBEDDING_DIM=1536                      # 须与 provider 实际输出维度一致
```

> Azure 需把 deployment 体现在 base 中（如 `https://<res>.openai.azure.com/openai/deployments/<dep>`）。默认未配置 `OPENAI_API_KEY` 但配置了 `MEMVAULT_EMBEDDING_PROVIDER=ollama` 时走本地，两者都不配置时自动探测本机 Ollama，未运行则降级纯关键词。

### 2.4 Embedding 维度与已有向量不匹配

**症状**

```
[embedding] dimension mismatch: db=1536 model=1024
```

这通常是先用了 `text-embedding-3-small`（1536 维），后改用 `bge-m3`（1024 维），但已有数据并未重算。**两种处理方式**：

```bash
# A. 切回原模型(简单)
export MEMVAULT_EMBEDDING_MODEL=text-embedding-3-small
export MEMVAULT_EMBEDDING_DIM=1536

# B. 全量重算并清空旧向量(彻底,但是慢)
memvault-cli db reinit --wipe-vectors
memvault-cli extract --text "..." --reembed
```

---

### 2.5 native 内嵌模型下载失败

**症状**：启动时 `WARN native embedding init failed — ... Failed to retrieve onnx/model.onnx`，随后降级为纯关键词模式。

**原因**：`provider=native` 首次使用会从 HuggingFace 下载模型(默认 `bge-small-zh-v1.5` ~95MB)；无法访问 `huggingface.co` 时(如国内网络)下载失败。

**解决**

1. 通过标准 `HF_ENDPOINT` 变量指向镜像(hf-hub 库自动读取)：

   ```bash
   export HF_ENDPOINT=https://hf-mirror.com
   ```

2. 重试即可，模型将下载到 `~/.memvault/models/`。
3. 或在代理环境设置 `HTTPS_PROXY` 后重试。
4. 确认 `~/.memvault/models` 目录可写。

> native 模型选择：默认中文 `bge-small-zh`(~95MB)；`MEMVAULT_EMBEDDING_MODEL=multilingual` 时用 `multilingual-e5-base`(~470MB，多语言)。

---

## 3. MCP 集成 / stdio 协议

### 3.1 Claude Desktop：看不到 memvault Server

**核对清单**

1. 配置文件路径是否正确：

   - macOS: `~/Library/Application Support/Claude/claude_desktop_config.json`
   - Windows: `%APPDATA%\Claude\claude_desktop_config.json`
   - Linux: `~/.config/Claude/claude_desktop_config.json`

2. `command` 是否为绝对路径？Claude Desktop 不展开 `~`，必须 `/Users/...`
3. 文件 JSON 是否合法（多余逗号、注释）？用 `jq . ~/.config/Claude/claude_desktop_config.json` 校验
4. CLI 直接启动能否工作？

   ```bash
   echo '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}' | \
     /path/to/memvault-mcp --db ~/.memvault/data.db
   ```

**重启 Claude Desktop**（不是关窗口 —— 完全退出再开），查看日志：

- macOS: `~/Library/Logs/Claude/mcp*.log`
- Windows: `%APPDATA%\Claude\logs\mcp*.log`

### 3.2 `Cannot find module @modelcontextprotocol/sdk`

Claude Code 检查：

```bash
claude mcp list         # 注册表状态
claude mcp add memvault /absolute/path/memvault-mcp -- --db ~/.memvault/data.db
```

### 3.3 stdio MCP 启动即退出

**症状**：启动后立刻 `ECONNRESET` / `进程退出`。

**原因**：MCP 用 newline-delimited JSON-RPC over stdio。若在 CLI 直接 `memvault-mcp` 会因 `EOF` 立刻退出（这是正常 MCP Server 行为）。应当通过 MCP Client 启动（Claude Desktop、Claude Code、Cline）。

自己调试时用 §3.1 的 `echo '…' | memvault-mcp` 模式。

### 3.4 Server 启动慢（>3s）

**症状**：Client 第一次调用耗时高。

**原因**：SQLite 首次打开需建 FTS5 索引；Embedding 模型首次加载。

```bash
# 看哪个慢
RUST_LOG=info memvault-mcp --db ~/.memvault/data.db 2>&1 | head -20
```

优化：

- 启用 `MEMVAULT_WAL=1`（已默认开）
- 预热：客户端启动后立刻调用一次 `search_memory` 触发索引加载
- 使用本地 Embedding 时把模型放到 SSD

---

## 4. 数据库与文件权限

### 4.1 `permission denied` on `data.db`

**症状**

```
sqlx: PoolError: PoolTimedOut ... os error 13 (permission denied)
```

**解决**

```bash
ls -la ~/.memvault/
# owner 应为当前用户
sudo chown -R $(id -u):$(id -g) ~/.memvault
chmod 600 ~/.memvault/data.db     # 防止同机用户读取
chmod 700 ~/.memvault             # 目录本身
```

### 4.2 Docker 卷权限错

```bash
# 容器内以 uid 10001（memvault 用户）运行,与 host UID 不同会导致 owner 漂移
docker run --rm -v memvault-data:/home/memvault/.memvault \
  --user $(id -u):$(id -g) \
  memvault:local memvault-cli doctor
```

如果之前误用了 root 写入：

```bash
docker run --rm -v memvault-data:/data alpine chown -R 10001:10001 /data
```

### 4.3 数据盘满

```bash
df -h ~/.memvault/
du -sh ~/.memvault/*.db ~/.memvault/cache/
```

清理：

```bash
memvault-cli decay --archive    # 归档低优先记忆
memvault-cli db vacuum          # 收缩 SQLite
```

---

## 5. 升级 / 迁移

### 5.1 升级后 `schema does not match`

**症状**：升级 binary 后启动报错 `Sqlite error: no such column: priority` 或类似。

**解决**：MemVault 通过 `memvault migrate` 做轻量迁移；如有破坏性变更需按 `CHANGELOG.md` 指引手动处理：

```bash
# 备份
memvault-cli export --format json --output ~/backup-$(date +%Y%m%d).json
# 升级 binary
cargo install --path crates/memvault-cli --locked --force
# 迁移
memvault-cli migrate
# 烟囱测试
memvault-cli search --query "smoke"
```

### 5.2 跨机器迁移

```bash
# 源端
memvault-cli export --format json --output bundle.json
memvault-cli export --format markdown --output ./memories/

# 目标端
memvault-cli import --format json --input bundle.json
memvault-cli import --format markdown --input ./memories/
```

向量字段不跨机器同步（语义搜索需重建），关键词检索立即可用：

```bash
memvault-cli db reindex
```

---

## 6. Web Dashboard

### 6.1 页面一直显示「Unreachable」

Settings 页会显示后端连接状态。若显示 Unreachable / 列表加载失败：

1. 确认 `memvault-mcp` 已用 `--transport http` 启动（而不是默认的 `stdio`）。
2. 确认端口一致：REST 默认 `3777`；前端开发服务器把 `/api` 代理到 `127.0.0.1:3777`（见 [INSTALL.md §3.2](INSTALL.md#32-启动开发模式)）。
3. 若 `--serve-web` 目录不存在，启动日志会输出
   `--serve-web: ... is not a directory; web dashboard not served` —— 检查指向的
   目录是否为 `npm run build` 产出的 `dashboard/dist/`。

### 6.2 SPA 路由刷新 404

`memvault-mcp --serve-web` 用 `ServeDir` 托管前端,对于未命中的路径会自动回退到
`index.html`。若刷新 `/memories/...` 出现 404,说明 `--serve-web` 没生效(见 6.1)
或代理配置把路径吞掉了。

### 6.3 `HMR` 频繁失败 / 端口占用

前端开发服务器默认端口 `1420`。若被占用:

```bash
# 找端口占用
ss -ltnp | grep 1420   # Linux
lsof -iTCP:1420 -sTCP:LISTEN
# 杀掉后重启 npm run dev
```

---

## 7. Token 预算与注入

### 7.1 `agent_memory too large`

**症状**：CLI 或 MCP 日志告警 `payload exceeds token_budget`。

**解决**

```bash
memvault-cli config set agent.token_budget 1500     # 默认 1500
memvault-cli config set agent.max_memories 8       # 默认 8
```

或在启动参数控制：

```bash
memvault-mcp --agent-token-budget 1500 --agent-max-memories 8 \
  --db ~/.memvault/data.db
```

### 7.2 注入质量差

MemVault 已内置 7 项召回优化（词级分词 / 多字段搜索 / 同义词扩展 / 相关性评分 / 软意图过滤 / 跨命名空间回退 / Embedding 自动回填）。若仍检索不到目标记忆，可放宽过滤并加大结果集重试：

```bash
memvault-cli search --query "<关键词>" --top-k 20 --namespace default
```

---

## 8. `memvault sync` 零入侵同步

**症状**：执行 `memvault sync` 后，Claude Code / Cursor / Cline 仍未读到他人的 AGENTS.md / CLAUDE.md。

**核对清单**

1. 检查文件是否真的写到了项目根目录：

   ```bash
   ls -la ./CLAUDE.md ./AGENTS.md ./.github/copilot-instructions.md
   git status   # 注意:你应该把 CLAUDE.md / AGENTS.md 提交进仓库才能被 Agent 读到
   ```

2. 各 Agent 默认读取路径：

   | Agent | 期望文件 | 是否要求 git tracked |
   |-------|---------|---------------------|
   | Claude Code | `./CLAUDE.md` 或 `~/.claude/CLAUDE.md` | 是（项目级） |
   | Cursor | `./.cursorrules` 或 `./AGENTS.md` | 视 Cursor 版本 |
   | Copilot | `./.github/copilot-instructions.md` | 是 |
   | Windsurf | `./AGENTS.md` | 是 |
   | Codex CLI | `./AGENTS.md` | 是 |

3. 如果 Agent 仍未生效，**完全重启客户端**（不是 reload window），重新打开会话。

### 8.1 生成目录被 .gitignore 忽略

如果项目里有 `.gitignore` 排除 `CLAUDE.md`，需要显式 `!CLAUDE.md` 添加反转规则。

### 8.2 关注文件没有触发 sync

```bash
# 强制全量同步
memvault-cli sync --all --namespace default

# 只同步 MUST 级别
memvault-cli sync --priority MUST

# 仅生成 AGENTS.md
memvault-cli sync --targets agents.md
```

---

## 9. 编译/链接依赖

### 9.1 `pkg-config` 找不到 openssl

```bash
# Debian / Ubuntu
sudo apt install pkg-config libssl-dev

# Fedora
sudo dnf install pkgconf-pkg-config openssl-devel

# macOS
brew install pkg-config openssl@3
export PKG_CONFIG_PATH="$(brew --prefix openssl@3)/lib/pkgconfig"
```

### 9.2 `linker not found` / `cc` 缺失

```bash
# Debian / Ubuntu
sudo apt install build-essential

# Fedora
sudo dnf install gcc gcc-c++

# Alpine
sudo apk add build-base

# macOS
xcode-select --install
```

### 9.3 `failed to read rusqlite`

确认未禁用 bundled 特性。`Cargo.toml` 中应包含 `rusqlite = { version = "0.32", features = ["bundled"] }`，若是自定义 feature set，需重新添加 `bundled`。

---

## 10. 收集诊断信息

提交 issue 前先跑：

```bash
memvault-cli doctor --json > doctor.json
rustc --version >> doctor.json
cargo --version >> doctor.json
uname -a >> doctor.json
```

附上：

- `doctor.json`
- 浏览器 / 终端 OS 版本
- 复现命令与日志（注意用 ```` ``` ```` 包裹，**不要**粘贴真实 API key）
- 是否能 `memvault-cli search --query "smoke"` 通过

`memvault doctor` 检查项：

```
[✓] SQLite WAL 正常
[✓] 数据目录可写: ~/.memvault/
[✓] MCP tool 数量: 8
[✓] MCP resource 数量: 2
[✓] (可选) Embedding API 联通
[✓] (可选) Agent registry 文件可解析
```

---

## 11. 已知非 Bug 行为

| 现象 | 解释 |
|------|------|
| `memvault-cli` 输出 ANSI 颜色 | 通过 `NO_COLOR=1` 关闭 |
| 启动时短暂打印 `not a key …` warning | SQLite 启动期 warning，无害 |
| Embedding 首次调用慢 | 模型懒加载；第二次调用不再加载 |
| MCP Server stdio 立即退出 | 正常行为；需要走 Client 启动 |
| DB 偶尔出现 `data.db-wal` 文件 | SQLite WAL 模式，运行时正常 |

---

## 12. 仍未解决？

1. 完整重置（保留数据）：

   ```bash
   mv ~/.memvault ~/.memvault.bak.$(date +%s)
   memvault-cli init
   memvault-cli import --format json --input ~/.memvault.bak.*/export.json
   ```

2. 完全重置（不保留数据）：

   ```bash
   rm -rf ~/.memvault/
   memvault-cli init    # 重新初始化
   ```

3. 提 issue：<https://github.com/dreamor/memvault/issues>，附 §10 的诊断信息
4. 安全相关：参见 [SECURITY.md](../SECURITY.md)，不要在公开 Issue 复现敏感问题。
