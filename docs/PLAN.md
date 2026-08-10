# MemVault 实施计划

> 基于 [DESIGN.md](DESIGN.md) 的产品与架构设计文档分析制定
> 日期：2026-08-07

---

## 一、文档分析总结

### 1.1 项目本质

MemVault 是一个 **AI Agent 记忆路由器**，核心价值在于解决记忆的"三层断裂"：

| 断裂 | 问题 | 解法 | 核心差异 |
|------|------|------|---------|
| 断裂 1 | 存了但找不到 | 混合检索 + 结构化过滤 | 复用已有方案 |
| 断裂 2 | 找到了但没注入 | Memory Router 自动拦截 | **市场空白** |
| 断裂 3 | 注入了但不遵循 | MUST/REF 指令化格式 | **市场空白** |

### 1.2 关键架构决策

- **独立工具**（非 Obsidian 插件），Obsidian 作为引流入口
- **存储层可插拔**，不重复造轮子，兼容 MemPalace/Mem0
- **MCP 为首要协议**，Rust 为核心引擎语言
- **Local-First** 架构，隐私优先
- **多 Agent 共享**：单 MCP Server 实例服务多个 Agent 客户端，共享 SQLite + LanceDB 存储

### 1.3 风险雷达

| 风险 | 等级 | 状态 |
|------|------|------|
| 冷启动难 | 🔴 高 | ✅ Embedding 自动回填 + sync 指令文件缓解 |
| 隐私合规 | 🔴 高 | ✅ Local-First 架构，数据不离开本地 |
| Router 误注入 | 🟡 中 | ✅ 软过滤 + 评分降权替代硬排除，MVP 验证通过 |
| Token 爆炸 | 🟡 中 | ✅ Token Budget 裁剪生效（默认 1500） |
| 多 Agent 记忆污染 | 🟡 中 | ✅ LWW 时间戳 + 多 Agent E2E 测试通过 |
| Agent 身份伪造 | 🟡 中 | ⬜ Phase 9 引入验证 |
| Pre-Prompt Injection 延迟 | 🟡 中 | ✅ SSE 传输 + Auto-Injection 已实现 |

---

## 二、可行性分析结论（2026-08-07 评审）

### 2.0 关键发现

经过对 [DESIGN.md](DESIGN.md) 全文的技术可行性评审，确认以下结论：

| 维度 | 评价 | 说明 |
|------|------|------|
| 市场定位 | **强** | Memory Router + 遵循保障确实是市场空白 |
| 技术可行性 | **中等偏上** | 核心功能可实现，但 Pre-Prompt Injection 需降级实现路径 |
| 架构设计 | **扎实** | 分层清晰，渐进式复杂度合理 |

### 2.0.1 MCP 协议限制（核心风险）

**标准 MCP 协议不支持消息拦截。** MCP Server 是被动的——Agent 主动调用 Tool 时 Server 才响应，Server 无法在"用户消息到达 Agent 之前"自动拦截并注入内容。

三种注入模式的实际可行性：

| 注入模式 | 可行性 | Phase 1 策略 |
|---------|--------|-------------|
| **MCP Resource** | ✅ 可行 | 作为主要注入方式，客户端启动时自动加载 |
| **Session Bootstrap** (`session_start` tool) | ⚠️ 部分可行 | 作为辅助方式，依赖客户端支持自动调用 |
| **Pre-Prompt Injection** (中间件拦截) | ❌ 需 MCP Proxy | 推迟到 Phase 2 实现 |

### 2.0.2 Compliance Tracker 调整

标准 MCP 中 Server 看不到 Agent 的回复，遵循度追踪需要额外架构支持。**推迟到 Phase 3 实现。**

### 2.0.3 Embedding 部署风险

本地 Embedding 模型 (bge-m3) 在 CPU 上推理延迟可能达到数百毫秒/条。Phase 1 先支持 API Embedding fallback，后续增加本地模型支持。

---

## 三、实施原则

### 3.1 核心策略：验证驱动的开发

先验证最不确定的假设，再投入完整工程：

