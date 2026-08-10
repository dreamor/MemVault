# 贡献指南

感谢你对 MemVault 的关注！本文档说明如何参与本项目的开发。

## 开发流程

我们采用以 PR 为核心的协作模式：

1. **Fork** 本仓库并 clone 到本地
2. 从 `main` 拉取特性分支：`git switch -c feat/<short-desc>`
3. **先写测试**（TDD）：参见下文「开发约定」
4. 实现功能 / 修复 Bug
5. `cargo fmt` + `cargo clippy` + `cargo test` 全部通过
6. 推送分支并发起 PR

## 开发约定

### Rust 代码

- `cargo +stable fmt` 必须无 diff
- `cargo +stable clippy --all-targets --all-features -- -D warnings` 必须通过
- 测试覆盖：单元 + 集成（新增/修改模块 ≥80%）
- 错误处理：可恢复错误走 `anyhow`，领域错误走自定义 `thiserror`，**禁止 `unwrap()`**（除非在测试或不可达分支）
- 公开 API 变更需同步 `docs/DESIGN.md` 对应章节

### Commit Message

遵循 [Conventional Commits](https://www.conventionalcommits.org/zh-hans/)：

```
<type>(<scope>): <description>

[optional body]

[optional footer(s)]
```

常用 `type`：`feat` / `fix` / `refactor` / `docs` / `test` / `chore` / `perf` / `ci` / `build`

### 文档

- `docs/DESIGN.md` 是唯一权威设计文档；架构/接口变更需同步更新
- `docs/PLAN.md` 维护阶段路线图状态
- 新增 `docs/*.md` 需在 `README.md` 文档索引表中登记

## 提交 PR 前自检

- [ ] 通过 `cargo fmt + cargo clippy + cargo test`
- [ ] 在 `CHANGELOG.md` 的 `[Unreleased]` 区段添加条目
- [ ] 涉及破坏性变更时在「BREAKING CHANGE」footer 注明
- [ ] 在新领域写入前先开 Issue 讨论（降低返工风险）

## 行为准则

请阅读 [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md)，所有互动均受其约束。

## 联系方式

- Bug / 需求：[GitHub Issues](https://github.com/user/memvault/issues)
- 安全问题：参见 [SECURITY.md](SECURITY.md)（**勿**通过公开 Issue 报告）
- 设计与讨论：[GitHub Discussions](https://github.com/user/memvault/discussions)