# MemVault Dashboard

> React + TypeScript + Vite 的浏览器端控制台。
> 配套 [`memvault-mcp`](../../crates/memvault-mcp) 使用:通过 **REST API**(`--transport http`)连接,提供 Memory 浏览、检索、审核、可视化。无桌面壳、无按平台打包/签名/公证。

## 工程位置

```
dashboard/
├── src/                   # React 前端(Vite + TS + Vitest)
│   ├── App.tsx            # 单页 UI(5 个 Tab)
│   └── api.ts             # 唯一数据层:REST API 信封解包 + 字段映射
├── index.html
├── vite.config.ts         # 开发代理 /api → 127.0.0.1:3777
├── package.json           # name = "dashboard"
└── README.md              # 本文件
```

## 前置依赖

| 依赖 | 版本 | 说明 |
|------|------|------|
| **Node.js** | ≥ 20 | 前端构建 |

> 后端 `memvault-mcp` 需要 Rust(参见根文档 docs/INSTALL.md §1)与一个可写的 SQLite 数据库。

## 开发

先启动 REST 后端(端口默认 3777):

```bash
cargo build --release -p memvault-mcp
./target/release/memvault-mcp --db ~/.memvault/data.db --transport http --port 3777
```

再启动前端开发服务器:

```bash
cd dashboard
npm install
npm run dev
```

打开 `http://localhost:1420` 即可开发(开发服务器把 `/api`、`/health`、`/metrics`
代理到 `127.0.0.1:3777`,同源直达,无需手动配 CORS)。

## 生产运行

```bash
cd dashboard && npm ci && npm run build    # 生成 dashboard/dist/
./target/release/memvault-mcp --db ~/.memvault/data.db \
  --transport http --port 3777 --serve-web ./dashboard/dist
```

浏览器打开 `http://127.0.0.1:3777` —— 同一个端口同时托管前端静态资源与 `/api/*` 接口。

GitHub Release 的 `memvault-dashboard-<版本>.tar.gz` 就是 `dist/` 的打包,直接解压后
作为 `--serve-web` 的目录即可。

## 页面

| 页面 | Tab | 功能 |
|------|------|------|
| **Memories** | Memories | 卡片网格,按 namespace 过滤 + 分页;支持新建 / 编辑 / 删除 |
| **Search** | Search | 关键词 / 向量 / 混合三种检索模式切换,结果高亮 |
| **Review Queue** | Review | 待审记忆审批 approve / reject(与 CLI `memvault-cli review` 等价) |
| **Stats** | Stats | 记忆数、按 Layer/Agent 拆分、Pipeline 操作(promote/decay/dedup)、Compliance 汇总 |
| **Settings** | Settings | 后端连接状态、API Key 与 Agent ID 配置 |

## 架构

```text
┌────────────────────────────── Browser ─────────────────────────────┐
│  React 19 + Vite(纯静态,由 memvault-mcp --serve-web 托管)            │
│    src/api.ts — 唯一数据层:fetch(/api/*) + 信封解包                    │
└───────────────────────────────│─────────────────────────────────────┘
                                │ fetch /api/... (同源,免 CORS)
┌───────────────────────────────▼─────────────────────────────────────┐
│  memvault-mcp --transport http (axum)                                │
│    REST 路由:/api/memories /api/search /api/stats /api/inbox/...     │
│    + ServeDir 托管 dist/ 静态资源                                     │
└───────────────────────────────────┬─────────────────────────────────┘
                                    │ memvault-core
                                    ▼
                        SQLite(记忆) + LanceDB(向量)
```

## 测试

```bash
cd dashboard
npm test        # Vitest 单元测试(jsdom)
npm run build   # tsc --noEmit + vite build 类型检查
```

## 故障排查

参见仓库根 [`docs/TROUBLESHOOTING.md` §6](../../docs/TROUBLESHOOTING.md)(Web Dashboard 连接/SPA 路由/端口)。