```
高不确定性 ──────────────────── 低不确定性
   ↓               ↓               ↓
MCP Resource    指令化格式      存储层选型
注入是否有效？  提升遵循率？    (已知方案)
```

### 3.2 阶段划分逻辑（已调整）

```
Phase 0: 技术验证 (1周)     ✅ → 验证核心依赖可行性
Phase 1: Core Engine (5周)  ✅ → 可运行的最小闭环
Phase 2: 检索增强 (3周)     ✅ → BM25 + Vector + RRF 混合检索
Phase 3: Dashboard (3周)    ✅ → Tauri 2.0 桌面应用
Phase 4: 智能管道 (3周)     ✅ → Extractor/Dedup/Decay/Sync
Phase 5: 生态扩展 (4周)     ✅ → VS Code + Obsidian 插件
Phase 6: 召回率优化 (2周)   ✅ → RECALL_PLAN 7 项改进
Phase 7: 零入侵同步 (1周)   ✅ → memvault sync --watch
Phase 8: MCP Proxy (2周)    ✅ → SSE Server + Auto-Injection
Phase 9: 检索增强二期       ⬜ → Rerank / Inbox 审核面板 / 遵循度追踪
Phase 10: 生态扩展二期      ⬜ → Web App / CRDT / 图数据库
```

---

## 四、Phase 0 技术验证（第 0 周）🔥

### 总目标

> 在投入完整工程前，验证核心技术依赖的可行性，消除最大不确定性。

| 任务 | 产出 | 通过标准 |
|------|------|---------|
| Rust MCP SDK 选型 | PoC：MCP Server 接收 Tool 调用 | 能注册 Tool 并响应请求 |
| LanceDB Rust PoC | 向量 CRUD + 相似度搜索 | 100 条记忆的写入+查询延迟 < 50ms |
| Embedding 方案基准 | API vs 本地模型延迟对比 | 确定 Phase 1 使用 API 还是本地 |
| MCP Resource 注入验证 | Claude Desktop 加载自定义 Resource | 确认 Resource 内容出现在 Agent 上下文中 |
| MCP `session_start` 自动调用验证 | 配置 Claude Desktop 自动调用 | 确认可行性及客户端兼容性 |

**验收标准**：
- [ ] 确定 MCP SDK（推荐 `rmcp`）
- [ ] LanceDB Rust CRUD 跑通
- [ ] Embedding 方案确定（API fallback 优先）
- [ ] MCP Resource 注入路径验证通过
- [ ] 所有技术选型决策记录到本文档

---

## 五、Phase 1 详细计划（第 1-5 周）🔥

### 总目标

> 实现一个**可运行的 MCP Server**，能保存/搜索记忆，通过 MCP Resource 和 `session_start` Tool 将记忆注入到 Agent 上下文。
>
> **注意**：Phase 1 不实现 Pre-Prompt Injection（需 MCP Proxy 架构，推迟到 Phase 2），不实现 Compliance Tracker（需访问 Agent 回复，推迟到 Phase 3）。

### Week 1: Rust 项目骨架 + 存储层

| 任务 | 产出 | 关键决策点 |
|------|------|-----------|
| 初始化 Rust 项目结构 | Cargo workspace | 选 Axum 还是 Actix-web |
| SQLite schema 设计 | migrations + models | 确定记忆条目的核心字段 |
| LanceDB 集成 | vector store 初始化 | 选 LanceDB 还是 SQLite-vss |
| Markdown Frontmatter 解析 | frontmatter parser | 确定 YAML frontmatter 规范 |
| 基础配置文件 | config + env | CLI args vs 配置文件 |

**验收标准**：
- [ ] `cargo build` 通过
- [ ] SQLite 可读写记忆条目
- [ ] LanceDB 可存储/查询向量
- [ ] 可解析 Markdown Frontmatter

### Week 2: MCP Server 核心（含多 Agent 身份）

