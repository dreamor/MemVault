# MemVault

> AI Agent 时代的个人记忆路由器（Memory Router）

**不是让 Agent 学会查记忆，而是让记忆自动出现在 Agent 面前。**

## 核心能力

- **自动注入**：MCP Resource 启动时加载 + `session_start` 按 Agent 身份过滤
- **混合检索**：关键词 + 向量语义 + RRF 融合（3 种搜索模式）
- **MUST 保障**：MUST 级记忆永远不会被过滤或裁剪
- **多 Agent 差异化**：Agent Registry 按类型/tag 过滤（coding agent 不收 writing 记忆）
- **智能管道**：自动提取 / 去重 / 衰减 / 归档
- **生态覆盖**：CLI + MCP Server + Tauri Dashboard + VS Code + Obsidian

## 安装

```bash
git clone https://github.com/user/memvault.git && cd memvault
cargo build --release
```

二进制：`target/release/memvault-cli` 和 `target/release/memvault-mcp`

## 快速开始

```bash
# 保存 MUST 级偏好
memvault-cli save --content "用户偏好 Python" --priority MUST --type preference \
  --instruction "代码使用 Python，不用 Java" --tags "coding,python"

# 搜索
memvault-cli search --query "Python"

# 查看 Agent 注入
memvault-cli session-start --agent-id claude-desktop --context "帮我写代码"

# 从文本提取记忆
memvault-cli extract --text "I prefer dark mode. Our project uses Rust." --save

# 去重扫描
memvault-cli dedup

# 运行衰减
memvault-cli decay

# 导出/导入
memvault-cli export --format json --output ~/backup.json
memvault-cli import --format markdown --input ~/vault/memories/
```

## MCP Server 接入

### Claude Desktop

`~/Library/Application Support/Claude/claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "memvault": {
      "command": "/path/to/memvault-mcp",
      "args": ["--db", "~/.memvault/data.db"],
      "env": { "OPENAI_API_KEY": "sk-..." }
    }
  }
}
```

### Claude Code

```bash
claude mcp add memvault /path/to/memvault-mcp -- --db ~/.memvault/data.db
```

### 环境变量

| 变量 | 作用 | 默认值 |
|------|------|--------|
| `OPENAI_API_KEY` | 启用语义搜索 | (无，纯关键词模式) |
| `OPENAI_API_BASE` | Embedding API 地址 | `https://api.openai.com/v1` |
| `MEMVAULT_EMBEDDING_MODEL` | 模型名 | `text-embedding-3-small` |
| `MEMVAULT_EMBEDDING_DIM` | 向量维度 | `1536` |

## MCP Tools (8 个)

| Tool | 说明 |
|------|------|
| `save_memory` | 保存记忆（auto-embedding） |
| `search_memory` | 搜索（keyword / semantic / hybrid） |
| `session_start` | 按 Agent 身份返回注入上下文 |
| `review_memory` | 审核：approve / reject / edit |
| `delete_memory` | 删除记忆 |
| `extract_memories` | 从文本提取结构化记忆 |
| `run_dedup` | 去重扫描 |
| `run_decay` | 衰减 + 自动归档 |

## MCP Resources (2 个)

| URI | 说明 |
|-----|------|
| `memory://user-profile` | MUST 级强制规则（启动自动加载） |
| `memory://project-context` | REFERENCE 级项目上下文 |

## CLI 命令 (11 个)

`save` · `search` · `list` · `delete` · `session-start` · `resource` · `extract` · `dedup` · `decay` · `export` · `import`

## Dashboard (Tauri)

```bash
cd dashboard && npm install && npm run tauri dev
```

4 个页面：Memory List / Search / Review Queue / Stats

## 扩展

### VS Code Extension

`vscode-extension/` — 侧边栏记忆列表、搜索、右键保存选中文本

### Obsidian Plugin

`obsidian-plugin/` — 侧边栏面板、搜索、双向 Markdown 同步（导出到 vault / 从 vault 导入）

## Agent Registry

`~/.memvault/agents.yaml`:

```yaml
agents:
  - id: claude-desktop
    agent_type: coding-assistant
    inject_rules:
      max_memories: 8
      token_budget: 1500
      exclude_types: ["writing", "design"]
```

## 架构

```
memvault-core    — 12 模块：storage / router / intent / embedding /
                   hybrid / extractor / dedup / decay / io / models / config / error
memvault-mcp    — MCP Server (rmcp 3.1.1, stdio, 8 tools + 2 resources)
memvault-cli    — 11 subcommands
dashboard/      — Tauri 2.0 (React + TypeScript)
vscode-extension/ — VS Code Extension
obsidian-plugin/  — Obsidian Plugin
```

## 测试

```bash
cargo test  # 58 tests (49 unit + 9 E2E)
```

## License

MIT
