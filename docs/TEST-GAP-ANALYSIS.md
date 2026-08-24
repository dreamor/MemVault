# Test Gap Analysis — 2026-08-24（已执行完毕）

基线审计 + 本轮「一口气补了」的执行结果。Rust/TS 全量测试通过，覆盖率较基线显著提升，
所有 P0/P1 缺口已关闭或明确留待 CI 阈值化。

## Baseline vs After

| Suite | Baseline | After |
|---|---|---|
| Rust workspace (`cargo test`) | 521 passed | **538 passed** (+17：upstream 3、handler 1、proxy main 6、mcp server 1、mcp main 4、sse 1、native_embedding 3)（新增 upstream/handler/server/main/sse/native_embedding 用例） |
| Dashboard (Vitest) | 21 passed | **32 passed**（api 6 函数 + App 交互 6） |
| Obsidian plugin | 14 passed | **31 passed**（client API + sync 全量，新增 `client.test.ts`） |
| VS Code extension | 8 passed | **18 passed**（新增 `extension.test.ts`，真实 HTTP server） |
| dsh plugin | 5 passed | **19 passed**（config 3 + mcp-client 6 + process-manager 5） |
| `cargo llvm-cov --workspace` | **90.95% line / 88.15% region** | **92.25% line / 94.11% region** |

## Rust 覆盖率变化（重点文件）

| File | Baseline line/region | After region/line |
|---|---|---|
| `memvault-proxy/src/upstream.rs` | 64.87% line | **92.00% line / 96.31% region** |
| `memvault-proxy/src/handler.rs` | 79.72% region | **86.72% region / 93.55% line** |
| `memvault-mcp/src/server.rs` | 78.44% region | **81.73% region / 89.10% line** |
| `memvault-core/src/native_embedding.rs` | 74.07% region | **80.74% region / 79.57% line** |
| `memvault-mcp/src/main.rs` | 0% | **33.85% region**（resolve_path/Args 已测；启动接管仍靠 smoke） |
| `memvault-mcp/src/sse_server.rs` | 0% | **94.06% region** |
| `memvault-proxy/src/main.rs` | 38.72% | **53.42% region** |
| `memvault-proxy/src/shutdown.rs` | 0% | 0%（信号处理，保持未测） |

## 本轮新增测试清单

### Rust
- `memvault-proxy/src/upstream.rs`：3 个 HTTP 往返集成测试（内存 fake MCP server → `UpstreamManager` 真实连接），
  覆盖 `connect_one` HTTP、`all_*`、`forward_*` 成功路径、死端口跳过、首失败索引回归。
- `memvault-proxy/src/handler.rs`：`test_http_roundtrip_resources_prompts_and_tools`（list/read resource、list/get prompt、forward tool save_memory）。
- `memvault-proxy/src/main.rs`：`resolve_path` / `Args` / `/mcp` 路由存在性。
- `memvault-mcp/src/server.rs`：`test_http_roundtrip_resources_and_read`（get_info/on_initialized/list_resources/read_resource/list_all_tools）。
- `memvault-mcp/src/main.rs`：`resolve_path` / `Args` 4 测试。
- `memvault-mcp/src/sse_server.rs`：起真服 poll `/mcp` 非 404。
- `memvault-core/src/native_embedding.rs`：抽 `resolve_model_dir` 纯函数 + 3 测试。

### TypeScript
- `obsidian-plugin/src/client.test.ts`（新，31 测试）：api 信封、9 个 REST 方法、settings 合并、`syncVaultFromServer`。
- `vscode-extension/src/extension.test.ts`（新，18 测试）：真实 node http server 后端，覆盖 activate/tree providers/全部命令。
- `dsh-plugin/src/config.test.ts`、`src/mcp-client.test.ts`、`src/process-manager.test.ts`（新）：配置校验、MCP 会话/重连/资源、子进程生命周期。
- `dashboard/src/api.test.ts`：补 `getStats`/`approveMemory`/`rejectMemory`/`runPromote`/`runDecay`/`getComplianceSummary`。
- `dashboard/src/App.test.tsx`：stats 面板、compliance 错误态、promote/decay/dedup 管线按钮、approve/reject 交互。

## 顺手修的 bug

- **`memvault-proxy/src/upstream.rs` `connect_one`**：`RunningService` 在分支结束被 drop，peer 立即 `TransportClosed`，
  生产环境上游转发一直挂。修复：`UpstreamConnection` 新增 `_service` 字段保活。
- **`connect_all` 索引错位**：原按 `defs` 的 enumerate idx 注册索引，前序连接失败会导致后续越界。改用 `connections.len()`。

## 剩余可做（非阻塞）

- `memvault-proxy/src/shutdown.rs` 0%：信号处理难以单测，交给 smoke/手动验证。
- `memvault-mcp/src/main.rs` 启动接管（~37% line）：可仿 `memvault-cli/tests/smoke.rs` 加二进制 smoke。

## CI 覆盖率门禁（已接）

`.github/workflows/ci.yml` 新增 `coverage` job：`cargo llvm-cov --workspace --all-features`
并强制 line ≥92% / region ≥90% / function ≥85%（当前 92.25/94.11/89.82）。fastembed 构建期
ORT 下载偶发抖动，job 内带一次重试兜底（重试走 target/ 缓存，速率等同本地）。新增/改动模块
仍以 ≥80% line 为仓库内 bar（当前 TOTAL 92.25%）。
