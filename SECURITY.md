# 安全策略

MemVault 重视用户数据的安全。本地优先（Local-First）的设计意味着敏感信息默认留在本机，但仍可能因使用不当或工具链漏洞产生风险。

## 支持的版本

| 版本 | 支持状态 |
|------|---------|
| `main` 分支 | ✅ 已修复 |
| 最新 3 个 release tag | ✅ 已修复 |
| 更早版本 | ❌ 不提供补丁 |

## 报告漏洞

**请勿**通过公开 Issue、Discussion 或 Pull Request 报告安全漏洞。

请通过以下任一私密渠道提交：

1. **GitHub Security Advisories**（推荐）：访问仓库 Security 标签页的 "Report a vulnerability"
2. **邮件**：发送到 `security@memvault.dev`（PGP key 见 `docs/SECURITY_PGP.asc` 暂缺）

报告内容请包含：

- 漏洞描述与影响面
- 复现步骤 / PoC
- 受影响版本
- 你的名字 / 联系方式（可选，用于致谢）

我们承诺在收到报告后 **48 小时内** 确认，并在 **7 天内** 给出修复时间线。

## 已知安全考虑

- **本地数据**：`~/.memvault/data.db` 存储全部记忆条目，默认权限 `0600`
- **Embedding 调用**：`OPENAI_API_KEY` 触发外发请求（语义搜索功能），关闭该环境变量即退化为纯关键词
- **MCP Stdio**：CLI/MCP Server 之间明文传输，仅适合本地进程通信，勿在公开网络上转发
- **指令注入**：Agent 收到的记忆以 MUST/REF 指令形式呈现，记忆来源（用户/其他 Agent）务必通过 `source` 字段审计

## 致谢

负责任披露漏洞的研究者将在 `CHANGELOG.md` 与本文件致谢（须本人同意）。