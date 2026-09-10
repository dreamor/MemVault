# MemVault 运维手册 (Runbook)

本文档面向服务器端部署 MemVault 的场景。CLI 的日常使用见 [README](../README.md#cli-命令)，安装步骤见 [INSTALL.md](INSTALL.md)。

## 架构与可执行文件

MemVault 发布 3 个二进制：

| 二进制 | 说明 |
|--------|------|
| `memvault-cli` | 命令行管理工具（save / search / sync / backup 等 23 个子命令，含 doctor / outcome / supersede / import-skills / checkpoints / restore / status / review） |
| `memvault-mcp` | MCP Server（stdio / SSE / REST 三种传输模式） |
| `memvault-proxy` | MCP 透明代理（上游 MCP 合并 + 记忆注入 + 遵循度追踪） |

<!-- AUTO-GENERATED: 启动模式 / 端口 / 健康检查 / REST 端点 源自 rest_api.rs、sse_server.rs 与各 main.rs；请勿手改，改动代码后用 update-docs skill 重新生成 -->
## 启动模式与端口

### memvault-mcp

| 模式 | 命令 | 端口 / 端点 |
|------|------|-------------|
| stdio（默认） | `memvault-mcp --db ~/.memvault/data.db` | 标准输入输出 |
| SSE（多客户端） | `memvault-mcp --transport sse --port 3777` | `http://127.0.0.1:3777/mcp` |
| REST API | `memvault-mcp --transport http --port 3777`（同义 `rest`） | `http://127.0.0.1:3777/` |
| REST + Web Dashboard | `memvault-mcp --transport http --port 3777 --serve-web ./dashboard/dist` | 前端 + `/api/*` 同端口托管 |

> 默认端口为 **3777**（`--port` 可覆盖）。DB 默认路径 `~/.memvault/data.db`，`--db` 可覆盖。
> `--serve-web <dir>` 把前端静态产物（`npm run build` 的 `dashboard/dist/`）直接托管在 REST 端口根路径（SPA 路由自动回退 `index.html`），同源访问免 CORS。

### memvault-proxy

| 模式 | 命令 | 端口 / 端点 |
|------|------|-------------|
| stdio（默认） | `memvault-proxy --config ~/.memvault/proxy.yaml` | 无 |
| SSE | `memvault-proxy --config ~/.memvault/proxy.yaml --transport sse --port 3778` | SSE / MCP |

Proxy 的传输、端口、DB、上游 MCP 列表统一在 `proxy.yaml` 配置（示例见 [proxy.example.yaml](../proxy.example.yaml)）。

---

## 状态文件与数据

| 路径 | 用途 | 备注 |
|------|------|------|
| `~/.memvault/data.db` | SQLite 数据库（记忆 / embedding 缓存 / `memory_history` 快照表） | **核心数据，必须持久化 / 备份** |
| `~/.memvault/agents.yaml` | Agent Registry（注入规则 / 可选 API Key） | 可选；缺省时使用默认 profile |
| `~/.memvault/proxy.yaml` | Proxy 配置（`memvault-proxy` 专用） | 可选 |

> Agent Registry 的 API Key 在 YAML 加载时自动做 SHA-256 哈希，配置中不留存明文。

---

## 健康检查与监控

### Health

```bash
# memvault-mcp（REST / SSE，默认端口 3777）—— 纯文本 "ok"
curl -s http://127.0.0.1:3777/health
# ok (HTTP 200)

# memvault-proxy（SSE，端口 3778）—— JSON 存活探针，不触碰数据库/MCP 会话
curl -s http://127.0.0.1:3778/health
# {"status":"ok","service":"memvault-proxy"} (HTTP 200)
```

> proxy 的 `/health` 是 dsh 桥接插件等启动器用于就绪探测的端点;它不读数据库、不产生副作用,可安全高频轮询。

### Metrics（Prometheus 文本格式）

```bash
curl -s http://127.0.0.1:3777/metrics
```

`/metrics` 由 `metrics-exporter-prometheus` 渲染，可直接被 Prometheus 抓取，用于 Grafana 面板与告警。MemVault 本身**不内置**告警推送，建议：

1. Prometheus 抓取 `/metrics`
2. 用 `up == 0` 检测服务存活
3. 业务指标（注入次数 / 遵循率 / Inbox 堆积量）按需配置阈值告警

---

## REST 端点参考

`memvault-mcp --transport http` 暴露以下端点（均基于 axum）：

| Method & Path | 说明 |
|---------------|------|
| `GET /health` | 存活探针 |
| `GET /metrics` | Prometheus 指标 |
| `GET /api/memories` | 列出记忆（`?namespace=`、`?limit=`、`?offset=` 分页） |
| `GET /api/stats` | 聚合统计（total / MUST-REF 计数 / 各 Layer / agents / namespaces / skills） |
| `POST /api/memories` | 保存记忆（支持 `human_reviewed` / `ai_generated` 覆盖） |
| `DELETE /api/memories/{id}` | 删除记忆 |
| `PUT /api/memories/{id}` | 更新/编辑记忆 |
| `POST /api/search` | 检索（keyword / semantic / hybrid） |
| `POST /api/outcome` | 上报任务结果（情景记忆）；失败自动反思生成教训 |
| `GET /api/episodes` | 按 task_type / status / namespace / limit 过滤列出情景记录（含教训回链） |
| `POST /api/memories/{id}/supersede` | 旧事实归档并指向替代事实（不删除、可回滚） |
| `POST /api/session` | 按 Agent 身份注入上下文，返回 `inject_session_id` |
| `POST /api/extract` | 从自由文本提取结构化记忆 |
| `POST /api/dedup` | 去重扫描 |
| `POST /api/decay` | 衰减 + 归档 |
| `POST /api/promote` | 提炼管线（L1→L2→L3） |
| `POST /api/confirm-read` | 标记已读（更新 access_count） |
| `GET /api/inbox` | 待人工审核的记忆 |
| `POST /api/inbox/{id}/approve` | 批准 |
| `POST /api/inbox/{id}/reject` | 拒绝 |
| `POST /api/inbox/{id}/edit` | 编辑 |
| `GET /api/compliance/session` | 单次注入会话的遵循报告 |
| `GET /api/compliance/summary` | 聚合遵循率统计 |
| `GET /api/agents` | Agent Registry 列表（注入规则 / API Key 校验状态） |
| `GET /api/agents/import/scan` | 扫描可导入的 agent 配置 |
| `POST /api/agents/import/preview` | 导入前预览（试解析，不改写） |
| `POST /api/agents/import/run` | 执行 agent 配置导入 |
| `GET /api/capabilities` | 能力清单（供客户端探测） |
| `GET /api/doctor` | 自诊断检查（health + 关键路径探针） |
| `GET /api/export` | 按实体导出记忆（JSON / Markdown） |
| `POST /api/import` | 导入记忆（JSON / Markdown） |
| `POST /api/backup` | SQLite 一致性快照备份（等价 `memvault-cli backup`） |
| `GET /api/checkpoints` | 全局变更历史（分页 / 按 memory_id 过滤） |
| `POST /api/checkpoints/{history_id}/restore` | 恢复到指定历史快照 |
| `GET /api/memories/{id}/checkpoints` | 单条记忆的变更历史 |
| `POST /api/skills/import` | 导入 skills 配置 |

> 若某 Agent 在 `agents.yaml` 配置了 `api_key`，对应请求需携带 `X-MemVault-Api-Key` 请求头方可鉴权通过（失败返回 `401`，资源不存在返回 `404`）；未配置的 Agent 不要求认证（向后兼容）。

---

<!-- AUTO-GENERATED -->

## 部署流程

### 方式 A：Docker（推荐）

```bash
# 1. 构建镜像（或从 registry 拉取）
docker build -t memvault:local .

# 2. 创建持久化数据卷
docker volume create memvault-data

# 3. 以 MCP stdio 模式运行（供 Claude Desktop / claude mcp add 接入）
docker run --rm -i \
  -v memvault-data:/home/memvault/.memvault \
  -e OPENAI_API_KEY=$OPENAI_API_KEY \
  memvault:local

# 4. 或以 REST / SSE 模式作为常驻服务
docker run -d --name memvault \
  -v memvault-data:/home/memvault/.memvault \
  -v $PWD/agents.yaml:/home/memvault/.memvault/agents.yaml:ro \
  -e OPENAI_API_KEY=$OPENAI_API_KEY \
  -e MEMVAULT_DB=/home/memvault/.memvault/data.db \
  memvault:local \
  memvault-mcp --transport http --port 3777
```

- 数据卷 `/home/memvault/.memvault` **必须挂载**，否则容器重启后数据丢失。
- 镜像内置 `tini` 入口，正确转发 SIGTERM 给 MCP 子进程。
- 以非 root 用户 `memvault`（uid 10001）运行。

### 方式 B：二进制 / 源码

```bash
cargo install --path crates/memvault-cli --locked
cargo install --path crates/memvault-mcp --locked
cargo install --path crates/memvault-proxy --locked

# 常驻服务示例（systemd / supervisord 包装即可）
memvault-mcp --transport http --port 3777
```

### 备份（升级前必做）

```bash
# 一致性快照（点对点时间点的 SQLite 备份）
memvault-cli backup --output ~/backups/memvault-$(date +%F).db

# 或按实体导出 / 导入（JSON / Markdown）
memvault-cli export --format json --output ~/backups/
memvault-cli import --format json --input ~/backups/xxx.json
```

---

## 常见问题与处理

| 症状 | 排查 / 处理 |
|------|-------------|
| `failed to bind` / 端口占用 | 确认 3777（MCP）/ 3778（Proxy）未被占用；`lsof -i :3777` |
| `OPENAI_API_KEY invalid` | 检查环境变量是否正确注入；未配置时降级为纯关键词检索（功能正常但无语义搜索） |
| 运行数据丢失 | 容器未挂载 `-v memvault-data:/home/memvault/.memvault`；数据卷被重建 |
| `permission denied`（数据卷） | host 上卷属主为 root；`docker run --user $(id -u):$(id -g) ...` 或先 root 创建再 `chown` |
| MCP 客户端连不上 | stdio 需 `-i` 而非 `-t`；确认客户端能执行 `docker`/二进制路径为绝对路径 |
| SQLite 启动报错 | `rusqlite` 启用 `bundled`，无需系统 SQLite；若出现文件锁问题先检查是否有残留进程占用 `data.db` |
| 注入不生效 | 确认 `~/.memvault/agents.yaml` 存在且 profile 的 `id` 与连接 Agent 一致；MUST 级记忆不会被过滤 |
| 误改 / 误删某条记忆 | 用 `memvault-cli checkpoints --memory-id <id>` 定位后 `memvault-cli restore --history-id <n>` 单条回滚，无需整库恢复 |

---

## 回滚流程

按恢复粒度有两条路径，按需选择：

### 单条记忆回滚（轻量，无需整库）

```bash
# 查看某条记忆的历史，或全局最近变更
memvault-cli checkpoints --memory-id <memory_id>
memvault-cli checkpoints --limit 50

# 恢复到对应快照；若该记忆已被删除，会自动重建
memvault-cli restore --history-id <history_id>
```

> 每次 update/delete 都会在事务内把旧行快照进 `memory_history`，恢复动作本身再记一条新快照——「撤销的撤销」可以逐级回溯。

### 整库回滚（快照 / 镜像）

1. **停机**：停止容器 / 进程。
2. **恢复数据库**：用 `memvault-cli backup` 生成的快照覆盖 `~/.memvault/data.db`（或在 Docker 中重新挂载旧数据卷）。
3. **回退二进制 / 镜像**：`docker run ghcr.io/dreamor/memvault:<上一版本>` 或 `git checkout` 到上一 release 标签后重建。
4. **验证**：启动后 `curl /health` 返回 `ok`，`memvault-cli list` 能列出原有记忆。

> schema 变更通常向后兼容（`serde(default)`）；一旦出现无法读取，优先用旧版本二进制读取旧库并导出，再导入新库。

---

## 告警与升级建议

- 告警接入：Prometheus 抓取 `/metrics` → Alertmanager → 邮件 / IM（MemVault 不自带告警）。
- 升级前：`memvault-cli backup` + `export` 双保险。
- 安全更新：跟踪 [SECURITY.md](../SECURITY.md)，及时轮换泄露的 API Key。
