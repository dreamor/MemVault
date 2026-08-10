# 零入侵多 Agent 接入 + 召回率提升方案

## Context

当前 MemVault 的接入方式（MCP stdio / REST API）都需要 Agent 端主动配置。用户希望"无入侵或少入侵"地接入更多 Agent 并提高召回率。

**核心洞察**：几乎所有主流 AI Coding Agent 都有"自动读取项目指令文件"的机制。MemVault 只需要把记忆写入这些文件，Agent 就会自动读取——**零入侵、零配置、零修改**。

## 实施方案

### 1. `memvault sync` 命令 — 自动生成 Agent 指令文件

在当前项目目录下生成所有主流 Agent 的指令文件：

| 生成文件 | 覆盖 Agent | 格式 |
|---------|-----------|------|
| `CLAUDE.md` | Claude Code | Markdown |
| `AGENTS.md` | Copilot, Cursor, Windsurf, Codex CLI, Zed, Gemini | Markdown |
| `.github/copilot-instructions.md` | GitHub Copilot | Markdown |
| `.cursorrules` | Cursor (legacy) | Markdown |
| `.clinerules` | Cline / Roo | Plain text |

**两个文件覆盖 90% Agent**：`CLAUDE.md` + `AGENTS.md`。

### 2. 项目上下文检测 — 选择相关记忆

自动检测当前项目类型（读 package.json / Cargo.toml / go.mod / pyproject.toml），
选择与项目相关的记忆子集。

逻辑：
- MUST 级记忆：始终包含（全局规则）
- project:当前项目 namespace 记忆：始终包含
- tags 与项目技术栈匹配的 REFERENCE 记忆：包含
- 其他：跳过

### 3. 智能格式引擎 — 适配不同 Agent 的 token 预算

| Agent | 预算限制 | 策略 |
|-------|---------|------|
| GitHub Copilot | ~8000 字符 | 最简洁，只含 MUST + 关键 REF |
| Cline | 每轮发送 | 极简模式，< 2000 字符 |
| Claude Code | 无硬限制 | 完整版，含上下文说明 |
| 其他 | 按需 | 默认 5000 字符上限 |

### 4. Watch 模式 — 记忆变化时自动刷新 ✅ 已实现

`memvault sync --watch`：
- 监听 SQLite 数据库变化（polling，5 秒间隔）
- 存储 hash 检测变更（`COUNT(*) + MAX(updated_at)` → u64）
- 记忆增删改时自动重新生成指令文件
- Ctrl+C 优雅停止
- 适合作为 daemon 后台运行

### 5. 召回率在静态文件场景的优化

静态指令文件没有实时搜索能力，因此需要最大化"放入文件的记忆的相关性"：

- **分层注入**：MUST 放文件顶部（Agent 更可能遵循开头内容）
- **项目匹配**：自动检测技术栈，只注入相关记忆
- **衰减过滤**：decay_score 低的记忆不写入文件
- **去重压缩**：合并语义相似的记忆为一条
- **指令化格式**：所有内容以命令式（"Always..."/"Never..."）写入，提升遵循率

## 修改文件

| 文件 | 改动 |
|------|------|
| `crates/memvault-core/src/sync.rs` | **新文件**：项目上下文检测 + 文件生成引擎 |
| `crates/memvault-cli/src/main.rs` | 添加 `sync` 子命令（含 --watch） |
| `crates/memvault-core/src/lib.rs` | 导出 sync 模块 |

## 验证（已完成）

1. ✅ `cargo test` 全部通过
2. ✅ `memvault sync` 生成 5 种指令文件（CLAUDE.md / AGENTS.md / copilot-instructions.md / .cursorrules / .clinerules）
3. ✅ `memvault sync --watch` 轮询检测变更并自动重新生成
4. ✅ SyncEngine 单元测试 + 集成测试
5. ✅ Core 覆盖率 89.17%
