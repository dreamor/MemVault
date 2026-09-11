<div align="center">

<img src="assets/memvault-logo.png" alt="MemVault — 怀抱记忆坚果的蜜金仓鼠吉祥物" width="360" />

# MemVault

### 每个 AI Agent 的共享记忆层

> 不是让 Agent 学会查记忆,而是让记忆自动出现在 Agent 面前。

**MCP 原生 &nbsp;·&nbsp; 混合检索 &nbsp;·&nbsp; 自动注入 &nbsp;·&nbsp; 零配置同步**

**开源 &nbsp;·&nbsp; 自托管 &nbsp;·&nbsp; 私有 &nbsp;·&nbsp; MIT 许可**

[![CI](https://img.shields.io/github/actions/workflow/status/dreamor/memvault/ci.yml?style=flat-square&branch=master)](https://github.com/dreamor/memvault/actions) [![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg?style=flat-square)](LICENSE) [![Rust](https://img.shields.io/badge/Rust-stable-orange.svg?style=flat-square)](https://www.rust-lang.org) [![MCP](https://img.shields.io/badge/MCP-compatible-blue.svg?style=flat-square)](https://modelcontextprotocol.io) [![Status](https://img.shields.io/badge/status-beta-yellow.svg?style=flat-square)](#项目状态)

[English](README.md) &nbsp;·&nbsp; **简体中文**

<div align="center">

如果 MemVault 确实帮你解决了实际问题,一颗 Star 就能帮到更多人。

**[⭐ 在 GitHub 上点亮](https://github.com/dreamor/memvault)** &nbsp;·&nbsp; **[报告 Bug](https://github.com/dreamor/memvault/issues/new)**

</div>

```bash
curl -fsSL https://raw.githubusercontent.com/dreamor/memvault/master/scripts/install.sh | bash
```

</div>

---

每个 AI Agent 的会话都是从零开始的。Claude Desktop 不知道 Cursor 刚刚学会了什么;DeepSeek Harness(dsh)也不知道你昨天跟 Claude Code 说过什么。不管你用的是国外的还是国内的 Agent、IDE 插件还是命令行 harness,它每次开启新对话都会忘记你的偏好。

你一直在手动重复上下文——项目约定、个人偏好、历史决策——而这些本应让已经认识你的 Agent 自动知晓。这并非模型的局限,而是一层缺失的基础设施。

MemVault 就是这一层。它是一个轻量、自托管的记忆路由器,位于你的 Agent 与它们的上下文之间。它说的是标准 MCP——没有 MemVault 专属 SDK,没有针对某个 Agent 的专门集成。**任何 MCP 兼容的 Agent,不管来自哪个厂商,一接入就自动共享同一份持久记忆。**

**适合谁用:**

- **任何在用支持 MCP 的 Agent 的人**——Claude Code、Claude Desktop、Cursor、Cline、Continue、DeepSeek Harness(dsh),或任何其它 MCP 客户端,不论国内国外——希望偏好、项目上下文和历史决策跨会话持久保存,无需重复
- **多 Agent 进阶用户**,同时在上面这些不同厂商、不同模型的 Agent 之间切换——全部共享同一份记忆,无需配置
- **平台团队**,在混合 Agent 环境里部署需要一致性的 AI 辅助工作流:代码评审约定、架构决策、项目专属偏好
- **任何不想把同一件事告诉 AI 两遍的人**——MemVault 的工作方式如同大脑本应的工作方式:你说一次,不管是哪个 Agent 在问,需要时它就在那儿

**[快速开始](#快速开始)** &nbsp;·&nbsp; **[工作原理](#工作原理)** &nbsp;·&nbsp; **[MemVault 能给你什么](#memvault-能给你什么)** &nbsp;·&nbsp; **[为什么选择 MemVault](#为什么选择-memvault)** &nbsp;·&nbsp; **[MCP 服务接入](#mcp-服务接入)** &nbsp;·&nbsp; **[CLI 命令](#cli-命令)** &nbsp;·&nbsp; **[集成](#集成)** &nbsp;·&nbsp; **[架构](#架构)** &nbsp;·&nbsp; **[项目状态](#项目状态)** &nbsp;·&nbsp; **[测试](#测试)** &nbsp;·&nbsp; **[文档](#文档)** &nbsp;·&nbsp; **[贡献与社区](#贡献与社区)** &nbsp;·&nbsp; **[许可证](#许可证)**

---

## 快速开始

```bash
# 安装(推荐,Linux / macOS 官方脚本,自动校验 SHA-256)
curl -fsSL https://raw.githubusercontent.com/dreamor/memvault/master/scripts/install.sh | bash
export PATH="$HOME/.memvault/bin:$PATH"

# Windows(PowerShell):
#   powershell -ExecutionPolicy Bypass -File scripts\install.ps1

# 或发布 crates.io 后:
#   cargo install memvault-cli memvault-mcp

# 保存一条 MUST 级偏好(以指令形式注入,Agent 必须遵守)
memvault save --content "用户偏好 Python" --priority MUST --type preference \
  --instruction "代码用 Python,不用 Java" --tags "coding,python"

# 跨全部记忆检索(混合检索,配置嵌入后可启用语义检索)
memvault search --query "Python"

# 查看某个 Agent 接入时会注入哪些上下文
memvault session-start --agent-id claude-desktop --context "帮我写代码"

# 从自由文本中抽取结构化记忆
memvault extract --text "我喜欢深色模式。我们的项目用 Rust。" --save

# 根据记忆自动生成 Agent 指令文件
memvault sync

# 一站式:去重、衰减、归档过期记忆
memvault dedup && memvault decay
```

**5 秒验证安装是否成功:**

```bash
memvault-cli --version
# memvault 0.3.0

# 冒烟检查:列出已保存记忆(验证数据库正常)
memvault-cli list
```

### 本地 Ollama 演示(零成本,不出本机)

MemVault 对本地 Ollama「发现即用」:LLM 提取(全文理解/失败反思/关系抽取)未配置时
自动探测本机 Ollama;嵌入用 `ollama` 或 `auto` provider 走本地模型。

```bash
# 1. 安装并启动 Ollama
brew install ollama && brew services start ollama    # 或官网安装包

# 2. 拉取模型
ollama pull nomic-embed-text        # 嵌入,768 维(ollama/auto 默认)
ollama pull qwen2.5:3b-instruct     # chat:LLM 提取/反思(默认 qwen2.5:7b,小机器用 3b)

# 3.(可选)显式固定 provider——持久化写进 ~/.memvault/.env
#    (shell export 也仍然有效——环境变量优先级高于文件——只是重启后就没了)
mkdir -p ~/.memvault
cat >> ~/.memvault/.env <<'EOF'
MEMVAULT_EMBEDDING_PROVIDER=ollama
MEMVAULT_LLM_EXTRACTION_PROVIDER=ollama
MEMVAULT_LLM_EXTRACTION_MODEL=qwen2.5:3b-instruct
EOF

# 4. 验证
memvault status     # Embedding provider: configured and reachable
memvault save --content "构建服务器 IP 是 10.20.30.40"   # 输出 (embedded int8)
memvault outcome --task "部署交易服务" --status failure --cause "磁盘空间不足" --task-type deploy
#   → Lesson (Llm): ... 表示失败反思走了本地 LLM(而非规则回退)
```

不配置任何东西(环境变量和 `~/.memvault/.env` 都不设)时:LLM 提取自动探测到本机 Ollama 即启用
(默认模型 `qwen2.5:7b`,需提前 `ollama pull qwen2.5:7b`,或用 `MEMVAULT_LLM_EXTRACTION_MODEL` 指向已装模型);
嵌入默认仍是进程内 native,在 `~/.memvault/.env` 里设 `MEMVAULT_EMBEDDING_PROVIDER=auto`
即可让 Ollama 优先、未运行时回退 native。

---

## 工作原理

MemVault 是一条流水线,而不是单个脚本。下面每一级都是一个已发布的模块:

```
Agent 连接 (MCP stdio/SSE)
        │
        ▼
┌───────────────────────┐
│  Agent 路由            │  ← 匹配 Agent 类型/标签 → 过滤相关记忆
│  (Agent 注册表)         │
└─────────┬────────────┘
          │
          ▼
┌───────────────────────┐
│  记忆检索               │  ← 关键词 (BM25) + 向量 (嵌入) + RRF 融合
│  (3 种搜索模式)         │     同义词扩展 · 打分 · 软过滤
└─────────┬──────────────┘
          │
          ▼
┌───────────────────────┐
│  自动注入               │  ← MUST 级 → 指令提示词
│                        │     REFERENCE → 上下文资源
│                        │     NORMAL    → 搜索结果
└─────────┬──────────────┘
          │
          ▼
  Agent 收到上下文 ──→ 做出更好的决策
```

- **存储:** SQLite,内置 FTS5(全文搜索);embedding 以 int8 量化存储(约为 f32 的 1/4 体积且排序质量几乎不变,旧 f32 行仍可读取)
- **检索:** 基于 FTS5 的 BM25 关键词搜索,带 CJK bigram 分词(中文两字词可正确命中)与三档匹配降级(严格→放宽单字→同义词 OR,放宽必上报、绝不静默);本地优先的 embedding(默认进程内 native,可切换本地 Ollama 或任意 OpenAI 兼容模型)、RRF 融合(每条结果附召回来源 kw#2/vec#5)、同义词扩展、相关度打分、软意图过滤
- **流水线:** 自动实体抽取、save 时 delta 写入(近重复跳过、相似项只吸收残差)、语义去重、基于时间的衰减、过期记忆归档
- **同步:** 零入侵文件生成——`memvault sync` 直接从数据库内容生成 CLAUDE.md、AGENTS.md 等

---

## MemVault 能给你什么

- **自动注入上下文:** 会话开始即按 Agent 身份自动拉取相关记忆——MUST 级规则以指令形式落地,而非仅作为聊天历史
- **混合检索:** BM25 + 向量 + RRF 融合,带同义词扩展、相关度打分与逐条召回来源留痕(哪一路、第几名召回了它)——可通过 CLI、MCP 工具或 REST API 调用
- **可解释注入:** 注入链路上每一条被丢弃的候选都记录原因(预算/上限/意图与类型惩罚)——「为什么这条记忆没进 Agent 上下文」永远有答案
- **MUST 强制约束:** MUST 优先级的记忆永不被过滤或截断。始终在上下文中,始终被遵守——信任来自内容出处(人工撰写/已审核),可选地兜底为"被多个身份已验证的 Agent 独立印证"(`MEMVAULT_CORROBORATION_GATE`,默认关闭),避免单个被冒充/劫持的 Agent 就能单方面注入一条必须遵守的 MUST
- **多 Agent 感知:** Agent 注册表提供基于类型/标签的软过滤(降分,而非硬排除)
- **MCP 代理:** 透明代理,可向**任意**上游 MCP 服务器的响应注入记忆——客户端零改动
- **合规追踪:** `inject_session_id` 记录注入了什么,并度量指令遵守率
- **跨平台:** CLI + MCP Server(stdio 与 SSE)+ Web Dashboard(浏览器)+ Obsidian 插件
- **零侵入同步:** 按需从记忆生成 AGENTS.md / CLAUDE.md——无需为每个 Agent 改配置
- **本地优先的上下文提取:** 默认纯规则关键词提取;可选让 LLM 理解完整的用户+助手对话,自动探测本机 Ollama 并优先免费本地跑,不会一上来就打远程 API
- **历史与回滚:** 每次更新/删除都会快照进 `memory_history`——`memvault checkpoints` + `memvault restore` 即可单条回滚,不影响其它记忆
- **能力自检:** `memvault status` 明确列出未配置 embedding provider 时哪些功能会降级,并输出 schema 指纹(迁移版本+checksum)便于跨库比对
- **情景记忆闭环:** `record_outcome` 上报任务结果(success / failure / partial),失败自动蒸馏为教训并注入后续同类任务(REFERENCE,升 MUST 必须人工确认)——不记同一个坑
- **程序记忆激活:** 技能 `trigger` 命中意图即按结构化 `[SKILL]` 块注入(带成功率,≥3 次样本才展示);失败自动打待修订标记、人工修订后 `version+1`;同类型 ≥3 次成功→自动沉淀技能草稿进审核队列
- **语义知识链接:** 轻量三元组关系,重复事实巩固为关联语义事实(带来源溯源),被取代事实归档并不再检索注入(列表可查、可回滚)
- **团队共享与 SOP 导入:** 标记 `shared` 的记忆注入任意会话(上限 20 条);支持从 Markdown SOP 批量导入可验证技能
- **save 时 delta 写入:** 每次保存先在同命名空间查重——近重复直接跳过,相似记忆只吸收**残差**(真正新增的部分)并刷新强度,让记忆库收敛而不是近义堆积;`--force` / `force_insert` 旁路
- **任务级评测:** `memvault bench` 以你自己的失败历史(蒸馏出教训的 episode)为样本,度量教训检索率/注入率;加 `--judge` 后由 LLM 对照已知失败原因评分"无记忆方案 vs 带记忆方案"——看的是**任务成功率提升**,不只是检索召回率
- **两阶段注入(绝不阻塞):** MUST 规则走确定性解析(零 embedding 调用)即时可用;语义管线在后台预取,短时间内(250ms)落地;超时则直接用确定性基线放行,请求永不被 embedding 延迟劫持(设计受 Qwen3.8-Flash-Next 技术报告启发)
- **会话 n-gram 检索:** 检索键由最近若干轮上下文构成、按新近度加权——当前焦点主导检索,而非一句平铺的查询
- **单一规范注入通路:** `agents.yaml` 中按 Agent 配置 `inject_channel`(mcp / proxy / sync),自动注入只走一条通路,同一记忆不会经多条路重复送达同一 Agent
- **数据属于你:** 单一 SQLite 文件,完整导出/导入,无云端依赖。你的数据,在你的机器上

---

## 为什么选择 MemVault

| 能力 | 纯 CLAUDE.md | 向量库 + RAG | **MemVault** |
|---|---|---|---|
| **上下文注入** | 手工编辑 | 仅查询时 | 会话开始自动 |
| **多 Agent 共享** | 复制粘贴 | 各自索引 | 单一共享存储 |
| **MUST 强制执行** | 无 | 无 | 指令层注入 |
| **搜索模式** | 文件 grep | 仅嵌入 | BM25 + 向量 + 混合 |
| **同义词扩展** | 无 | 无 | 内置 |
| **去重** | 无 | 无 | 语义去重流水线 |
| **写入时 delta 合并** | 无 | 无 | 保存即查重:近重复跳过、相似项吸收残差 |
| **任务级评测** | 无 | 仅检索指标 | `bench`:开/关记忆的任务成功率差值 |
| **衰减 / 归档** | 无 | 无 | 基于时间 + 自动归档 |
| **记忆提取** | 手动 | 不适用 | 默认规则提取;可选本地优先 LLM 提取 |
| **MCP 原生** | 无 | 无 | stdio + SSE + Proxy |
| **Agent 区分** | 全局文件 | 查询过滤 | 类型/标签注册表 |
| **合规追踪** | 无 | 无 | inject_session_id + 完成率 |
| **自托管** | 是 | 视情况 | 单一二进制,无需云端 |

与任何 Agent 现有设定共存,而非替代。你的 LLM、你的 IDE、你的工作流保持原样。MemVault 只是在底层补上记忆层。

---

## MCP 服务接入

> Tier-1 的 agent(Claude Code、OpenCode、dsh、Gemini CLI、Codex)有一条命令的原生插件安装——先看[「集成」](#集成)。以下内容是其它任意 MCP 客户端的通用兜底。

### stdio(任意标准 MCP 客户端)

MemVault 说的是标准 MCP stdio——同一份 `mcpServers` JSON 在 Claude Desktop、Cursor、Cline、Continue 以及任何读这种格式的客户端上都能原样用:

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

个别客户端有自己的一行式命令,不用手改 JSON:

```bash
# Claude Code
claude mcp add memvault /path/to/memvault-mcp -- --db ~/.memvault/data.db
```

**DeepSeek Harness(dsh)**——一个国产 Agent Harness——享有比标准 stdio 配置更深的接入方式:仓库自带的原生 Cordis 插件(`dsh-plugin/`)能自动把记忆注入 system prompt、每轮结束自动抽取,不需要 agent 每轮主动配合。零代码接入和深度插件两种方式详见 [docs/INSTALL.md §2.5](docs/INSTALL.md#25-deepseek-harness-dsh)。

其它支持 MCP 的 Agent——不论国内国外、IDE 插件还是命令行 harness——理论上都能用同样的方式接入:任何实现标准 MCP stdio/SSE 的客户端,MemVault 侧都不需要改动。上面列的是我们实际验证过的;如果你在别的 Agent 上跑通了,欢迎提 PR 补充这个列表。

### SSE(多客户端、可网络访问)

```bash
memvault-mcp --transport sse --port 3777
# 客户端连接地址: http://127.0.0.1:3777/mcp
```

SSE 特性:多客户端同时连接、初始化时自动触发嵌入向量回填、HTTP 远程访问。

> **注意:** `--transport sse` 只挂载 MCP-over-HTTP 端点(`/mcp`),**不会**暴露 REST API(`/api/*`)。Web Dashboard 由 REST 后端托管(`memvault-mcp --transport http --serve-web <dist>`),Obsidian 插件同样走 REST API,必须改用 `--transport http`。详见 [docs/INSTALL.md §2.6](docs/INSTALL.md#26-rest-apiobsidian-客户端专用)。

### 18 个 MCP 工具

| 工具 | 说明 |
|------|------|
| `save_memory` | 保存并自动生成嵌入向量;默认 delta 写入(近重复跳过、相似合并),`force_insert` 旁路 |
| `record_outcome` | 上报任务结果(情景记忆);失败自动反思生成教训 |
| `import_skills` | 从 Markdown SOP 导入技能(标题→技能,列表项→步骤) |
| `search_memory` | 关键词 / 语义 / 混合 |
| `session_start` | 按 Agent 身份注入上下文;遵循 Agent 的 `inject_channel`(另一通路为规范时跳过并说明) |
| `review_memory` | 批准 / 拒绝 / 编辑 |
| `delete_memory` | 删除一条记忆 |
| `extract_memories` | 从文本中结构化抽取 |
| `run_dedup` | 去重扫描 |
| `run_decay` | 衰减 + 自动归档 |
| `confirm_read` | 标记已读(更新 access_count) |
| `list_inbox` | 列出待人工审核的记忆 |
| `run_promote` | 提升流水线(L1→L2→L3),把来源归档到 L0 |
| `report_compliance` | 上报某次注入会话的遵循/违规状态 |
| `get_compliance_report` | 按会话或汇总的合规率 |
| `add_evidence` | 记录记忆间证据关系(supports / contradicts / sourced_from) |
| `get_memory_evidence` | 获取一条记忆蒸馏来源的原始证据链(L0 trace 行)及证据画像——只读溯源,让 Agent 能引用原始会话文本并注明来源 |
| `get_effectiveness_report` | 已注入记忆的自动效果判定(有用/中性/有害/上下文不足占比,由 `record_outcome` 自动判定)——独立于手动的 `report_compliance` 流程 |

### 2 个 MCP 资源

| URI | 内容 |
|-----|------|
| `memory://user-profile` | MUST 级规则,连接时自动加载 |
| `memory://project-context` | REFERENCE 级项目上下文 |

### 配置(.env 文件与环境变量)

推荐把 [.env.example](.env.example) 复制为 `~/.memvault/.env` 后按需取消注释——它同时是全部配置键的唯一事实来源文档。

取值优先级(高 → 低):**CLI flag > 进程环境变量 > `~/.memvault/.env` > 内置默认**。所有 binary 启动时最先加载 env 文件;`--env-file <路径>` 或 `MEMVAULT_ENV_FILE` 可指定其他文件,文件不存在则静默跳过(零配置即可用)。`memvault status` 可打印每项配置的实际来源(env / file / default)。

两类东西刻意不在下表:宿主安装合同变量(`MEMVAULT_AGENT_ID`/`MEMVAULT_HOOK_EXTRACT` 等——per-agent 值,由各 agent 的 plugin/mcpServers env 注入);proxy 上游拓扑(`upstreams` 结构化列表,在 `~/.memvault/proxy.yaml`)。

| 变量 | 用途 | 默认值 |
|------|------|--------|
| `MEMVAULT_EMBEDDING_PROVIDER` | 提供商:`native`(进程内推理,默认)、`auto`(Ollama 优先,native 兜底)、`ollama`/`local`、`openai`、`openai-compatible`(任意 OpenAI 兼容端点) | `native` |
| `MEMVAULT_EMBEDDING_API_KEY`(旧名兜底:`OPENAI_API_KEY`) | 远端提供商的 API Key(本地 Ollama 不需要);默认 `native` 本地模型无需 key | (无需 — native 本地模型) |
| `MEMVAULT_EMBEDDING_API_BASE` | 任意 OpenAI 兼容端点(OpenAI / Azure / vLLM / 网关…)。`ollama`/`local` 时走 Ollama 原生端点 `http://localhost:11434/api` | `https://api.openai.com/v1` / `http://localhost:11434/api`(Ollama) |
| `MEMVAULT_EMBEDDING_MODEL` | 嵌入模型:native 用 `bge-small-zh`(中文,~95MB)/`multilingual`/`e5-base`;ollama 用 `nomic-embed-text`(768 维);API 提供商填具体模型名 | `bge-small-zh`(native)/ `nomic-embed-text`(Ollama)/ `text-embedding-3-small`(API) |
| `MEMVAULT_EMBEDDING_DIM` | 向量维度 | `768`(本地/Ollama)/ `1536`(API) |
| `MEMVAULT_LLM_EXTRACTION_PROVIDER` | 可选:开启基于 LLM 的**上下文**记忆提取(理解完整的用户+助手对话,而非逐行关键词匹配)。不设置或 `auto` → **本地优先**:自动探测本机是否跑着 Ollama,有就零配置直接用(免费、不出本机),没有则保持纯规则提取。`openai`/`openai-compatible`/自定义值 → 显式指定远程提供商(不会因为别处配了 API key 就自动启用远程——远程调用有真实成本和幻觉风险)。`off`/`disabled`/`none` → 强制纯规则提取,即使本机有 Ollama 在跑 | (未设置——本地优先,无本地 Ollama 时纯规则) |
| `MEMVAULT_LLM_EXTRACTION_API_KEY`(回退到 `OPENAI_API_KEY`)/ `MEMVAULT_LLM_EXTRACTION_API_BASE` / `MEMVAULT_LLM_EXTRACTION_MODEL` | LLM 提取所用 chat/completions 端点配置 | 本地:`http://localhost:11434/v1` / `qwen2.5:7b`(无需 key)——远程:`https://api.openai.com/v1` / `gpt-4o-mini` |
| `MEMVAULT_RELATIONS` | 可选 LLM 关系抽取:`true` 时 `extract_memories`(mode=llm) 额外持久化 `supports`/`contradicts`/`sourced_from` 三元组 | (未设置/false) |
| `MEMVAULT_DELTA_WRITE` | save 时 delta 写入:同命名空间先查重,近重复跳过、相似项吸收残差。`false` 关闭(兼容 on/off/1/0 等别名);单次旁路用 `--force` / `force_insert` | true |
| `MEMVAULT_CONTEXT_NGRAM_WINDOW` | proxy 自动注入构造"按新近度加权检索键"所用的最近观察轮数 | `5` |
| `MEMVAULT_HOOK_EXTRACT_MIN_FRICTION` | Stop hook 触发的 `extract` 落库所需的最低摩擦分(工具重试出错 / 拒绝工具调用 / 会话中纠正);设为 `0` 关闭门控,退回"每次 Stop 都抽取"的旧行为 | `1` |
| `MEMVAULT_IDENTITY_VERIFICATION` | 记录 `save_memory` 调用的 `agent_id` 是否真的通过了 `agents.yaml` 注册 key 的校验（`Memory.identity_verified`），而非处于未鉴权模式。`false` 关闭记录（兼容 on/off/1/0 等别名）;仅记录本身不改变信任判定 | true |
| `MEMVAULT_CORROBORATION_GATE` | 可选的 MUST 信任门槛:一条 MUST 记忆被足够多不同的已验证 Agent 独立印证（见下一项）即视为可信,即使未经人工审核。`true` 开启——默认关闭,不开启则 `is_trusted` 行为不变 | false |
| `MEMVAULT_CORROBORATION_MIN_AGENTS` | 上述印证门槛所需的最少不同已验证 Agent 数 | `2` |
| `MEMVAULT_DB_POOL_SIZE` | SQLite 连接池大小 | `5` |
| `MEMVAULT_CORS_ORIGIN` | REST 允许的 CORS 来源(逗号分隔;未设置仅本机) | (仅本机) |
| `MEMVAULT_DB` | 数据库路径 | `~/.memvault/data.db` |
| `RUST_LOG` | 日志级别 | `info` |

---

## CLI 命令

`save` · `outcome` · `search` · `list` · `review` · `delete` · `session-start` · `resource` · `extract` · `dedup` · `decay` · `doctor` · `promote` · `backup` · `export` · `import` · `import-skills` · `import-agent` · `ingest` · `confirm-read` · `sync` · `checkpoints` · `restore` · `supersede` · `status` · `bench` · `eval-history`

```bash
memvault <命令> --help   # 每个命令的详细用法
```

### 常用命令

| 命令 | 作用 |
|---------|--------------|
| `save` | 保存一条记忆,支持优先级、类型、可选指令。默认 delta 写入:近重复跳过、相似记忆吸收残差;`--force` 旁路 |
| `outcome` | 记录任务结果(success / failure / partial);失败自动蒸馏为教训注入后续同类任务 |
| `search` | 混合检索 + 相关度打分,参数:`--query`、`--top-k`、`--namespace` |
| `session-start` | 模拟 Agent 接入时会收到的上下文;多行 `--context` 视为轮次序列并按新近度加权 |
| `extract` | 解析自由文本,抽取结构化记忆 |
| `import-skills` | 从 Markdown SOP(`# / ##` 标题→技能,列表项→步骤)导入技能;默认进入审核收件箱,除非加 `--approve` |
| `import-agent` | 冷启动导入:读取其他 Agent 的原生记忆文件——Claude Code/Desktop(`CLAUDE.md`/auto-memory)、Codex CLI(`AGENTS.md`)、Hermes Agent(`USER.md`/`MEMORY.md`/skills)、Qoder(`.qoder/rules`)、OpenClaw(实验性);`--scan` 仅探测不写库,`--path` 手动指定路径,`--paste`/stdin 作为其他任意 Agent 的通用兜底,默认进入审核收件箱,除非加 `--approve` |
| `sync` | 根据记忆生成 Agent 指令文件(AGENTS.md / CLAUDE.md / MEMORY-INDEX.md 等,带 `--watch`) |
| `dedup` | 扫描并合并语义重复的记忆(配置了 embedding provider 时启用向量辅助去重) |
| `checkpoints` | 列出记忆历史快照(单条或全局);参数:`--memory-id`、`--limit` |
| `restore` | 按历史快照回滚单条记忆(`--history-id`) |
| `supersede` | 归档旧事实并指向替代事实(不删除任何东西;搜索跳过已取代记录,列表仍可见) |
| `status` | 显示 embedding provider 就绪状态(区分「未配置 / 显式禁用 / 已配置但不可用」三种情况),以及缺失时哪些功能会降级 |
| `doctor` | 只读记忆卫生巡检:悬空/陈旧/重复/反证 + `--json` 机器可读 |
| `bench` | 任务级记忆基准:以你自己的 outcome 历史为样本,度量教训检索率/注入率;`--judge` 追加 LLM 评分的"无记忆方案 vs 带记忆方案"成功率差值;每次运行自动落库供 `eval-history` 查看 |
| `eval-history` | 历史 `bench`/`doctor` 运行的时间趋势视图——每次运行自动归档,此命令只负责列出累积结果 |
| `decay` | 基于访问新鲜度归档过期记忆 |
| `backup` | 创建一致的 SQLite 时间点备份 |
| `export` / `import` | 备份与恢复——JSON 支持文件或目录(目录内写 `export.json`);Markdown 支持目录或单个 `.md` 文件;导入幂等(已存在的 id 跳过、绝不覆盖) |
| `confirm-read` | 标记记忆已读(更新 access_count) |

---

## 集成

MemVault 为大多数 agent 提供了原生适配器——共享同一个记忆库,各 host 用 `MEMVAULT_AGENT_ID` 区分身份,分四个层级(T1/T2/T3 细节见下文;各客户端注册片段在 [integrations/mcp-clients/](integrations/mcp-clients/))。

**Tier 1 —— 一条命令装原生插件**(记忆由 hook 注入;host 有生命周期 hook 的,抽取默认关闭、按需开启):

| Agent | 安装 | 注入 | 抽取 |
|---|---|---|---|
| **Claude Code** | `/plugin marketplace add dreamor/memvault`,然后 `/plugin install memvault@memvault`(两条分开发送)——自带 MCP server、4 个 skills、3 个 slash commands | ✅ SessionStart hook | ✅ Stop hook,`MEMVAULT_HOOK_EXTRACT=1` 开启 |
| **OpenCode** | 把 [`integrations/opencode/opencode.json`](integrations/opencode/opencode.json) 合并进项目 | ✅ system transform | ✅ `session.idle` 时 |
| **DeepSeek Harness (dsh)** | 仓库自带 Cordis 插件 [`dsh-plugin/`](dsh-plugin/)——见 [docs/INSTALL.md §2.5](docs/INSTALL.md#25-deepseek-harness-dsh) | ✅ system prompt | ✅ 每轮结束 |
| **Gemini CLI / Antigravity** | `gemini extensions install https://github.com/dreamor/memvault` | ⚠️ 规则文件 + 工具 | ❌ |
| **Codex CLI** | [`integrations/codex/`](integrations/codex/):config.toml 注册 MCP + `memvault sync` + custom prompts | ⚠️ 规则 + 工具 | ❌ |

⚠️ = 该 host 没有注入型 hook;注入靠自带的 canonical 规则文本驱动(agent 开场调一次 `session_start`)。

**Tier 2 —— 粘贴一段 MCP 配置。** [integrations/mcp-clients/](integrations/mcp-clients/) 里是经过 CI 严格 JSON 校验的注册片段,各 host 用独立身份(合并目标路径见其 [README](integrations/mcp-clients/README.md)):Cursor · Windsurf · Cline/Roo · Continue · Zed · JetBrains AI/Junie · VS Code (Copilot Chat) · Claude Desktop。

**Tier 3 —— 原生清单,装后验证。** Qoder(`.qoder/rules/` + `.qoder-plugin/` + `UserPromptSubmit` hook 模板)、Grok Build(`grok plugin install dreamor/memvault --trust`)、Hermes Python 插件([integrations/hermes/](integrations/hermes/),`pre_llm_call` 注入 + 抽数助手)与 pi 扩展(`pi-extension/`,`pi install git:github.com/dreamor/memvault`)均已在仓库内;OpenClaw 与 Swival 消费生成的根级 `skills/`(同时导出 `.openclaw/skills/`);Devin 仍为手工配方,见 [integrations/README.md](integrations/README.md)。

**Tier 4 —— 纯规则文件兜底。** canonical 文本 + `scripts/gen-rule-copies.sh`(CI 里跑 parity 校验)生成 `AGENTS.md`/`CLAUDE.md` 块与 `.cursor/rules/`、`.clinerules/`、`.kiro/steering/`、Junie guidelines;`memvault sync --watch` 让它们持续跟随记忆库更新。

其它任何说 MCP 的客户端(不论国内国外、IDE 插件还是命令行 harness)都可以用下方标准 stdio 配置零改动接入——未逐一验证过,欢迎提 PR 补充已验证的条目。

GUI 面与 agent 安装相互独立:**Web Dashboard**(9 个标签页) · **Obsidian 插件**(α —— Vault 同步 + 查看/搜索/选区捕获) · **MCP Proxy**(把记忆透明注入任意上游服务器的响应)。

---

## 架构

```
┌────────────────────────────────────────────────┐
│  客户端(任意 MCP 兼容 Agent)                     │
│  ┌────────────┐ ┌────────┐ ┌─────┐ ┌────────┐  │
│  │ Claude Code│ │ Cursor │ │ dsh │ │ 其它   │  │
│  └────────────┘ └────────┘ └─────┘ └────────┘  │
└──────────────────┬───────────────────────────────┘
                   │ MCP (stdio / SSE / HTTP)
┌──────────────────▼───────────────────────────────┐
│  memvault-mcp     (rmcp 3.1.1)                    │
│  ┌──────────────┐ ┌────────────────┐ ┌────────┐  │
│  │  18 个工具    │ │  2 个资源      │ │ SSE    │  │
│  │   + REST API │ │  + 自动注入     │ │ Server │  │
│  └──────┬───────┘ └──────┬─────────┘ └────────┘  │
│         └────────┬───────┘                        │
│              ┌───▼────────┐                       │
│              │ Agent      │ (Agent 注册表         │
│              │ 路由        │  类型/标签过滤)       │
│              └───┬────────┘                       │
├──────────────────┼────────────────────────────────┤
│  memvault-core    │                               │
│  ┌──────────┐  ┌─▼───────┐ ┌────────────┐ ┌───┐ │
│  │ 存储      │  │ 检索     │ │ 流水线      │ │同步│ │
│  │ SQLite   │  │BM25+向量│ │抽取器       │ │   │ │
│  │          │  │RRF+同义 │ │去重/衰减    │ │   │ │
│  │ 嵌入      │  │词       │ │导出/        │ │   │ │
│  │ 回填      │  │打分     │ │导入         │ │   │ │
│  └──────────┘  └─────────┘ └────────────┘ └───┘ │
└────────────────────────────────────────────────┘
```

---

## 测试

```bash
cargo test                      # 约 970 个测试(全 workspace)
cargo clippy --all-targets      # 零告警
cargo fmt --all -- --check      # 格式检查
cargo llvm-cov --workspace --all-features   # CI 门禁:line ≥92% / region ≥90% / function ≥85%
```

---

## 文档

| 文档 | 内容 |
|------|------|
| [docs/DESIGN.md](docs/DESIGN.md) | 产品与架构设计 |
| [docs/INSTALL.md](docs/INSTALL.md) | 安装指南(全平台) |
| [docs/DOCKER.md](docs/DOCKER.md) | Docker 部署 |
| [docs/RUNBOOK.md](docs/RUNBOOK.md) | 部署 / 健康检查 / 回滚手册 |
| [docs/TROUBLESHOOTING.md](docs/TROUBLESHOOTING.md) | 症状 → 原因 → 解决 排查指南 |
| [docs/experiments/](docs/experiments/README.md) | 假设验证实验(H1–H7,2026-08-11 → 2026-08-27,全部 CONFIRMED)+ 运行时 plumbing 回归(2026-08-28) |
| [docs/RELEASING.md](docs/RELEASING.md) | 发布流程——CI 自动化范围(Linux/macOS 二进制、Docker 镜像、Dashboard 归档、Obsidian zip)vs. 需要手动完成的步骤(Obsidian 插件提交——无需 macOS 签名) |
| [docs/DISTRIBUTION.md](docs/DISTRIBUTION.md) | 分发渠道全景——自动化 vs. 手动渠道、所需凭据、MCP 注册表、可选渠道 |
| [docs/DISTRIBUTION-TODO.md](docs/DISTRIBUTION-TODO.md) | 分发待办清单——已就位 vs. 待办项、分阶段执行、所需 Secrets(仓库当前为 private) |
| [CHANGELOG.md](CHANGELOG.md) | 版本历史 |
| [CONTRIBUTING.md](CONTRIBUTING.md) | 贡献指南 |
| [SECURITY.md](SECURITY.md) | 安全公告 |
| [.env.example](.env.example) | 配置模板——所有配置项的唯一事实来源 |

---

## 贡献与社区

- 🐛 **Bug:** [提交 Issue](https://github.com/dreamor/memvault/issues/new)
- 💡 **想法:** [功能建议](https://github.com/dreamor/memvault/issues/new)
- 📖 **指南:** [CONTRIBUTING.md](CONTRIBUTING.md)
- 🔒 **安全:** [SECURITY.md](SECURITY.md)

---

## 许可证

MemVault 基于 [MIT 许可](LICENSE) 发布。