| 任务 | 产出 | 关键决策点 |
|------|------|-----------|
| MCP 协议实现 | MCP Server 骨架 | 选 mcp-rs 还是自实现 |
| `save_memory` tool | 可保存记忆（含 agent_id） | agent_id 自动注入还是要求请求方传入 |
| `search_memory` tool | 可搜索记忆 | 基础检索 vs 混合检索 |
| `session_start` tool | 可预加载记忆 + Agent 身份过滤 | Agent Registry 结构 |
| **Agent Registry** | 硬编码的 Agent 配置表 | YAML vs 内存配置 |
| **agent_id 传递** | 所有 MCP 请求携带 agent_id | MCP protocol extension 方式 |
| MCP Resource 注册 | memory://user-profile（多 Agent 共享） | 资源格式 |

**验收标准**：
- [ ] MCP Server 可启动并接受连接
- [ ] `save_memory` 写入后 `search_memory` 可召回
- [ ] `session_start` 返回 5-8 条相关记忆
- [ ] MCP Resource 可被客户端读取

### Week 3: Memory Router 核心（MCP Resource + session_start） 🔥

> **调整说明**：Pre-Prompt Injection 依赖 MCP Proxy 架构，标准 MCP 协议不支持消息拦截。Phase 1 使用 MCP Resource（启动时加载）+ `session_start` Tool（会话开始时调用）两种方式实现记忆注入。

| 任务 | 产出 | 关键决策点 |
|------|------|-----------|
| MCP Resource 实现 | `memory://user-profile` + `memory://project-context` | Resource 内容格式和更新频率 |
| `session_start` Tool 增强 | 按 Agent 身份返回过滤后的记忆 | 与 MCP Resource 的内容去重策略 |
| **Agent 身份识别** | Router 识别当前请求来自哪个 Agent | agent_id 从 Tool 参数提取 |
| **按 Agent 类型过滤** | 编程 Agent 不收到写作相关记忆 | exclude_types 配置 |
| 意图分析器（规则版） | 关键词匹配 | 先上规则引擎（快速迭代） |
| MUST/REF 格式引擎 | 指令化格式转换 | 模板 vs 动态生成 |
| Token Budget 控制 | 裁剪逻辑（每个 Agent 可配置） | 1500 tokens 默认值 |
| 优先级排序 | Rank + Trim | MUST 占多少配额 |

**验收标准**：
- [ ] MCP Resource 可被 Claude Desktop 自动加载
- [ ] `session_start` 返回按 Agent 类型过滤的记忆
- [ ] MUST 级记忆永远优先于 REFERENCE
- [ ] Token 总量不超过设定 Budget
- [ ] 记忆格式可被 Agent 识别（MUST/REF 指令化格式）
- [ ] 不同 Agent 收到不同的记忆子集

### Week 4: CLI 工具 + 多 Agent 集成测试

| 任务 | 产出 | 关键决策点 |
|------|------|-----------|
| CLI 工具 | memvault CLI | 子命令设计 |
| 端到端测试 | E2E 测试用例 | 模拟多个 MCP Client |
| **多 Agent 连接测试** | 2 个 MCP Client 同时连入 | 并发读写正确性 |
| **Agent Registry 配置化** | 从硬编码改为 YAML 配置 | 配置文件路径 |
| 错误处理完善 | 错误类型 + 日志 | 引入 tracing |
| 基础文档 | README + 使用指南 | API 文档格式 |

### Week 5: 集成验证 + Buffer

| 任务 | 产出 | 关键决策点 |
|------|------|-----------|
| Claude Desktop 真实集成测试 | 与 Claude Desktop 联调 | MCP Resource 加载验证 |
| 验证性 A/B 测试 | 遵循率基线 | 注入 vs 不注入的差异 |
| Bug 修复 + 稳定性 | 修复集成测试发现的问题 | — |
| 性能基准 | 搜索延迟、内存占用 | 是否需要优化 |
| 文档完善 | 完整 README + Quick Start | — |

**验收标准（Week 4-5 合并）**：
- [ ] CLI 可完成"保存 → 搜索 → 注入"闭环
- [ ] 2 个 MCP Client 可同时连接同一 MemVault 实例
- [ ] Claude Desktop 可通过 MCP Resource 加载用户记忆
- [ ] E2E 测试覆盖核心路径
- [ ] 有初步的遵循率基线数据
- [ ] README 包含安装和使用说明

