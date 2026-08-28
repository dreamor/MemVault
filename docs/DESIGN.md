# 跨平台跨 Agent 记忆工具 — 产品与架构设计文档

> **项目代号**：MemVault（暂定）
> **版本**：v0.3
> **日期**：2026-08-07
> **定位**：AI Agent 时代的个人记忆路由器（Memory Router）
> **一句话**：不是让 Agent 学会查记忆，而是让记忆自动出现在 Agent 面前。
> **核心承诺**：任何 MCP 兼容的 Agent 接入后，自动共享同一套用户记忆。

---

## 目录

1. [项目愿景与核心问题](#1-项目愿景与核心问题)
2. [市场分析与竞品对标](#2-市场分析与竞品对标)
3. [核心痛点：记忆的三层断裂](#3-核心痛点记忆的三层断裂)
4. [路径选择：独立工具 + Thin Clients](#4-路径选择独立工具--thin-clients)
5. [最终架构设计（含 Memory Router）](#5-最终架构设计含-memory-router)
6. [Memory Router 核心模块设计](#6-memory-router-核心模块设计)
7. [技术栈选型](#7-技术栈选型)
8. [数据模型与 Schema 设计](#8-数据模型与-schema-设计)
9. [核心功能模块](#9-核心功能模块)
10. [MVP 路线图（更新版）](#10-mvp-路线图更新版)
11. [差异化竞争策略](#11-差异化竞争策略)
12. [风险与应对](#12-风险与应对)
13. [商业模式](#13-商业模式)
14. [多 Agent 共享记忆设计（v0.3 新增）](#14-多-agent-共享记忆设计v03-新增)
15. [三类记忆演进落地状态](#15-三类记忆演进落地状态)
16. [远期规划（尚未实现）](#16-远期规划尚未实现)

---

## 1. 项目愿景与核心问题

### 1.1 核心痛点

当前 AI Agent 生态中，记忆存在**三层断裂**：

用户期望：我告诉你了 → 你记住了 → 下次自动想起来 → 并且照做

实际发生：

- ❌ 断裂 1：存了但找不到（召回率低）
- ❌ 断裂 2：找到了但没注入（Agent 不知道去查）
- ❌ 断裂 3：注入了但不遵循（Agent 看到了但忽略）

### 1.2 愿景

> **为 AI Agent 构建"记忆路由器"——在 Agent 收到请求之前，自动把相关记忆以指令化格式注入到上下文中。**

让用户拥有：

- **数据主权**：记忆属于用户，不属于任何单一 Agent
- **自动召回**：Agent 不需要"想起来去查"，记忆自动出现
- **遵循保障**：记忆以指令化格式注入，Agent 必须遵循
- **人机协作**：用户可以审核、编辑、删除 AI 对自己的"理解"
- **隐私优先**：Local-First 架构，敏感数据不离开本地

### 1.3 设计原则

| 原则 | 说明 |
|------|------|
| 以用户为中心 | 数据模型是 `User → Memory`，不是 `Agent → Memory` |
| 自动注入优先 | 不依赖 Agent 主动调用，Router 自动分发 |
| 指令化格式 | 记忆不是"描述"，是"指令"，Agent 必须遵循 |
| 记忆与展示解耦 | 核心引擎独立于任何前端 |
| 开放标准 | 基于 MCP 协议，Markdown 兼容，随时可迁出 |
| 遗忘机制 | 有主动遗忘、归档、衰减策略 |

---

## 2. 市场分析与竞品对标

### 2.1 第一梯队：记忆存储与检索

| 项目 | Stars | 核心能力 | 召回率方案 | ❌ 缺失 |
|------|-------|---------|-----------|--------|
| **MemPalace** | 21.6k | 记忆宫殿层级 + ChromaDB | 结构化分层检索，LongMemEval 96.6% R@5 | 无自动注入；无人机审核；纯被动 Tool |
| **Mem0** | 42.5k | 向量+图谱+KV 混合存储 | 混合检索 + Mem0⁸图增强 | 仅 add/search API，Agent 必须主动调用 |
| **Zep** | ~3k | 时序知识图谱 | Graph Traversal + 时间推理 | 被动工具模式；企业级闭源 |
| **TiMEM** | 新项目 | 时序分层记忆树 | 跑分最高 | 学术原型，无产品化 |
| **LangMem** | 生态组件 | LangGraph 集成记忆 | create_memory_store | 框架绑定，非独立产品 |
| **Letta** (MemGPT) | ~15k | OS 式记忆管理 | 分页式上下文 | 偏 Agent 运行时 |
| **Memoria** | GTC 2026 | 可信记忆框架 | 安全审计 | 刚开源，功能不完整 |

### 2.2 第二梯队：Obsidian 生态

| 项目 | 核心能力 | ❌ 缺失 |
|------|---------|--------|
| **Khoj** | 本地 RAG + Obsidian 插件 | 仅问答，无记忆写入/审核/自动注入 |
| **Smart Connections** | 语义搜索 + 双链推荐 | 仅检索，无写入管道 |
| **mcp-obsidian** | MCP 文件读写代理 | 文件级操作，无语义抽象 |
| **Claudian** | Claude + Skills 系统 | 绑定单一模型 |

### 2.3 关键发现：市场空白

> **所有现有项目的共同架构盲区：**
>
> ```
> 现有项目：User Message → Agent → [Agent自己决定要不要调 search_memory] → LLM
>
> 我们的方案：User Message → [Memory Router 自动拦截] → 检索+注入 → Agent(已携带记忆) → LLM
> ```
>
> **没有任何一个项目在做 "Memory Router / Auto-Inject / 遵循保障" 这个完整链路。**

### 2.4 与最接近竞品的对比

| 维度 | MemPalace | Mem0 | Khoj | **MemVault** |
|------|-----------|------|------|-------------|
| 记忆写入 | ✅ | ✅ | ❌ | ✅ |
| 结构化检索 | ✅ 96.6% | ✅ | ⚠️ | ✅（可复用后端） |
| **自动注入（Router）** | ❌ | ❌ | ❌ | ✅ **核心差异** |
| **遵循保障（MUST/REF）** | ❌ | ❌ | ❌ | ✅ **核心差异** |
| 人机审核流 | ❌ | ⚠️ 有限 | ❌ | ✅ Inbox + Dashboard |
| 跨 Agent (MCP) | ⚠️ | ✅ | ❌ | ✅ |
| Obsidian 原生体验 | ❌ | ❌ | ✅ | ✅ |
| 遵循度追踪 | ❌ | ❌ | ❌ | ✅ |
| 遗忘/衰减 | ❌ | ❌ | ❌ | ✅ |

---

## 3. 核心痛点：记忆的三层断裂

### 3.1 断裂分析

```
┌─────────────────────────────────────────────────────────────┐
│ 断裂 1: 存了但找不到（召回率低）                             │
│ 原因：平铺存储、单一检索方式、无结构化过滤                    │
│ 解法：混合检索 + 结构化过滤 + 查询改写                       │
├─────────────────────────────────────────────────────────────┤
│ 断裂 2: 找到了但没注入（Agent 不主动查）                      │
│ 原因：记忆工具是被动 Tool，Agent 必须"想起来"去调用            │
│ 解法：Memory Router 自动拦截 + Pre-prompt Injection          │
├─────────────────────────────────────────────────────────────┤
│ 断裂 3: 注入了但不遵循（Agent 忽略记忆）                      │
│ 原因：记忆格式是描述性的，非指令性；数量过多导致信息过载        │
│ 解法：MUST/REF 分级 + 指令化格式 + Token Budget 控制          │
└─────────────────────────────────────────────────────────────┘
```

### 3.2 遵循率与记忆数量的关系

| 注入记忆条数 | 大致遵循率 |
|-------------|-----------|
| 1-3 条 | ~95% |
| 4-7 条 | ~80% |
| 8-15 条 | ~60% |
| >15 条 | ~30%（信息过载） |

**结论**：每次最多注入 5-8 条记忆，MUST 级永远优先。

---

## 4. 路径选择：独立工具 + Thin Clients

### 4.1 决策结论

> **做独立工具，采用 "Engine + Memory Router + Thin Clients" 架构。**
>
> 存储层可插拔（兼容 MemPalace/Mem0），核心价值在 Router + 遵循保障层。
> Obsidian 插件作为引流入口，独立工具承载商业价值。

### 4.2 关键策略

> **不要从零做存储引擎。把 MemPalace/Mem0 当作可插拔后端，核心价值是上面的 Memory Router + 遵循保障层。**

---

## 5. 最终架构设计（含 Memory Router）

### 5.1 整体架构图

```
┌─────────────────────────────────────────────────────────────────────┐
│ MemVault 系统架构                                                    │
│                                                                     │
│ ┌────────────────────────────────────────────────────────────────┐ │
│ │ Layer 5: 客户端层 (Thin Clients)                               │ │
│ │ Obsidian Plugin │ VS Code Ext │ Dashboard │ Web │ CLI          │ │
│ └────────────────────────────┬───────────────────────────────────┘ │
│                              │                                      │
│ ┌────────────────────────────▼───────────────────────────────────┐ │
│ │ Layer 4: 协议层 (Protocol)                                     │ │
│ │ MCP Server │ REST API │ WebSocket │ MCP Resource               │ │
│ └────────────────────────────┬───────────────────────────────────┘ │
│                              │                                      │
│ ┌────────────────────────────▼───────────────────────────────────┐ │
│ │ Layer 3: Memory Router（核心差异化）🔥                          │ │
│ │                                                                │ │
│ │ ┌──────────────┐ ┌──────────────┐ ┌────────────────────────┐  │ │
│ │ │ Auto-Inject  │ │ Format       │ │ Compliance             │  │ │
│ │ │ Engine       │ │ Engine       │ │ Tracker                │  │ │
│ │ │              │ │              │ │                        │  │ │
│ │ │ • Pre-prompt │ │ • MUST/REF   │ │ • 遵循度统计           │  │ │
│ │ │   Injection  │ │   分级       │ │ • 违规检测             │  │ │
│ │ │ • Session    │ │ • 指令化     │ │ • 优先级动态调整       │  │ │
│ │ │   Bootstrap  │ │   格式转换   │ │ • 反馈环               │  │ │
│ │ │ • Context    │ │ • Token      │ │                        │  │ │
│ │ │   Matching   │ │   Budget     │ │                        │  │ │
│ │ └──────────────┘ └──────────────┘ └────────────────────────┘  │ │
│ └────────────────────────────┬───────────────────────────────────┘ │
│                              │                                      │
│ ┌────────────────────────────▼───────────────────────────────────┐ │
│ │ Layer 2: 记忆引擎层 (Memory Engine)                             │ │
│ │                                                                │ │
│ │ ┌────────────┐ ┌────────────┐ ┌────────────┐ ┌────────────┐  │ │
│ │ │ Extractor  │ │ Retriever  │ │ Dedup &    │ │ Decay &    │  │ │
│ │ │            │ │            │ │ Merge      │ │ Forget     │  │ │
│ │ │ • 对话提取 │ │ • 混合检索 │ │ • 去重     │ │ • 衰减     │  │ │
│ │ │ • 意图分析 │ │ • 查询改写 │ │ • 合并     │ │ • 归档     │  │ │
│ │ │ • 分类     │ │ • Rerank   │ │ • 冲突检测 │ │ • 复习     │  │ │
│ │ └────────────┘ └────────────┘ └────────────┘ └────────────┘  │ │
│ └────────────────────────────┬───────────────────────────────────┘ │
│                              │                                      │
│ ┌────────────────────────────▼───────────────────────────────────┐ │
│ │ Layer 1: 存储层 (Pluggable Storage)                            │ │
│ │                                                                │ │
│ │ ┌──────────────────────────────────────────────────────────┐  │ │
│ │ │ 本地优先：SQLite(内嵌 int8 向量列) + Markdown Files               │  │ │
│ │ ├──────────────────────────────────────────────────────────┤  │ │
│ │ │ 可插拔后端：MemPalace │ Mem0 │ Zep │ 自定义              │  │ │
│ │ └──────────────────────────────────────────────────────────┘  │ │
│ └────────────────────────────────────────────────────────────────┘ │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
```

### 5.2 Memory Router 工作流

#### 5.2.1 单 Agent 模式（基础）

```
用户发送消息
│
▼
┌─────────────────────────────────────────────┐
│ Memory Router 拦截                           │
│                                             │
│ Step 1: 识别 Agent 身份                     │
│ agent_id: claude-desktop                     │
│ agent_type: coding-assistant                 │
│                                             │
│ Step 2: 意图分析                            │
│ "这条消息涉及哪些领域/项目/偏好？"            │
│                                             │
│ Step 3: 记忆检索                            │
│ 结构化过滤(agent + namespace) → 混合检索      │
│                                             │
│ Step 4: 优先级排序 + Token Budget            │
│ MUST 级优先 → REFERENCE 级补充               │
│ 总量控制在 5-8 条                            │
│                                             │
│ Step 5: 格式转换（指令化）                    │
│ 描述性记忆 → 命令式指令                      │
│                                             │
│ Step 6: 注入到 Agent 上下文                  │
│ System Prompt / Context / MCP Resource       │
└─────────────────────┬───────────────────────┘
                      │
                      ▼
Agent 收到的实际输入：
┌─────────────────────────────────────────────┐
│ System: 你是编程助手...                      │
│                                             │
│ [MEMORY CONTEXT - 必须遵循]:                 │
│ [MUST] 用户偏好 Python，不用 Java            │
│ [MUST] 代码不加注释，≤10行/函数              │
│ [REF] 当前项目：FastAPI + PostgreSQL         │
│ [REF] 上次讨论：订单服务分页方案              │
│                                             │
│ User: 帮我写一个...                          │
└─────────────────────────────────────────────┘
```

#### 5.2.2 多 Agent 共享模式（核心差异）

```
                            ┌─────────────────┐
                            │  MCP Server       │
                            │  (共享记忆引擎)    │
                            │ SQLite+内嵌向量   │
                            └────────┬─────────┘
                                     │
                    ┌────────────────┼────────────────┐
                    │                │                 │
          ┌─────────▼─────────┐  ┌──▼──────────┐  ┌───▼──────────┐
          │  Agent A           │  │  Agent B     │  │  Agent C     │
          │  (Claude Desktop)  │  │  (Cline)     │  │  (Cursor)    │
          │  编程助手           │  │  通用助手     │  │  IDE 内嵌    │
          └─────────┬─────────┘  └──┬──────────┘  └───┬──────────┘
                    │                │                 │
          ┌─────────▼────────────────▼─────────────────▼──────────┐
          │            Memory Router（共享实例）                    │
          │                                                       │
          │  ┌─────────────┐  ┌──────────┐  ┌──────────────────┐  │
          │  │ Agent Registry │  │ Filter   │  │ Inject Scheduler │  │
          │  │               │  │ Engine   │  │                  │  │
          │  │ A: coding     │  │ • agent  │  │ A: MUST preference│  │
          │  │ B: general    │  │ • ns     │  │ B: REF project   │  │
          │  │ C: code-ide   │  │ • intent │  │ C: MUST style    │  │
          │  └─────────────┘  └──────────┘  └──────────────────┘  │
          │                                                       │
          │  每个 Agent 收到不同的记忆子集，但读写同一份存储        │
          └───────────────────────────────────────────────────────┘
```

**核心机制**：

| 场景 | 行为 |
|------|------|
| Agent A 写了一条记忆 | 所有 Agent 都能读到（除非设置了可见性限制） |
| Agent B 查询记忆 | 看到包括 Agent A 写入的所有匹配记忆 |
| Agent A 写入"偏好 Python" | Agent B 的注入中不会出现这条（B 是通用助手，不相关） |
| 用户通过 Dashboard 编辑 | 所有 Agent 下次注入时使用最新版本 |

### 5.3 记忆分层模型

| 层级 | 类型 | 生命周期 | 存储方式 | 示例 |
|------|------|---------|---------|------|
| L1 | 工作记忆 (Working) | Session 级 | 内存 | 当前对话上下文 |
| L2 | 情景记忆 (Episodic) | 天/周级 | Daily Notes + Vector | "今天讨论了架构" |
| L3 | 语义记忆 (Semantic) | 长期 | Entity Notes + Graph | "用户偏好 Python" |
| L4 | 程序记忆 (Procedural) | 永久 | Skill Notes | "部署流程" |

> 落地状态（2026-08-27）：L2 情景 / L3 语义 / L4 程序三类认知记忆已实现（原《三类记忆演进计划》已并入本文档），架构与接口见 §15。

---

## 6. Memory Router 核心模块设计

### 6.1 Auto-Inject Engine（自动注入引擎）

#### 三种注入模式（按可行性排序）

> **重要说明（2026-08-07 可行性评审）**：标准 MCP 协议中，Server 是被动的——Agent 主动调用 Tool 时 Server 才响应。Server **无法**在用户消息到达 Agent 之前自动拦截并注入内容。因此三种注入模式的实现优先级需要调整。

| 模式 | 触发时机 | 适用场景 | 实现方式 | Phase |
|------|---------|---------|---------|-------|
| **MCP Resource** ⭐ | Agent 启动时 | 高频核心记忆（默认） | MCP Resource URI，客户端自动加载 | **Phase 1** |
| **Session Bootstrap** | 新会话开始时 | 长对话、项目切换 | `session_start` 工具，需客户端支持自动调用 | **Phase 1** |
| **Pre-Prompt Injection** | 每次用户发消息前 | 动态上下文匹配 | 需实现 MCP Proxy 架构 | **Phase 2** |

> Phase 1 策略：MCP Resource 承载 MUST 级核心记忆（启动时加载），`session_start` Tool 提供按上下文过滤的 REFERENCE 级记忆。Pre-Prompt Injection 在 Phase 2 通过 MCP Proxy 实现。

#### Pre-Prompt Injection 伪代码（多 Agent 版）

```python
class MemoryRouter:
    def __init__(self):
        self.agent_registry = AgentRegistry()
        # 注册已知 Agent 及其上下文类型
        self.agent_registry.register(
            agent_id="claude-desktop",
            agent_type="coding-assistant",
            default_namespace="project:memvault",
            inject_rules={"max_memories": 8, "priority_order": ["MUST", "REF"]}
        )
        self.agent_registry.register(
            agent_id="cline-vscode",
            agent_type="general-assistant",
            default_namespace="global",
            inject_rules={"max_memories": 5, "priority_order": ["MUST"]}
        )

    def intercept(self, user_message: str, agent_context: dict) -> str:
        """在消息到达 Agent 之前自动注入记忆（支持多 Agent）"""

        agent_id = agent_context.get("agent_id", "unknown")
        agent_profile = self.agent_registry.get(agent_id)

        # 1. 意图分析（结合 Agent 身份）
        intent = self.analyze_intent(user_message, agent_type=agent_profile.type)

        # 2. 检索相关记忆
        #    - namespace 过滤：不同 Agent 可以关注不同范围
        #    - agent_access 过滤：记忆可能对特定 Agent 可见/不可见
        memories = self.retrieve(
            query=user_message,
            intent=intent,
            filters={
                "namespace": agent_context.get("project", agent_profile.default_namespace),
                "agent_access_mode": "all"   # Phase 1：全部可见
            },
            top_k=20
        )

        # 3. 优先级排序 + Token Budget 裁剪（每个 Agent 可配置）
        selected = self.rank_and_trim(
            memories,
            max_count=agent_profile.inject_rules["max_memories"],
            token_budget=agent_profile.inject_rules.get("token_budget", 1500),
            priority_order=agent_profile.inject_rules["priority_order"]
        )

        # 4. 格式转换（指令化）+ 注入会话追踪
        inject_session_id = self.generate_session_id()
        formatted = self.format_as_instructions(selected, inject_session_id)

        # 5. 注入到 System Prompt
        return self.inject_to_context(agent_context, formatted)
```

### 6.2 Format Engine（格式引擎）

#### 记忆分级

```yaml
# MUST 级：Agent 必须遵循，违反即为错误
- priority: MUST
  content: "代码不加注释，不用 type hints，函数≤10行"
  enforcement: "HARD_RULE"
  source: "用户明确要求 (2026-07-15)"

# REFERENCE 级：Agent 可参考，相关时使用
- priority: REFERENCE
  content: "用户上次提到对 Redis 感兴趣"
  enforcement: "SOFT_HINT"
  source: "对话推断 (2026-08-01)"
```

#### 格式转换规则

```
❌ 描述性格式（Agent 容易忽略）：
"用户在2026-07-15的对话中提到他喜欢简洁的代码风格"

✅ 指令性格式（Agent 必须遵循）：
"[MUST] 代码输出规范：
- 不加注释
- 不用 type hints
- 单函数 ≤ 10 行
- 来源：用户明确要求
- 优先级：HARD_RULE"
```

### 6.3 Compliance Tracker（遵循度追踪）

> **注意（2026-08-07 可行性评审）**：标准 MCP 中 Server 看不到 Agent 的回复——Server 只处理 Tool 调用请求，不会收到 LLM 的最终输出。以下设计需要额外的反馈机制支持，**推迟到 Phase 3 实现**。
>
> 可选实现路径：
> - **方案 A**：MCP Proxy 拦截完整对话流（Phase 2 MCP Proxy 就绪后可用）
> - **方案 B**：离线分析对话日志（可行，但非实时）
> - **方案 C**：Agent 主动调用 `report_compliance` Tool（依赖 Agent 配合，不可靠）

```
记忆注入 → Agent 生成回复 → 分析回复是否遵循
                                    │
                    ┌───────────────┼───────────────┐
                    ▼               ▼               ▼
               遵循 ✅          部分遵循 ⚠️      未遵循 ❌
               (记录)         (记录+提醒)     (记录+升级)
                                                │
                                                ▼
                                    提升优先级 / 改变注入位置
                                    / 通知用户审核
```

### 6.4 查询改写（Query Rewriting）

```python
def rewrite_query(original_query: str, intent: str) -> list[str]:
    """将 Agent 的短查询扩展为多路检索"""

    rewrites = []

    # 同义词扩展
    rewrites.append(expand_synonyms(original_query))

    # 基于意图的扩展
    if intent == "coding":
        rewrites.append(f"{original_query} coding style preference")

    # 基于历史的扩展
    recent_topics = get_recent_topics(limit=3)
    for topic in recent_topics:
        rewrites.append(f"{original_query} {topic}")

    return rewrites
```

---

## 7. 技术栈选型

### 7.1 核心引擎

| 组件 | 推荐方案 | 理由 |
|------|---------|------|
| 引擎语言 | Rust | 高性能，低资源占用，跨平台 |
| 结构化存储 | SQLite | 零依赖，本地优先 |
| 向量检索 | SQLite 内嵌列 | int8 量化,随行存储,零额外依赖(LanceDB 调研后弃用) |
| 图数据库 | FalkorDB（可选） | P2 阶段引入 |
| 同步协议 | CRDTs (Automerge) | 多端无冲突 |
| 记忆协议 | MCP | 事实标准 |
| 可插拔后端 | MemPalace / Mem0 adapter | 借力已有生态 |

### 7.2 Memory Router

| 组件 | 方案 | 用途 |
|------|------|------|
| 拦截层 | MCP Proxy / HTTP Middleware | 请求拦截 |
| 意图分析 | 本地小模型 / 规则引擎 | 判断消息涉及哪些记忆 |
| 查询改写 | LLM / 模板 | 扩展检索词 |
| 格式引擎 | 模板 + 规则 | MUST/REF 转换 |
| 遵循检测 | LLM-as-a-Judge | 分析回复是否遵循 |

### 7.3 客户端

| 客户端 | 技术栈 | 定位 |
|--------|--------|------|
| Web Dashboard | React + TypeScript | 主界面（浏览器，REST 后端） |
| Obsidian Plugin | TypeScript | 薄客户端 + 引流 |
| VS Code Extension | TypeScript | Coding 上下文 |
| CLI | Rust | 开发者 |

### 7.4 AI / NLP

| 组件 | 方案 | 用途 | Phase |
|------|------|------|-------|
| Embedding | native 内嵌（fastembed，默认）→ 可选 ollama / openai-compatible | 向量化 | v0.2.0 默认内嵌本地模型 |
| 记忆提取 | 规则引擎(默认)+ 可选任意 OpenAI 兼容 Chat LLM | 对话→结构化记忆 | 基础版已实现,`MEMVAULT_LLM_EXTRACTION_PROVIDER` 开启 |
| 遵循检测 | 轻量 LLM | 判断回复是否遵循 | Phase 3 |
| Rerank | bge-reranker | 检索结果重排 | Phase 2 |

> **Embedding 部署说明**（v0.2.0 更新）：默认采用 **native 内嵌推理**（fastembed + ONNX Runtime，进程内运行，零外部依赖），模型默认中文 `bge-small-zh-v1.5`（~95MB），可经 `MEMVAULT_EMBEDDING_MODEL=multilingual` 切换多语言 `multilingual-e5-base`。也支持配置切换本地 Ollama 服务或任意 OpenAI 兼容端点（`MEMVAULT_EMBEDDING_PROVIDER`）。首次使用自动从 HuggingFace 下载模型，国内网络可设 `HF_ENDPOINT=https://hf-mirror.com`。
>
> **记忆提取部署说明**：默认**本地优先**——不设置 `MEMVAULT_LLM_EXTRACTION_PROVIDER` 时会自动探测本机 Ollama（`http://localhost:11434`），探测到就零配置直接用它做上下文提取（默认模型 `qwen2.5:7b`，免费、不出本机）；没探测到则保持纯规则/关键词提取器（`Extractor`，零外部依赖）。理解完整对话上下文的 LLM 提取路径见 `crates/memvault-core/src/llm_extractor.rs`；`memvault-proxy` 的 `notify_response` 会把 user_text + response_text 一起喂给它，而不是分别逐行扫描。远程提供商（`openai`/`openai-compatible`）必须显式设置 provider 才会启用——不会因为别处配了 `OPENAI_API_KEY` 就自动调用付费 API，这是刻意选择：本地探测零风险可以零配置，远程调用有真实成本和幻觉风险，必须显式 opt-in。LLM 路径调用失败(网络/解析错误)会自动回退到规则提取，不影响主流程。

---

## 8. 数据模型与 Schema 设计

### 8.1 记忆条目 Schema（多 Agent 版）

```yaml
---
id: mem_20260807_001
type: preference          # preference | fact | episode | entity | skill
content: "用户喜欢简洁的代码风格，不喜欢过度注释"
instruction: "[MUST] 代码不加注释，不用 type hints，函数≤10行"
priority: MUST            # MUST | REFERENCE | BACKGROUND

# --- Agent 身份信息 ---
source_agent:
  id: claude-desktop       # 写入该记忆的 Agent
  type: coding-assistant   # Agent 类型（coding | general | research 等）
  session_id: "sess_abc123" # 写入时的会话 ID（用于追溯）

# --- 可见性控制 ---
namespace: global          # global | project:xxx
agent_access:
  mode: all                # all | allowlist | denylist
  allowlist: []            # ["agent:claude-desktop", "agent:cline-vscode"]
  # Phase 1 只实现 mode: all，allowlist/denylist 为 Phase 3

confidence: 0.9
tags: [coding, style, python]
created: 2026-08-07T15:00:00+08:00
updated: 2026-08-07T15:00:00+08:00
related: ["[[Python]]", "[[Code Style]]"]
ai_generated: true
human_reviewed: false
decay_score: 1.0
access_count: 0
compliance:
  rate: 0.95               # 跨所有 Agent 的综合遵循率
  by_agent:                 # 按 Agent 拆分的遵循率
    claude-desktop: 0.97
    cline-vscode: 0.90
  violation_count: 1        # 跨所有 Agent 的违规次数
  violations:               # 具体违规记录（关联 inject_session_id）
    - agent_id: cline-vscode
      inject_session_id: "inj_def456"
      timestamp: 2026-08-07T16:00:00+08:00
      violation_type: ignored_must
---
```

### 8.2 Vault 目录结构

```
vault/
├── 00-Inbox/              # AI 自动捕获，待审核
├── 10-Daily/              # 情景记忆 (Episodic)
│   └── 2026-08-07.md
├── 20-Entities/           # 语义记忆 (Semantic)
│   ├── People/
│   ├── Projects/
│   └── Concepts/
├── 30-Memories/           # 长期事实/偏好
│   ├── MUST-Rules.md      # 强制规则
│   ├── Preferences.md
│   └── Facts.md
├── 40-Skills/             # 程序记忆
├── 50-Archive/            # 衰减归档
├── _Router/               # Memory Router 配置
│   ├── inject-rules.yaml  # 注入规则
│   ├── format-templates/  # 格式模板
│   └── compliance-log.json # 遵循度日志
└── Templates/
```

> **已实现（2026-08-27）**：Obsidian 插件 `sync.ts::folderFor` 按类型把记忆落盘到上述目录——episode → `10-Daily`、entity → `20-Entities`、fact/preference → `30-Memories`、skill → `40-Skills`，同步时自动创建子目录。

> **Truth source 原则（2026-08-28 明确）**：**SQLite 是唯一 truth source**；Obsidian Vault 目录、未来的导出/文件客户端都只是**投影/缓存**。文件侧内容可被外部工具改写，但不得作为回写权威；因此文件侧写入需要「事务式写入协议」（见 §16）这类安全机制保护，其投入时机由多 Agent 共享写入 / 人机审核流触发决定。

### 8.3 MCP Tools 定义（多 Agent 版）

```json
{
  "tools": [
    {
      "name": "save_memory",
      "description": "保存一条记忆（自动记录来源 Agent）",
      "parameters": {
        "agent_id": "claude-desktop",
        "agent_type": "coding-assistant",
        "type": "preference | fact | episode | entity | skill",
        "content": "记忆内容",
        "priority": "MUST | REFERENCE | BACKGROUND",
        "tags": [],
        "namespace": "global | project:xxx",
        "confidence": 0.0
      }
    },
    {
      "name": "search_memory",
      "description": "搜索用户记忆（跨 Agent 共享）",
      "parameters": {
        "query": "搜索内容",
        "agent_id": "当前 Agent ID",
        "type_filter": "",
        "priority_filter": "",
        "time_range": "",
        "namespace": "",
        "top_k": 10,
        "token_budget": 2000
      }
    },
    {
      "name": "session_start",
      "description": "新会话开始时自动调用，预加载记忆（按 Agent 身份过滤）",
      "parameters": {
        "agent_id": "claude-desktop",
        "agent_type": "coding-assistant",
        "context_hint": "当前对话主题提示",
        "project": "当前项目名",
        "max_memories": 8
      }
    },
    {
      "name": "review_memory",
      "description": "审核待确认的记忆",
      "parameters": {
        "action": "approve | reject | edit",
        "memory_id": "",
        "edited_content": ""
      }
    },
    {
      "name": "get_compliance_report",
      "description": "获取记忆遵循度报告（支持跨 Agent 聚合）",
      "parameters": {
        "time_range": "7d | 30d | all",
        "agent_filter": "",
        "aggregate": true
      }
    }
  ],
  "resources": [
    {
      "uri": "memory://user-profile",
      "name": "User Profile & MUST Rules",
      "description": "用户核心偏好和强制规则，每次对话必须加载（所有 Agent 共享）"
    },
    {
      "uri": "memory://project-context",
      "name": "Current Project Context",
      "description": "当前项目的技术栈和约定（按 Agent 类型过滤）"
    }
  ]
}
```

> **当前实现（2026-08-27）**：MCP 工具已扩展至 **15 个**——在原有 13 个基础上新增 `record_outcome`（任务结果上报/教训）与 `import_skills`（Markdown SOP 导入）；`search_memory` / `POST /api/search` 新增 `expand_relations` 参数。完整清单见根 README「15 MCP Tools」与 §15。

---

## 9. 核心功能模块

### 9.1 记忆写入管道

```
Agent 对话 → Extractor → 去重 → 写入 Inbox → 通知审核
                                        ↓
                              用户确认 → 正式目录 + 生成 instruction
                              用户拒绝 → 标记删除
```

### 9.2 Memory Router 检索管道（多 Agent 版）

```
用户消息进入（携带 agent_id）
     ↓
Agent 身份识别 → 查询 Agent Registry 获取注入规则
     ↓
意图分析器 → 判断涉及哪些领域/项目/偏好（结合 Agent 类型）
     ↓
结构化过滤（type × namespace × priority × agent_access）
     ↓
混合检索：
  ├─ 精确查询 → SQLite WHERE
  ├─ 语义查询 → SQLite 内嵌向量(cosine)
  ├─ 关系查询 → Graph / Backlinks
  └─ 时序查询 → Daily Notes
     ↓
按 Agent 类型过滤不相关的记忆
（例如：写作风格的 MUST 不注入给编程 Agent）
     ↓
Rerank + Token Budget 裁剪（≤8条）
     ↓
格式转换（指令化 + 注入会话 ID）
     ↓
注入到 Agent System Prompt

### 9.3 人机协作审核流

1. AI 写入 → `00-Inbox/`，标记 `human_reviewed: false`
2. Dashboard / Obsidian 侧边栏显示待审核面板
3. 用户审核 → 批准时自动生成 `instruction` 字段
4. 批准后更新 Frontmatter + 建立双链
5. Router 优先返回 `human_reviewed: true` 的记忆

### 9.4 遵循度反馈环

```
注入记忆 → Agent 回复 → 遵循检测 → 记录
                                      ↓
                              违规次数 > 3
                                      ↓
                              自动提升优先级
                              或改变注入位置
                              或通知用户
```

### 9.5 遗忘与衰减

- `decay_score` 随时间递减
- 长期未访问 → 自动归档到 `50-Archive/`
- 被再次访问 → 恢复权重
- 用户可配置策略

---

## 10. MVP 路线图（更新版）

### Phase 0: 技术验证（第 0 周）🔥 **新增**

- Rust MCP SDK（`rmcp`）选型验证
- ~~LanceDB Rust PoC~~(结论:v0.2 弃用,向量改存 SQLite 内嵌 int8 列)
- Embedding 延迟基准测试（API vs 本地）
- MCP Resource 注入验证（Claude Desktop 实测）
- MCP `session_start` 自动调用验证

### Phase 1: Core Engine + Memory Router + 多 Agent 基础（第 1-5 周）🔥

> **调整说明**：Phase 1 使用 MCP Resource + `session_start` Tool 实现记忆注入，不实现 Pre-Prompt Injection（需 MCP Proxy，推迟到 Phase 2）。不实现 Compliance Tracker（需访问 Agent 回复，推迟到 Phase 3）。时间线从 4 周调整为 5 周（含 1 周 buffer）。

- Rust 本地服务骨架
- 存储初始化(SQLite;向量方案先后验证 LanceDB 与内嵌列,最终采用后者,以 int8 量化随行存储)
- MCP Server 实现（save / search / session_start）
- **MCP Resource 实现**（`memory://user-profile`, `memory://project-context`）
- **Agent Registry 注册表**（Phase 1 硬编码，后续改配置）
- **MCP 请求中携带 agent_id / agent_type**
- **Router 按 Agent 类型过滤记忆（基础版）**
- 记忆指令化格式引擎（MUST/REF）
- Token Budget + 优先级排序
- Markdown 文件读写 + Frontmatter 解析
- 基础向量检索（API Embedding）
- CLI 工具验证
- 多 Agent 连接 E2E 测试
- Claude Desktop 真实集成验证

#### Phase 1.5: 多 Agent 共享增强（第 4 周中追加）🔥

- Agent Registry 从硬编码改为配置驱动（YAML）
- `source_agent` 结构化存储（id / type / session_id）
- 注入会话追踪（`inject_session_id`）
- 基础遵循度日志（按 Agent 拆分）
- 两个 MCP Client 同时连接的压力测试
- 多 Agent 共享记忆的正确性验证

### Phase 2: 检索增强 + MCP Proxy + Obsidian 客户端（第 6-8 周）

- 混合检索（BM25 + Vector + 结构化过滤）
- 查询改写
- Rerank
- **MCP Proxy 原型**（验证 Pre-Prompt Injection 可行性）
- 本地 Embedding 模型支持（bge-m3）
- Obsidian 插件：连接 Core Engine
- Inbox 审核面板
- 双向同步
- MCP Resource 自动加载

### Phase 3: Web Dashboard + 遵循追踪（第 9-11 周）

- Web Dashboard（浏览器,由 `memvault-mcp --transport http --serve-web` 托管）
- 记忆审核队列 UI
- Agent 活动监控面板
- **遵循度追踪 Dashboard**（从 Phase 1 推迟至此，依赖 MCP Proxy）
- 记忆类型可视化图谱
- **MCP Proxy 正式集成**（Pre-Prompt Injection 完整实现）
- 导入/导出工具

### Phase 4: 智能管道 + 生态（第 9-12 周）

- 后台 Extractor Worker
- 去重 / 合并 / 冲突检测
- 遗忘曲线 / 衰减
- VS Code Extension
- 可插拔存储后端（MemPalace / Mem0 adapter）

### Phase 5: 扩展（第 12-16 周）

- Web App
- CRDTs 多端同步
- 图数据库集成（仍按计划推迟：关系规模未超单表一跳扩展收益点，见 §15）
- 团队共享记忆池 → ✅ 已落地（2026-08-27，见 §15）
- 插件市场发布

### Phase 6: 三类记忆演进闭环（2026-08 已完成 ✅）

> 由原《三类记忆演进计划》（已并入本文档）落地，验收假设 H5–H7 均在真实 `memvault-mcp` 服务器上实测 CONFIRMED（详见 `docs/experiments/REPORT.md`）。

- **情景记忆（Phase A）**：`episodes` 表 + `memories.superseded_by`（migration 6-9）；`record_outcome` 全链路（MCP/CLI/REST）+ `GET /api/episodes`；失败自动反思出教训（`reflection.rs`，SourceRole 守卫防自我强化漂移）；同类 `task_type` 教训自动注入（REFERENCE，MUST 需人工确认）；Dashboard「Episodic」页。
- **程序记忆（Phase B）**：技能 `trigger` × 意图匹配 → 结构化 `[SKILL]` 注入（成功率 ≥3 次样本才展示）；`skill_stats` 表（migration 10，注入/成功/失败计数）；失败命中 → 自动 `needs-revision` 标记，人工修订 `version+1`；同类型 ≥3 次成功自动沉淀技能草稿进审核队列。
- **语义记忆（Phase C）**：`memory_relations` 三元组表（migration 11-13）+ LLM 关系抽取（`MEMVAULT_RELATIONS=on` 显式开启）；promote 新增事实巩固/实体归一阶段（`consolidated_from`/`superseded_by` 溯源）；`supersede` 取代流程（检索默认排除旧事实、不物理删除、可回滚）；检索支持 `expand_relations` 一跳展开。
- **Phase D（可选部分已落地）**：团队共享记忆池（`visibility=shared`，跨命名空间注入上限 20 条）；SOP/Markdown 批量导入技能（`sop.rs` + CLI `import-skills` + MCP `import_skills`）；Obsidian 插件按类型分目录同步（见 §8.2）。

---

## 11. 差异化竞争策略

### 11.1 核心差异化（三层壁垒）

| 层级 | 现有项目状态 | 我们的方案 | 壁垒强度 |
|------|------------|-----------|---------|
| L1: 存储与检索 | MemPalace/Mem0 已做好 | 兼容接入，不重复造轮子 | 🟢 借力 |
| L2: 自动注入 (Router) | ⚠️ 几乎无人做 | Pre-prompt + MCP Resource + Session Bootstrap | 🔴 核心壁垒 |
| L3: 遵循保障 | ⚠️ 完全空白 | MUST/REF + 指令化 + 遵循度追踪 | 🔴 核心壁垒 |
| L4: 人机协作审核 | 部分有 | Inbox + Dashboard + 双向同步 | 🟡 差异化 |
| L5: Obsidian 原生 | Khoj/SC 做了 RAG | 记忆管理专属 UI | 🟡 差异化 |

### 11.2 定位话术

> "Mem0 和 MemPalace 解决了'怎么存和找'。
> 我们解决的是'怎么自动给到 Agent，并让它照做'。
> 不是让 Agent 学会查记忆，而是让记忆自动出现在 Agent 面前。"

### 11.3 与已有生态的关系

```
MemPalace / Mem0（存储后端）
        ↓ 可插拔接入
MemVault Memory Router（自动注入 + 遵循保障）
        ↓ 分发到
Obsidian / VS Code / Dashboard / 任意 MCP Client
```

---

## 12. 风险与应对

| 风险 | 严重程度 | 应对策略 |
|------|---------|---------|
| **MCP 协议不支持消息拦截** | 🔴 高 | Phase 1 用 MCP Resource + session_start；Phase 2 实现 MCP Proxy |
| 冷启动难 | 🔴 高 | Obsidian 插件引流；兼容 MemPalace/Mem0 降低迁移成本 |
| Router 误注入（注入不相关记忆） | 🟡 中 | 保守策略：宁少勿多；用户可关闭自动注入 |
| Agent 不遵循 MCP 协议 | 🟡 中 | 多模式兜底：Tool + Resource + System Prompt |
| 编辑体验不如 Obsidian | 🟡 中 | 不复刻编辑器，专注记忆管理 |
| 同步复杂 | 🟡 中 | Local-first，文件导出兜底 |
| Token 爆炸 | 🟡 中 | Token Budget 硬限制；默认返回摘要 |
| **本地 Embedding 延迟** | 🟡 中 | Phase 1 用 API fallback；Phase 2 增加本地模型 |
| **Compliance Tracker 无法获取 Agent 回复** | 🟡 中 | 推迟到 Phase 3，依赖 MCP Proxy 架构 |
| 隐私合规 | 🔴 高 | Local-First；加密存储；GDPR 红线 |
| 大厂抄袭 | 🟢 低 | 开源核心；社区壁垒；垂直体验 |

---

## 13. 商业模式

### 13.1 产品分层

| 层级 | 价格 | 功能 |
|------|------|------|
| Free / OSS | 免费 | Core Engine + Router + CLI + Obsidian 插件 |
| Pro | $8/月 | Dashboard + 遵循度追踪 + 云同步 + 高级可视化 |
| Team | $20/人/月 | 共享记忆池 + 权限 + 审计 |
| Enterprise | 定制 | 私有部署 + SSO + SLA |

### 13.2 增长飞轮

```
开源 Core + Router → 开发者信任
       ↓
Obsidian 插件引流 → 免费用户
       ↓
"Agent 不遵循记忆"痛点 → 升级 Pro（遵循度追踪）
       ↓
收入 → 研发 → 更好的 Router → 更多用户

---

## 14. 多 Agent 共享记忆设计（v0.3 新增）

### 14.1 设计目标

```
多个 Agent → 同一个 MemVault 实例 → 共享记忆但按需过滤
                               ↓
                 Agent A 看到编程相关的 MUST
                 Agent B 看到写作风格相关的 MUST
                 Agent C 看到所有全局 REFERENCE
                 ──────────────────────────────
                 底层是同一份 SQLite(含内嵌向量)数据
```

### 14.2 Agent Registry（Agent 注册表）

每个连接的 Agent 需要在 MemVault 中注册身份，Router 据此提供差异化的注入服务：

```yaml
# _Router/agent-registry.yaml
agents:
  - id: claude-desktop
    type: coding-assistant
    description: "日常编程助手"
    inject_rules:
      max_memories: 8
      token_budget: 1500
      priority_order: ["MUST", "REFERENCE"]
      namespace_filter: ["global", "project:*"]
      exclude_types: ["writing", "design"]  # 不注入写作/设计相关的记忆

  - id: cline-vscode
    type: general-assistant
    description: "VS Code 内通用助手"
    inject_rules:
      max_memories: 5
      token_budget: 1000
      priority_order: ["MUST"]
      namespace_filter: ["global"]
      exclude_types: []

  - id: cursor-ide
    type: code-ide
    description: "Cursor IDE 内嵌助手"
    inject_rules:
      max_memories: 6
      token_budget: 1200
      priority_order: ["MUST", "REFERENCE"]
      namespace_filter: ["global", "project:*"]
      exclude_types: []
```

### 14.3 共享策略矩阵

| 操作 | 共享策略 | 说明 |
|------|---------|------|
| **写入** | 所有 Agent 写入同一存储 | 数据天然共享，`source_agent` 记录来源 |
| **读取** | 所有 Agent 可读取所有记忆 | Phase 1 不做隔离 |
| **注入** | 按 Agent 类型过滤 | 编程 Agent 不注入写作记忆（减少噪音） |
| **审核** | 用户一次审核，所有 Agent 受益 | 审核后的 `human_reviewed: true` 全局生效 |
| **遵循追踪** | 按 Agent 拆分统计 | `compliance.by_agent` 字段 |
| **遗忘** | 全局生效 | 一条记忆被归档，所有 Agent 不再收到 |

### 14.4 Phase 1 的多 Agent 范围

```
✅ 支持的场景：
  - Claude Desktop + Cline + Cursor 同时连接同一 MemVault 实例
  - 所有 Agent 共享同一份记忆数据
  - source_agent 记录写入来源
  - Router 简单按 Agent 类型过滤（硬编码规则）

❌ 暂不支持（Phase 3+）：
  - Agent 粒度的权限隔离（allowlist/denylist）
  - 跨设备同步（CRDTs）
  - 并发写入的语义冲突合并
  - Agent 专属命名空间
```

### 14.5 典型部署拓扑

```
┌─────────────────────────────────────────────────────────┐
│ 同一台机器                                               │
│                                                         │
│  ┌──────────────┐    ┌──────────────┐                   │
│  │ Claude Desktop│    │ VS Code      │                   │
│  │ (MCP Client)  │    │ (MCP Client) │                   │
│  └──────┬───────┘    └──────┬───────┘                   │
│         │                   │                            │
│         └─────────┬─────────┘                            │
│                   │ MCP 协议                              │
│         ┌─────────▼─────────┐                            │
│         │  MemVault Service  │                            │
│         │  (单一进程)        │                            │
│         │                    │                            │
│         │  ┌──────────────┐ │                            │
│         │  │ SQLite +     │ │                            │
│         │  │ 内嵌向量     │ │                            │
│         │  │ (共享存储)    │ │                            │
│         │  └──────────────┘ │                            │
│         └──────────────────┘                            │
└─────────────────────────────────────────────────────────┘
```

### 14.6 风险补充：多 Agent 场景专有风险

| 风险 | 严重程度 | 应对策略 |
|------|---------|---------|
| Agent A 写入错误记忆污染 Agent B | 🟡 中 | Phase 1 默认 Inbox 审核；Phase 3 引入 allowlist |
| 两个 Agent 写入矛盾记忆 | 🟡 中 | Dedup & Merge 模块；时间戳 LWW 策略 |
| Agent 身份伪造 | 🟡 中 | MCP 连接来源验证；Phase 2 引入 token 认证 |
| 记忆注入量翻倍（多 Agent 各自注入） | 🟢 低 | 每个 Agent 独立 Token Budget，互不影响 |
```

---

## 15. 三类记忆演进落地状态

> **规划**：原《三类记忆演进计划》（2026-08-26，v0.1；已并入本文档）。**实施快照**：2026-08-27，四阶段工作项全部落地（对应 §10 Phase 6）。

| 阶段 | 交付物 | 验收 |
|------|--------|------|
| **A 情景记忆** | `episodes` 表 + `memories.superseded_by`（migration 6-9）；`record_outcome`（MCP/CLI/REST）+ `GET /api/episodes`；教训反思 `reflection.rs`（SourceRole 守卫防自我强化）；`task_type` 教训注入（配额 ≤3，MUST 豁免）；Dashboard「Episodic」页 | ✅ H5：知识传达 0% → 90%（2026-08-26 CONFIRMED） |
| **B 程序记忆** | 技能 `trigger` × 意图匹配 → 结构化 `[SKILL]` 注入（成功率 ≥3 样本展示）；`skill_stats`（migration 10）；失败 → `needs-revision`，修订 `version+1`；重复成功自动沉淀技能草稿进审核队列 | ✅ H7：传达 0% → 78%、误触发 0/40（2026-08-27 CONFIRMED） |
| **C 语义记忆** | `memory_relations` 三元组（migration 11-13）+ LLM 抽取（`MEMVAULT_RELATIONS=on` 显式开启）；promote 事实巩固/实体归一（`consolidated_from`/`superseded_by` 溯源）；`supersede` 取代流程（检索默认排除、不物理删除、可回滚）；检索 `expand_relations` 一跳展开 | ✅ H6 三项指标全 CONFIRMED（2026-08-27） |
| **D 可选（已落地）** | 团队共享池（`visibility=shared`，跨命名空间注入上限 20）；SOP/Markdown 批量导入（`sop.rs` + CLI `import-skills` + MCP `import_skills`）；Obsidian 分类型目录同步（`folderFor`） | 全 workspace 测试绿（643 passed / 0 failed） |

> **图数据库集成**仍按计划推迟（DESIGN 原 Phase 5）：关系规模超单表一跳扩展收益点后再启动。

## 16. 远期规划（尚未实现）

> 本节承接原《三类记忆演进计划》（2026-08-26，v0.1）中**未实现**的部分，作为远期路线保留；原计划文档已在实现完成（2026-08-27）后归档删除，已落地细节见 §15。

| 项目 | 说明 | 状态 |
|------|------|------|
| **图数据库集成** | 关系规模超「单表 + 一跳扩展」收益点后再引入图数据库（替代/升级 `memory_relations` 的查询路径） | 未启动（按计划推迟，待规模信号触发） |
| **通用世界知识库** | 世界常识由模型自身承担，MemVault 只沉淀个人/项目/组织级领域知识 | 明确不做（设计约束） |
| **CRDTs 多端同步** | 多设备离线协作（DESIGN 原 Roadmap Phase 5 遗留项） | 未启动 |
| **插件市场发布** | VS Code / Obsidian / dsh 插件的上架与市场运营 | 未启动 |

### 源自 claude-obsidian 竞品分析（2026-08-28 归档）

> 对 [AgriciDaniel/claude-obsidian](https://github.com/AgriciDaniel/claude-obsidian)（v2.1.1，MIT）逐项代码核实的分析报告 `docs/CLAUDE-OBSIDIAN-REVIEW.md` 已完成核实并**归档删除**；已落地项（P0 注入安全包装 / P1 证据关系与证据驱动衰减 / P1.5 `memvault doctor`）详见 CHANGELOG [Unreleased]。以下为分析中**未实现**的候选，触发条件满足后再启动。

| 项目 | 说明 | 触发条件 / 状态 |
|------|------|------|
| **事务式写入协议（plan → sha256 → apply）** | 文件侧（Obsidian sync 等）写入改为「先出计划 → 校验 SHA-256 → 应用」，并配套 MCP `plan-approve-apply` 三阶段工具；当前 `obsidian-plugin/src/sync.ts` 仍是 create/update/skip 直写 | 暂缓——启动多 Agent 共享写入（§14）或人机审核流（§9.3）时再投入 |
| **REST `/api/memories/{id}/evidence` 端点** | 为 Dashboard 证据图谱提供按记忆查证据/被证关系的 REST 出口（MCP 已有 `add_evidence`） | 后续候选（Dashboard 需要时启动） |
| **`agent_adapt.rs::format_memories` treat-as-data 包装** | REST `/api/search` 返回格式路径补上与 P0 `router/format.rs` 同款的注入安全包装 | 后续候选（P0 时列为候选） |
| **Obsidian 插件记忆健康检查** | 插件侧完整 lint / 陈旧索引巡检（对标 claude-obsidian `lint_engine.py`）；现有 `detectOrphans` 仅做孤儿笔记清理 | 后续候选 |

> **原计划开放问题处理**：Q1（教训升 MUST 需人工确认）、Q2（episode 与 memories 1:1）、Q3（成功率最小样本 3 次）、Q4（关系抽取默认关闭、`MEMVAULT_RELATIONS=on` 显式开启）、Q5（教训默认仅命名空间内、global 需人工标记）——均已决策并随实现落地，无遗留待决项。

## 附录

### A. 参考项目

- [MemPalace](https://github.com/) — 记忆宫殿存储，96.6% R@5
- [Mem0](https://github.com/) — 42.5k stars，混合记忆层
- [Zep](https://github.com/) — 时序知识图谱
- [MCP 规范](https://spec.modelcontextprotocol.io/) — 协议标准
- [LanceDB](https://lancedb.github.io/) — 嵌入式向量库(调研过;v0.2 起未采用,向量存 SQLite 内嵌列)
- [Khoj](https://khoj.dev/) — Obsidian RAG
- [Automerge](https://automerge.org/) — CRDTs

### B. 关键决策记录

| 日期 | 决策 | 理由 |
|------|------|------|
| 2026-08-07 | 独立工具 + Thin Clients | 突破 Obsidian 天花板 |
| 2026-08-07 | Memory Router 为核心差异化 | 市场空白，无竞品 |
| 2026-08-07 | 存储层可插拔，兼容 MemPalace/Mem0 | 不重复造轮子 |
| 2026-08-07 | MUST/REF 记忆分级 | 解决"不遵循"问题 |
| 2026-08-07 | MCP 为首要协议 | 生态最好 |
| 2026-08-07 | Local-First | 隐私优先 |
| 2026-08-07 | 多 Agent 共享记忆：MCP Server 单实例 + Agent Registry | 所有 Agent 通过同一 MCP 服务读写，共享 SQLite 数据 |
| 2026-08-07 | Phase 1 不做 Agent 粒度的权限隔离 | 减少 MVP 复杂度，所有记忆对任何 Agent 可见 |
| 2026-08-07 | Router 按 Agent 类型过滤注入内容 | 编程 Agent 不收到写作相关记忆，减少噪音 |
| 2026-08-07 | **Phase 1 注入方式：MCP Resource + session_start** | **MCP 协议不支持消息拦截，Pre-Prompt Injection 推迟到 Phase 2 通过 MCP Proxy 实现** |
| 2026-08-07 | **Phase 1 Embedding：API fallback 优先** | **本地 bge-m3 在 CPU 上延迟高，Phase 2 增加本地模型** |
| 2026-08-07 | **Compliance Tracker 推迟到 Phase 3** | **标准 MCP Server 看不到 Agent 回复，需依赖 MCP Proxy** |

### C. 待验证假设

1. Pre-Prompt Injection 是否真的提升 Agent 遵循率？（A/B 测试）
2. 指令化格式 vs 描述性格式的遵循率差异？（目标：+20%）
3. 用户是否愿意花时间审核记忆？（审核率 > 30%）
4. Token Budget 设为多少最优？（1000 / 1500 / 2000 tokens）
5. Router 误注入率能否控制在 < 5%？
6. Obsidian 插件 → Dashboard 转化率？（目标 > 5%）

> **（2026-08 更新）**：H1–H4 与新增的 H5/H6/H7（三类记忆验收）均已实测 CONFIRMED，见 `docs/experiments/REPORT.md` 与本文档 §15。

---

> 文档更新至 **2026-08-27**：Phase 1–10 已全部交付。后续演进（如团队共享隔离、SOP 导入、情景/程序/语义三类记忆闭环）已落地并有验收证据，见 §15、§16 与 `docs/experiments/`。