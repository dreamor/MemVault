# MemVault

> AI Agent 时代的个人记忆路由器（Memory Router）

**不是让 Agent 学会查记忆，而是让记忆自动出现在 Agent 面前。**

核心差异化：所有现有记忆工具都是"被动 Tool"——Agent 必须主动调用才能读取记忆。MemVault 通过 MCP Resource 自动注入 + `session_start` 按 Agent 身份过滤，让记忆主动出现在 Agent 上下文中。

## 安装

```bash
git clone https://github.com/user/memvault.git
cd memvault
cargo build --release
```

二进制文件在 `target/release/` 下：
- `memvault-cli` — 命令行工具
- `memvault-mcp` — MCP Server

## 快速开始

### 1. 保存记忆

```bash
# 保存 MUST 级偏好（Agent 必须遵循）
memvault-cli save \
  --content "用户偏好 Python，不用 Java" \
  --priority MUST \
  --type preference \
  --instruction "代码使用 Python，不用 Java" \
  --tags "coding,python"

# 保存 REFERENCE 级事实
memvault-cli save \
  --content "当前项目使用 FastAPI + PostgreSQL" \
  --priority REFERENCE \
  --type fact \
  --tags "coding,project"
```

### 2. 搜索记忆

```bash
memvault-cli search --query "Python"
```

### 3. 查看 Agent 注入效果

```bash
# 模拟 claude-desktop 的 session_start（coding agent，过滤 writing 记忆）
memvault-cli session-start --agent-id claude-desktop

# 查看 MCP Resource 内容
memvault-cli resource "memory://user-profile"
```

## MCP Server 接入

### Claude Desktop

在 `~/Library/Application Support/Claude/claude_desktop_config.json` 中添加：

```json
{
  "mcpServers": {
    "memvault": {
      "command": "/absolute/path/to/memvault-mcp",
      "args": ["--db", "~/.memvault/data.db"]
    }
  }
}
```

重启 Claude Desktop 后，MemVault 会自动注册为 MCP Server。你可以：
- 让 Claude 调用 `save_memory` 保存记忆
- 让 Claude 调用 `search_memory` 搜索记忆
- 让 Claude 调用 `session_start` 获取按身份过滤的注入上下文
- Claude 启动时自动加载 `memory://user-profile` Resource

### Claude Code

```bash
claude mcp add memvault /path/to/memvault-mcp -- --db ~/.memvault/data.db
```

### Cursor / 其他 MCP Client

在 MCP 配置中添加 stdio transport：

```json
{
  "command": "/path/to/memvault-mcp",
  "args": ["--db", "~/.memvault/data.db"]
}
```

### 验证接入

接入后，在 Agent 中执行：
1. 要求 Agent 调用 `save_memory` 保存一条测试记忆
2. 要求 Agent 调用 `search_memory` 搜索该记忆
3. 要求 Agent 调用 `session_start` 查看注入格式

## MCP Tools

| Tool | 说明 |
|------|------|
| `save_memory` | 保存记忆（偏好/事实/事件/实体/技能），支持 MUST/REFERENCE/BACKGROUND 优先级 |
| `search_memory` | 按关键词搜索，支持类型/优先级/命名空间过滤 |
| `session_start` | 新会话开始时调用，返回按 Agent 身份过滤的 MUST/REF 指令化记忆 |
| `review_memory` | 审核记忆：批准/拒绝/编辑 |
| `delete_memory` | 按 ID 删除记忆 |

## MCP Resources

| URI | 说明 |
|-----|------|
| `memory://user-profile` | 用户核心偏好和 MUST 级强制规则（启动时自动加载） |
| `memory://project-context` | 当前项目上下文和 REFERENCE 级记忆 |

## Agent Registry 配置

默认使用硬编码的 Agent 注册表。如需自定义，创建 `~/.memvault/agents.yaml`：

```yaml
agents:
  - id: claude-desktop
    agent_type: coding-assistant
    description: "Claude Desktop — daily coding assistant"
    inject_rules:
      max_memories: 8           # 最多注入 8 条
      token_budget: 1500        # token 预算
      priority_order: ["MUST", "REFERENCE"]
      namespace_filter: ["global", "project:*"]
      exclude_types: ["writing", "design"]  # 不注入写作/设计相关记忆

  - id: default
    agent_type: general-assistant
    description: "Default profile for unregistered agents"
    inject_rules:
      max_memories: 8
      token_budget: 1500
      priority_order: ["MUST", "REFERENCE"]
      namespace_filter: ["global"]
      exclude_types: []
```

完整示例见 [agents.example.yaml](agents.example.yaml)。

## 记忆优先级

| 级别 | 标签 | 含义 |
|------|------|------|
| **MUST** | `[MUST]` | Agent 必须遵循，永远不会被过滤或裁剪 |
| **REFERENCE** | `[REF]` | Agent 可参考，相关时使用，可能被 Token Budget 裁剪 |
| **BACKGROUND** | `[BG]` | 背景信息，低优先级 |

## 架构

```
memvault-core   — 存储引擎 + Memory Router + 意图分析 + 数据模型
memvault-mcp    — MCP Server（rmcp 3.1.1, stdio transport）
memvault-cli    — 命令行工具
```

**Memory Router 管道**：
```
全量记忆 → exclude_types 过滤（MUST 豁免）
         → 意图分析过滤
         → Token Budget 裁剪（MUST 豁免）
         → 数量限制
         → MUST/REF 指令化格式输出
```

## 开发

```bash
# 构建
cargo build

# 测试（24 个：15 单元 + 9 E2E）
cargo test

# Debug 日志
RUST_LOG=debug cargo run --bin memvault-cli -- session-start
```

## 路线图

- **Phase 1** ✅ Core Engine + Memory Router + MCP Server
- **Phase 2** 检索增强（BM25 + 向量混合）+ MCP Proxy
- **Phase 3** Tauri Dashboard + 遵循度追踪
- **Phase 4** 智能管道（自动提取/去重/衰减）
- **Phase 5** 生态扩展（Web/移动端/插件市场）

See [PLAN.md](PLAN.md) for details.

## License

MIT
