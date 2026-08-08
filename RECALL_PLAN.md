# 提升记忆召回率实施方案

## Context

当前 MemVault 的记忆召回存在多个瓶颈，导致 Agent 在 session_start 和 search_memory 时无法找到相关记忆。核心问题：关键词搜索是全字符串子串匹配（LIKE %query%），只搜 content 字段，不搜 tags/instruction；意图过滤是二元排除（有 writing tag 就完全排除），无软评分；无查询扩展。

## 实施内容（7 项改进）

### 1. 词级分词搜索（替换全字符串 LIKE）

**文件**: `crates/memvault-core/src/storage/sqlite.rs` — `search` 方法

当前: `content LIKE '%Python API%'`（整个 query 作为子串匹配）

改为: 将 query 拆分为单词，每个词独立 LIKE 匹配，用 OR 连接，命中多个词的结果评分更高。

### 2. 多字段搜索（content + instruction + tags）

**文件**: `crates/memvault-core/src/storage/sqlite.rs` — `search` 方法

搜索范围从只搜 `content` 扩展到同时搜 `content`、`instruction`、`tags` 三个字段。tag 命中视为精准匹配，给予额外加分。

### 3. 查询扩展（同义词 + 相关词）

**新文件**: `crates/memvault-core/src/query_expand.rs`

维护一个静态同义词表，对 query 中的每个词查找扩展加入搜索。

### 4. 相关性评分公式

**文件**: `crates/memvault-core/src/storage/sqlite.rs`

新评分:
```
score = match_score × 0.4 + recency_score × 0.2 + priority_score × 0.2 + access_score × 0.2
```

### 5. 软意图过滤（评分降权替代二元排除）

**文件**: `crates/memvault-core/src/router.rs`

不相关 tag 的记忆 score 降为 50%（仍有机会出现），而非完全排除。

### 6. 跨命名空间回退

**文件**: `crates/memvault-core/src/router.rs`

先搜 project namespace，不足时追加 global namespace 补充。MUST 始终跨 namespace。

### 7. 缺失 Embedding 自动回填

在 session_start 时，缺少 embedding 的记忆异步生成并写入。

## 修改文件清单

- `crates/memvault-core/src/storage/sqlite.rs` — 重写 search：词级 + 多字段 + 新评分
- `crates/memvault-core/src/router.rs` — 软过滤 + 跨 namespace + embedding 回填
- `crates/memvault-core/src/query_expand.rs` — 新文件：同义词扩展
- `crates/memvault-core/src/intent.rs` — should_exclude 改为 relevance_score
- `crates/memvault-core/src/lib.rs` — 导出新模块

## 验证

1. `cargo test` 全部通过（调整受影响的断言）
2. 新增召回率测试用例验证改进效果
3. CLI 端到端验证
