# TencentDB-Agent-Memory 对 MemVault 的启发与改进计划

## Context

TencentDB-Agent-Memory 是腾讯云 2026 年 8 月开源的团队级 Agent 记忆中枢（v2.0，15k+ GitHub Stars）。
MemVault 是个人级 Agent 记忆路由器（v0.1.0），核心差异化在自动注入 + 遵循保障。

两者定位不同（团队 vs 个人），但 TencentDB 在记忆结构化、资产类型、分层注入策略方面有值得参考的设计。

---

## 一、两项目核心对比

| 维度 | MemVault | TencentDB-Agent-Memory | 判断 |
|------|----------|------------------------|------|
| **定位** | 个人记忆路由器 | 团队记忆中枢 | 不同赛道 |
| **记忆分层** | type-based (preference/fact/episode/skill) | L0→L1→L2→L3 渐进式管线 | **TDB 更系统** |
| **资产类型** | 单一记忆条目 | 4 类 (Chat/Skill/Wiki/CodeGraph) | **TDB 更丰富** |
| **Skill 结构** | type=skill，无结构化字段 | 版本/触发边界/步骤/验证规则/审核 | **TDB 更成熟** |
| **遵循保障** | MUST/REF + Compliance Tracker | 无明确机制 | **MemVault 领先** |
| **检索质量** | BM25+Vector+RRF+同义词+软过滤+7项优化 | BM25+FTS5，可选向量 | **MemVault 更强** |
| **注入方式** | MCP Resource + session_start + Proxy | Proxy injection + 工具检索 | 基本对等 |
| **权限/ACL** | namespace + agent_access (简单) | Team/Agent/Task/ACL 完整 | TDB 更完善 |
| **团队协作** | Phase 5 远期计划 | 已实现 | TDB 领先 |
| **代码理解** | 无 | CodeGraph (符号/调用链/影响范围) | **TDB 独有** |
| **文档知识** | 无 | LLM-Wiki (结构化文档知识库) | **TDB 独有** |
| **遗忘/衰减** | ✅ 已实现 | 无 | MemVault 独有 |
| **部署体验** | 单 Rust 二进制 | Docker 多服务 (Core+Hub+Proxy) | MemVault 更轻量 |
| **隐私** | Local-First，数据不离本地 | 团队级共享，需权限治理 | 各有定位 |

---

## 二、TencentDB 值得 MemVault 借鉴的 5 个方向

### 方向 1：L0-L3 分层沉淀机制（最有价值）

**TencentDB 做法**：
- L0 原始对话：全量保存，可追溯
- L1 原子事实：从对话中精准提取偏好、约束、关键事件
- L2 场景记忆：按项目/场景组织归纳
- L3 核心画像：长期稳定的用户/项目 Persona

**注入策略**：L3 直注 prompt、L2 作为索引按需加载、L1/L0 通过工具检索

**对 MemVault 的启发**：
当前 MemVault 的记忆是"平铺"的（所有记忆同一层级，仅靠 priority 区分注入优先级）。可以引入分层概念：
- 将现有 `type: preference` + `priority: MUST` 视为 L3
- 将 `type: fact/episode` 视为 L1/L2
- 增加**自动提炼管线**：从 L0/L1 自动归纳出 L3 画像
- 注入时 L3 永远注入、L2 按相关性注入、L1 通过 search 工具按需访问

**收益**：更精细的 Token Budget 管理，减少噪音注入。

---

### 方向 2：Skill 结构化升级

**TencentDB 做法**：
Skill 不是一段 prompt，而是完整的可复用 SOP：
```yaml
skill:
  name: "排障流程-数据库连接超时"
  version: 2
  trigger: "数据库连接超时/timeout/connection refused"
  steps:
    - "检查网络连通性"
    - "查看连接池状态"
    - "检查数据库实例状态"
  verification: "连接成功且延迟 < 100ms"
  owner: user_001
  shared: true
```

**对 MemVault 的启发**：
当前 `type: skill` 只是一个标签，content 是自由文本。可以增加结构化 Skill 模型：
- 增加 `steps` / `trigger` / `verification` 可选字段
- Skill 可以从成功的 Agent 会话中自动提取
- Skill 有版本管理，支持迭代改进

---

### 方向 3：分层注入策略优化

**TencentDB 做法**：
不是每次把所有记忆塞进 prompt，而是：
- L3 直接注入（核心画像，Token 开销小）
- L2 只注入索引摘要（"有 5 条关于 FastAPI 项目的记忆"）
- L1/L0 通过工具按需检索

**对 MemVault 的启发**：
当前 MemVault 的 `session_start` 返回 top-k 记忆全文。可以优化为：
- MUST 记忆：全文注入（已实现）
- REFERENCE 记忆：注入摘要 + 提供 `search_memory` 深度检索
- 超过 Token Budget 的部分：只注入索引（"还有 N 条相关记忆可通过 search 获取"）

**收益**：在有限 Token 内传递更多信息密度。

---

### 方向 4：Extraction 闭环增强

**TencentDB 做法**：
Proxy 架构实现完整闭环：
```
用户消息 → Proxy injection(注入记忆) → 转发给 Agent → Agent 回复 → Proxy extraction(提取写回)
```
Extraction 是白名单制：只有明确配置的类型才写回。

**对 MemVault 的启发**：
当前 MCP Proxy 已实现 injection，但 extraction 侧可以增强：
- Proxy 拦截 Agent 回复后，自动判断是否包含值得保存的新信息
- 白名单策略：只提取用户明确偏好变更、新的项目约束等
- 与现有 `extract` 命令整合，实现"对话中自动沉淀"

