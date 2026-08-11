# Plan: MCP Proxy + Pre-Prompt Injection + Compliance Tracker

## Context

MemVault Phase 1–7 全部完成。剩余 Phase 8 (MCP Proxy + Pre-Prompt Injection) 和 Phase 10 (Compliance Tracker) 未实现。标准 MCP 协议中 Server 是被动的，无法拦截用户消息。通过 Proxy 架构解决：MemVault 作为中间层代理上游 MCP Server，同时注入记忆上下文并追踪遵循度。

## 实现方案

### 新建 crate: `crates/memvault-proxy/`

```
crates/memvault-proxy/
├── Cargo.toml
└── src/
    ├── main.rs          # CLI 入口（--config / --transport / --port）
    ├── config.rs        # ProxyConfig YAML 解析
    ├── upstream.rs      # UpstreamManager: 管理上游 MCP Server 连接
    ├── handler.rs       # ProxyHandler: ServerHandler 实现（核心）
    ├── merge.rs         # Tool/Resource/Prompt 合并策略
    ├── context.rs       # SessionContext: 观察 tool calls 推断意图
    ├── injection.rs     # InjectionEngine: 动态 Resource + 通知客户端刷新
    └── compliance.rs    # Compliance tools (report + query)
```

### 核心架构

```
MCP Client ←→ memvault-proxy (ServerHandler) ←→ Upstream MCP Servers (Peer<RoleClient>)
                     ├── MemVault Router (注入记忆)
                     └── ComplianceStore (追踪遵循度)
```

### 关键实现步骤

#### Step 1: 项目骨架 + Config
- 在 workspace `Cargo.toml` 添加 `memvault-proxy`
- 新建 `Cargo.toml`，启用 rmcp features: `client`, `transport-child-process`, `transport-streamable-http-client-reqwest`
- `config.rs`: 解析 `proxy.yaml`（upstreams 列表，支持 command/args 和 url 两种）

#### Step 2: UpstreamManager
- 对 `command` 类型：用 `TokioChildProcess` 启动子进程，`().serve(transport)` 获得 `Peer<RoleClient>`
- 对 `url` 类型：用 `StreamableHttpClientTransport` 连接
- 启动后缓存 upstream 的 tools/resources/prompts 列表
- 提供 `find_tool_owner(name)` / `forward_tool_call()` / `forward_read_resource()` 方法

#### Step 3: ProxyHandler (ServerHandler)
- `list_tools()`: 合并 MemVault 本地 tools + 所有 upstream tools
- `call_tool()`: 本地 tool 就地处理，upstream tool 转发
- `list_resources()`: 合并 `memory://` resources + upstream resources
- `read_resource()`: `memory://` 前缀本地处理，其余转发
- `list_prompts()` / `get_prompt()`: 添加 `memvault-context` prompt，其余转发

#### Step 4: Pre-Prompt Injection
- 新增动态 Resource `memory://session-inject`：内容根据 SessionContext 变化
- `SessionContext` 观察 tool calls，提取文件路径推断 project、从 tool 名推断 intent
- 上下文变化时调用 `router.session_start()` 更新注入内容
- 发送 `notifications/resources/updated` 通知客户端重新读取
- 注入文本包含 `inject_session_id` 供 compliance 关联

#### Step 5: Compliance Tracker
**memvault-core 新增** `crates/memvault-core/src/compliance.rs`:
- `ComplianceStore`: SQLite 表 `compliance_events`
- Schema: `id, inject_session_id, memory_id, priority, status, agent_id, evidence, timestamps`
- `ComplianceReport`: 聚合统计

**新增 2 个 MCP Tools**:
- `report_compliance`: Agent 报告遵循/违反
- `get_compliance_report`: 查询统计

#### Step 6: CLI + Transport
- stdio 模式：`proxy_handler.serve(rmcp::transport::stdio())`
- sse 模式：复用 `StreamableHttpService` 模式

### 修改的已有文件
- `Cargo.toml` (workspace root): members 添加 `crates/memvault-proxy`
- `crates/memvault-core/src/lib.rs`: 添加 `pub mod compliance;`
- `crates/memvault-core/src/router.rs`: 新增 `session_start_with_tracking()` 返回 inject_session_id

### 验证
1. `cargo build --workspace` 编译通过
2. `cargo test --workspace` 现有 100 tests 不回归
3. 新增测试验证 config 解析、merge 逻辑、compliance CRUD
4. 集成测试：proxy + mock upstream，验证 tool 转发和 resource 合并
