## 变更说明

<!-- 简述本次 PR 解决了什么问题、改动了什么、为什么这么改 -->

## 关联 Issue

<!-- 关联的 Issue 编号,例如 Closes #123 / Fixes #456 -->

## 变更类型

- [ ] 新功能 (feature)
- [ ] Bug 修复 (fix)
- [ ] 重构 (refactor)
- [ ] 文档 (docs)
- [ ] 测试 (test)
- [ ] 性能 (perf)
- [ ] 构建 / CI (ci/build)
- [ ] 其他

## 自检清单

- [ ] `cargo fmt --all` 已执行(无 diff)
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` 通过
- [ ] `cargo test` 通过,新增/修改的代码有单元测试覆盖
- [ ] README / docs/ 中相关章节已同步更新
- [ ] 公共 API 变更在 CHANGELOG.md 中记录
- [ ] 无新增 `unwrap()` / panic,可恢复错误走 `anyhow` / `thiserror`
- [ ] 无硬编码密钥 / 路径 / 调试输出

## 测试说明

<!-- 描述本次变更的验证步骤、边界用例、性能影响(如涉及) -->

## 截图 / 录屏(可选)

<!-- 涉及 UI / Dashboard 时附图 -->

## Checklist 复核者

<!-- Reviewer 关注点:架构 / 安全 / 性能 / API 设计 / 测试覆盖 / 文档完整性 -->