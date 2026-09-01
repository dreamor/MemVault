# 使用 Docker 部署 MemVault

> 本镜像面向 CLI 与 MCP Server 的本地/服务端通用场景。

## 快速开始

### 构建

```bash
docker build -t memvault:local .
```

### 运行 CLI

```bash
# 数据卷持久化
docker volume create memvault-data

docker run --rm \
  -v memvault-data:/home/memvault/.memvault \
  memvault:local \
  memvault-cli --help

# 保存一条记忆
docker run --rm \
  -v memvault-data:/home/memvault/.memvault \
  memvault:local \
  memvault-cli save --content "用户偏好 Python" --priority MUST --type preference
```

### 以 MCP Server 方式启动（stdio）

> MCP stdio 协议要求容器 PID 1 由服务进程持有,并正确转发信号。
> 镜像默认入口已配置 `tini` 包装器。

```bash
# 交互式 stdio（供 Claude Desktop / claude mcp add 接入）
docker run --rm -i \
  -v memvault-data:/home/memvault/.memvault \
  memvault:local
```

如果是接入 Claude Desktop：

```json
{
  "mcpServers": {
    "memvault": {
      "command": "docker",
      "args": [
        "run", "--rm", "-i",
        "-v", "memvault-data:/home/memvault/.memvault",
        "memvault:local",
        "memvault-mcp",
        "--db", "/home/memvault/.memvault/data.db"
      ]
    }
  }
}
```

### 以 REST API + Web Dashboard 方式启动（http）

Web Dashboard 是纯静态前端,由 `memvault-mcp --transport http --serve-web` 在
同一端口托管(同源、免 CORS)。容器内需同时把 `dist/` 目录挂载进来:

```bash
# 1. 构建前端 dist/（本机执行）
cd dashboard && npm ci && npm run build

# 2. 把 dist/ 挂进容器并用 --serve-web 托管
docker run --rm -p 3777:3777 \
  -v memvault-data:/home/memvault/.memvault \
  -v "$PWD/dashboard/dist:/srv/dashboard:ro" \
  memvault:local \
  memvault-mcp --db /home/memvault/.memvault/data.db \
    --transport http --port 3777 --serve-web /srv/dashboard
```

浏览器访问 `http://127.0.0.1:3777` 打开 Dashboard。

> **注意**：`memvault-mcp` 的 SSE / REST **固定监听 `127.0.0.1`**，没有 `--bind` / `--host`
> 改绑能力。因此默认 bridge 网络下 `-p 3777:3777` 通常无法从宿主访问容器内服务
> （容器内回环地址不接收 eth0 流量）；如需对外提供 REST 服务，改用
> `--network host` 或经反向代理暴露。

> 注: 镜像基础命令默认是 `memvault-mcp`(stdio 模式),覆盖 REST/Web 时
> 显式传 `--transport http` 与 `--serve-web` 即可,无需改动镜像。

## 配置

### 挂载点

| 路径 | 用途 | 建议 |
|------|------|------|
| `/home/memvault/.memvault` | SQLite 数据 / Embedding 缓存 | **必挂载** |
| `/home/memvault/.memvault/agents.yaml` | Agent Registry(必须与 DB 同目录,程序在 `db` 所在目录查找 `agents.yaml`) | 推荐挂载 |

### 环境变量

| 变量 | 说明 |
|------|------|
| `OPENAI_API_KEY` | 启用语义搜索 |
| `OPENAI_API_BASE` | 自定义 Embedding Endpoint |
| `MEMVAULT_EMBEDDING_MODEL` | Embedding 模型名 |
| `MEMVAULT_DB` | 数据库绝对路径,默认 `/home/memvault/.memvault/data.db` |
| `RUST_LOG` | 日志级别,如 `info,memvault_core=debug` |

### 完整示例

```bash
docker run -d --name memvault \
  -v memvault-data:/home/memvault/.memvault \
  -v $PWD/agents.yaml:/home/memvault/.memvault/agents.yaml:ro \
  -e OPENAI_API_KEY=$OPENAI_API_KEY \
  -e RUST_LOG=info \
  -e MEMVAULT_DB=/home/memvault/.memvault/data.db \
  memvault:local
```

## 层优化策略

`Dockerfile` 已使用以下 BuildKit 特性最大化缓存命中率：

- **依赖 → 源码分层**：先复制 `Cargo.toml` + `Cargo.lock` + `crates/`,再触发 release 构建;源码修改只重编不影响依赖下载
- **`--mount=type=cache`**:`/usr/local/cargo/registry` 与 `/build/target` 跨构建保留,避免每次重新下载 crates.io 数据
- **`--locked`**:`cargo build --locked` 强制使用 `Cargo.lock` 锁版本,避免 CI/本地漂移
- **`debian-slim` + 非 root 用户**:精简运行时(未内置 embedding 模型,首次使用按需下载到 `~/.memvault/models`),以 `memvault`(uid 10001)运行,符合容器安全最佳实践
- **`tini` 入口**:正确转发 SIGTERM 给 MCP stdio 子进程,避免 CLI 客户端关闭时服务僵死

## 故障排查

| 问题 | 排查 |
|------|------|
| 写入数据丢失 | 是否忘了 `-v memvault-data:/home/memvault/.memvault`? |
| `permission denied` 数据卷 | host 上的卷 owner 可能是 root:`docker run --user $(id -u):$(id -g) ...` 或先用 root 创建再 `chown` |
| MCP 连不上 | 确认 `-i` 而非 `-t`;Claude Desktop 必须能访问 `docker` 命令 |
| Embedding 失败 | 检查 `OPENAI_API_KEY` 是否被镜像 build 包含(应使用 `docker run -e` 而非 ARG 注入) |
| 启动报 SQLite 错误 | 原镜像已 `bundled` SQLite,无需系统库;若启用 mysql/pg 后端则需对应客户端 |

## CI / Release 集成

`.github/workflows/ci.yml` 的 `docker-smoke` job 会构建本地镜像做冒烟测试但**不推送**;推送 ghcr.io 由 `release.yml` 在 `v*` tag 时执行。

推送 `v*` tag 时 `.github/workflows/release.yml` 的 `docker` job 会构建镜像并推送到
GitHub Container Registry:`ghcr.io/<repo>:<tag>` 与 `ghcr.io/<repo>:latest`(不含
`:main` 之类的 tag)。日常本地可直接构建镜像:

```bash
# 本地构建(与 release 同一份 Dockerfile)
DOCKER_BUILDKIT=1 docker build \
  --tag memvault:local .
```