### Phase 1-8 完成状态

- [x] Rust MCP Server 可运行（stdio + SSE 双传输模式）
- [x] 记忆可保存到 SQLite 并查询
- [x] **MCP Resource 可被客户端自动加载**
- [x] **`session_start` Tool 按 Agent 身份返回过滤记忆**
- [x] 指令化格式（MUST/REF）
- [x] **多 MCP Client 同时连接并共享记忆**（SSE 支持）
- [x] Agent Registry（YAML 配置，按类型过滤注入）
- [x] CLI 工具（12 个子命令：save/search/list/delete/session-start/resource/extract/dedup/decay/export/import/confirm-read/sync）
- [x] Token Budget 有效工作（默认 1500）
- [x] 混合检索（关键词 + 向量 + RRF 融合）
- [x] 智能管道（Extractor/Dedup/Decay/Sync）
- [x] Tauri 2.0/VS Code/Obsidian 多端覆盖
- [x] **RECALL_PLAN 7 项改进**：词级分词/多字段/同义词扩展/评分/软过滤/跨namespace/Embedding回填
- [x] **`memvault sync --watch`**：轮询自动重新生成指令文件
- [x] **MCP Proxy (SSE + Auto-Injection)**：`--transport sse` 网络传输
- [x] **单元测试 118 + E2E 12**，core 覆盖率 **89.17%**

---

## 四、Phase 2-5 概要

### Phase 2（第 6-8 周）：检索增强 + MCP Proxy 实验

| 核心任务 | 目的 |
|---------|------|
| BM25 + Vector 混合检索 | 提升召回率 |
| 查询改写 | 扩展检索覆盖面 |
| Rerank | 提升 top-k 精度 |
| **MCP Proxy 原型** | **验证 Pre-Prompt Injection 可行性** |
| Obsidian 插件 MVP | 用户引流入口 |
| Inbox 审核面板 | 人机协作 |

### Phase 3（第 9-11 周）：Dashboard + Compliance Tracker

| 核心任务 | 目的 |
|---------|------|
| Tauri 2.0 桌面应用 | Native 主界面 |
| 记忆审核队列 UI | 用户体验 |
| **遵循度追踪 Dashboard** | **Pro 功能基础（从 Phase 1 推迟至此）** |
| 记忆图谱可视化 | 差异化体验 |
| **MCP Proxy 集成** | **Pre-Prompt Injection 正式实现** |

### Phase 4（第 10-12 周）：智能管道

| 核心任务 | 目的 |
|---------|------|
| 后台 Extractor Worker | 自动记忆提取 |
| 去重/合并/冲突检测 | 记忆质量 |
| 遗忘曲线/衰减 | 记忆生命周期 |
| VS Code Extension | 开发者生态 |
| MemPalace/Mem0 adapter | 可插拔后端 |

### Phase 5（第 13-16 周）：生态扩展

| 核心任务 | 目的 |
|---------|------|
| Web App | 移动端覆盖 |
| CRDTs 多端同步 | 数据一致性 |
| 图数据库集成 | 关系推理 |
| 团队共享记忆池 | Team 产品 |
| 插件市场发布 | 社区生态 |

---

## 五、待验证假设的实验设计

### 实验 1：Pre-Prompt Injection 是否提升遵循率

```
控制组：Agent 无记忆注入
实验组：Agent 通过 Router 注入记忆
测量指标：Agent 回复是否包含用户偏好的违反
样本量：每组 50 次交互
```

### 实验 2：指令化 vs 描述性格式的遵循率

```
控制组：描述性格式（"用户喜欢简洁代码"）
实验组：指令性格式（"[MUST] 代码不加注释"）
测量指标：遵循率
```

### 实验 3：Token Budget 阈值测试

```
测试 token 预算：500 / 1000 / 1500 / 2000
测量：遵循率 vs 上下文占用 trade-off
```

---

## 六、技术准备

### 依赖调研（Week 0 完成）