---

### 方向 5：Team 级记忆的前瞻规划

**TencentDB 做法**：
- Team → Agent → Task 三级组织
- 资产有 Owner / 版本 / 共享范围
- 按角色装配不同资产组合
- Memory Hub 统一管理面板

**对 MemVault 的启发**：
当前 Phase 5 规划了"团队共享记忆池"，可以参考 TencentDB 的模型提前设计 Schema：
- `team_id` 字段预留
- 资产 ownership 模型（谁创建、谁可见、谁可编辑）
- Task 级上下文隔离（同一 Team 不同 Task 的记忆独立性）

---

## 三、MemVault 无需跟进的方向（已领先或不适用）

| 方向 | 原因 |
|------|------|
| 遵循保障 (MUST/REF) | MemVault 已有，TDB 无此机制 |
| 混合检索 7 项优化 | MemVault 已完成，TDB 检索能力相对弱 |
| 遗忘/衰减机制 | MemVault 独有优势 |
| 单二进制轻量部署 | 个人工具的核心体验，不应 Docker 化 |
| Obsidian 双向同步 | 生态差异化入口 |
| CodeGraph | 偏离个人记忆路由器定位，工程投入巨大 |
| LLM-Wiki | 可通过 Obsidian 同步 + namespace 组织实现类似效果 |

---

## 四、建议的改进实施计划

### 优先级排序

| 优先级 | 改进项 | 工作量 | 状态 |
|--------|--------|--------|------|
| P0 | 分层注入策略（摘要+索引模式） | 小（1-2天） | ✅ 已完成 |
| P1 | L0-L3 分层标记 + 提炼管线 | 中（1周） | ✅ 已完成 |
| P2 | Skill 结构化字段扩展 | 小（2-3天） | ✅ 已完成 |
| P3 | Extraction 闭环（Proxy 回复提取） | 中（1周） | ✅ 已完成 |
| P4 | Team Schema 预留 | 小（1天） | ⬜ 暂不推进 |

### P0：分层注入策略（建议纳入 Phase 9）

修改 `session_start` 和 Proxy injection 逻辑：
- MUST 记忆：全文注入（不变）
- REFERENCE 记忆超过 Token Budget 时：注入 1 行摘要 + 提示"可通过 search_memory 获取详情"
- 在注入末尾追加："还有 N 条相关记忆未展示，使用 search_memory 查询"

涉及文件：
- `crates/memvault-core/src/retrieval/` — 增加 summary 模式
- `crates/memvault-mcp/` — session_start 返回格式调整
- `crates/memvault-proxy/` — injection 格式调整

### P1：L0-L3 分层记忆

在现有 `priority` 之外，增加 `layer` 概念：
- `layer: L3` = 长期稳定画像（对应当前 MUST preference）
- `layer: L2` = 场景级归纳（对应当前 REFERENCE fact）
- `layer: L1` = 原子事实（对应当前 extract 产出）
- `layer: L0` = 原始对话（新增：可选保存完整对话片段）

增加自动提炼管线：定期从 L1 归纳出 L2、从 L2 沉淀出 L3。

涉及文件：
- `crates/memvault-core/src/storage/` — Schema 增加 layer 字段
- `crates/memvault-core/src/pipeline/` — 增加 `promote` 模块
- `crates/memvault-cli/` — extract 输出标记 layer

### P2：Skill 结构化

扩展 `type: skill` 的记忆条目，支持可选结构化字段：
```json
{
  "type": "skill",
  "content": "...",
  "skill_meta": {
    "trigger": "pattern or description",
    "steps": ["step1", "step2"],
    "verification": "success criteria",
    "version": 1
  }
}
```

涉及文件：
- `crates/memvault-core/src/storage/models.rs` — 增加 skill_meta
- `crates/memvault-mcp/` — save_memory 支持 skill_meta 参数

### P3：Extraction 闭环

在 MCP Proxy 的回复路径增加提取逻辑：
- Agent 回复经过 Proxy 时，异步分析是否包含新的用户偏好/约束
- 白名单策略：只提取 preference/constraint 类型变更
- 提取后进入 Inbox 待审核（不自动生效）

涉及文件：
- `crates/memvault-proxy/` — 增加 response extraction 中间件

---

## 五、验证方式

1. **P0 分层注入**：对比修改前后 session_start 输出的信息密度（同 Token Budget 下覆盖更多记忆）
2. **P1 分层记忆**：运行 extract → 验证 layer 标记 → 运行 promote → 验证 L3 产出
3. **P2 Skill**：`memvault-cli save --type skill --steps "..." --trigger "..."` → search 返回结构化结果
4. **P3 Extraction**：通过 Proxy 发送含偏好变更的对话 → 验证 Inbox 出现新条目

---

## 六、结论

TencentDB-Agent-Memory 最大的启发是**分层沉淀 + 分层注入**的思路——不是把所有记忆平铺在同一层级，而是通过 L0→L3 的渐进提炼控制注入的信息密度。这与 MemVault 已有的 MUST/REF 优先级机制可以互补：优先级决定"必须看到什么"，分层决定"以什么精度看到"。

MemVault 的核心差异化（自动注入 + 遵循保障 + 遗忘衰减 + 混合检索）依然是市场空白，不需要因为 TencentDB 的出现改变方向。建议将分层注入策略作为 P0 快速落地，其余改进按节奏推进。