- [ ] Rust MCP SDK（`rmcp` crate 验证）
- [ ] LanceDB Rust SDK（PoC 跑通 CRUD + 向量搜索）
- [ ] SQLite Rust SDK (`rusqlite`)
- [ ] Embedding 方案（API vs 本地，延迟基准测试）
- [ ] MCP Resource 注入验证（Claude Desktop 实测）
- [ ] Frontmatter 解析库 (`gray-matter` 或类似)
- [ ] Markdown 生成库
- [ ] CLI 框架 (`clap`)

### 环境准备

- [ ] Rust 工具链 (`rustup`, `cargo`)
- [ ] 开发数据库初始化脚本
- [ ] E2E 测试环境
- [ ] CI 配置

---

## 七、关键决策清单

以下决策需要在 Phase 1 开始前或过程中做出：

| 决策 | 选项 | 建议 | 截止时间 |
|------|------|------|---------|
| Web 框架 | Axum / Actix-web | Axum（社区活跃） | Week 1 |
| 向量库 | LanceDB / SQLite-vss | LanceDB（Rust 原生，Week 0 验证） | Week 0 |
| MCP 实现方式 | rmcp crate / 自实现 | rmcp（Week 0 验证） | Week 0 |
| 意图分析 | 规则引擎 / 本地 LLM | 规则引擎先上（快） | Week 3 |
| **Phase 1 注入策略** | MCP Resource + session_start | 两种并用（Pre-Prompt Injection 推迟到 Phase 2） | Week 0 |
| Embedding 方案 | API (OpenAI/本地 Ollama) / 本地模型 (bge-m3) | Phase 1 用 API fallback，Phase 2 增加本地模型 | Week 0 |
| 默认 Token Budget | 1000 / 1500 / 2000 | 1500（文档建议） | Week 3 |
| CLI 命名 | `memvault` / `mv` | `memvault`（明确） | Week 4 |
| Agent 身份传递 | MCP request header / tool param | tool param（更兼容） | Week 2 |
| Agent Registry 存储 | 硬编码 / YAML / SQLite | Phase 1 硬编码 → Week 4 改 YAML | Week 2 |
| 注入过滤策略 | 全部可见 / 按类型过滤 / ACL | 按类型过滤（编程代理不收到写作记忆） | Week 3 |
| 多 Agent 并发写入 | LWW / 版本向量 / CRDT | LWW（Last-Write-Wins，时间戳） | Week 4 |

---

## 八、成功定义

### Phase 1 结束时的成功标准

> **用户可以通过 `memvault` CLI 完成以下操作：**
>
> 1. `memvault save --content "用户偏好 Python" --priority MUST`
> 2. `memvault search --query "coding preference"` → 返回相关记忆
> 3. 启动 MCP Server，通过任意 MCP Client（如 Claude Desktop）连接
> 4. Agent 在收到用户消息时，自动携带相关记忆
> 5. 记忆以 [MUST]/[REF] 指令化格式注入
> 6. Token 总量控制在 1500 tokens 以内

### 质量门禁

- [ ] 所有核心路径有错误处理
- [ ] 关键函数有单元测试
- [ ] MCP 协议兼容性通过测试
- [ ] README 包含 Quick Start
- [ ] 代码符合 Rust 最佳实践

---

## 九、下一步行动

### v0.2.0 候选（Phase 9：检索增强二期）

| 优先级 | 任务 | 说明 |
|--------|------|------|
| 🔴 高 | **Rerank** | 对 top-k 结果二次排序，提升精度 |
| 🔴 高 | **Inbox 审核面板** | REST API 增加 pending 审核端点 |
| 🟡 中 | **遵循度追踪 Compliance Tracker** | 统计 Agent 遵循率 |
| 🟡 中 | **CLI/MCP 集成测试** | binary 覆盖率 0% → 提升 |
| 🟢 低 | **安全审计** | `cargo audit` + secret scan |
| 🟢 低 | **性能基准测试** | search/session_start 延迟基准 |

### 远期

- Web App / CRDT 多端同步
- 图数据库集成 / 团队共享记忆池
- 插件市场发布（VS Code + Obsidian）
- Pre-Prompt Injection MCP Proxy 生产化