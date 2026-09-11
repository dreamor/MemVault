# MemVault 分发任务待办清单

> 状态基准：2026-09-11。仓库已 **public**（2026-09-11 转 public，匿名分发链路当日全部实测通过）。
> 本文档记录所有分发渠道的落地状态、待办项、所需权限与执行顺序。
> 渠道全景与设计见 [`DISTRIBUTION.md`](DISTRIBUTION.md)，发布操作细节见 [`RELEASING.md`](RELEASING.md)。

## 一、总览

| 渠道 | 状态 | 阻塞因素 | 所需凭据 |
|------|------|----------|----------|
| GitHub Releases 资产 | ✅ 已发布（v0.3.0 Latest：14 资产实测齐全——4 平台归档 + 4 个 `.sha256` + `SHA256SUMS` + dashboard + Obsidian 顶层三资产；v0.2.0 pre-release 及 tag 已删；2026-09-11 匿名下载实测 302→206 正常） | — | — |
| Homebrew tap | ✅ 端到端通过（v0.3.0 formula 已推 tap commit `085d3f6`；`brew install dreamor/tap/memvault` + `brew test` 全绿，Apple Silicon 实测 `memvault 0.3.0`。顺带修复 formula 模板非法 DSL `only_arm64` → `depends_on arch: :arm64`——旧 formula 一经 brew 解析即崩，从未可装） | — | — |
| CLI 一键安装脚本 | ✅ 实测通过（`install.sh` 端到端：latest 下载 + SHA256SUMS 校验 + `~/.memvault/bin` 落位，`memvault-cli --version` = 0.3.0；`install.ps1` 静态核对通过——Join-Path/反斜杠/TLS12/校验逻辑规范，zip 目录结构与 `tar -a` 产物吻合；真机 Windows 验证待有机器补） | — | — |
| crates.io | ✅ 已发布（2026-09-11：四 crate v0.3.0 全部上架，core→cli→mcp→proxy 顺序本地发布，token 只经本机 credentials.toml） | — | — |
| VS Code 扩展 | ❌ 已移除（2026-09-10，管理功能收敛到 Dashboard / Obsidian 插件） | — | — |
| Obsidian 社区插件 | ✅ **已上架**（2026-09-11 确认通过门户复审；本轮整改与发布历史如下——**2026-09-11：材料全就位；已过门户新流程整改——首轮自动审核仅报 description 不得含 "Obsidian"（manifest.json:6），0.3.1 修 description 后按复审建议升级 `0.3.2`：release 资产补构建溯源 attestation（attest-build-provenance@v4.2.2，注意需 `attestations: write` + `id-token: write` 双权限）并从 release 移除 versions.json（目录只消费 main.js/manifest.json/styles.css 三件，仓库文件保留），monorepo 副本同步 0.3.2；第二轮全量审核（Error×5）整改为 0.3.3：minAppVersion 提到 1.7.2（revealLeaf 1.7.2/createFolder·fileManager 1.4，本地用 obsidianmd 官方 eslint 套件复现并清零全部 Error）、Settings 页 heading 规范化、孤儿笔记删除改 FileManager.trashFile、any/unsafe 全类型化、浮动 Promise 全治理、原生 confirm 换 Modal、测试同步 trashFile 断言；monorepo 副本同步 0.3.3，待门户复审**——官方门户 community.obsidian.md 要求 manifest 在仓库根目录 → 提交仓库改用独立仓库 `dreamor/memvault-obsidian`（public，自包含：根目录 README/LICENSE/manifest.json/versions.json + 自带 tag 触发的 release CI）。release `0.3.0` 已发布并实查验证：顶层 `main.js`/`manifest.json`/`styles.css`/`versions.json`，tag=manifest version，独立树 typecheck+build+31 测试全绿；`obsidian-releases` 的 PR 流程已官方废弃，fork 侧旧条目作废可删） | 只剩门户人工步骤：community.obsidian.md 登录（Obsidian 账号）→ 绑定 GitHub 账号 → Add plugin → 看自动审核反馈 | GitHub 账号 + Obsidian 账号绑定 |
| npm（dsh 插件） | ✅ 已发布（2026-09-11：`@dreamor/dsh-memvault@0.3.0` 本地 `npm publish` 上架，浏览器 2FA。曾用 `@memvault/` scope——该组织名已被他人占用、无发布权，registry 一律拒 404；改为 npm 用户名 scope `@dreamor/` 后发布成功。npmjs.com 元数据 CDN 对新包有分钟级延迟，发布后短暂 404 属正常） | — | — |
| Docker Hub 镜像 | ✅ 端到端验证通过（2026-09-11：secrets 已配，rebuild-docker 双推，`docker.io/dreamor/memvault:0.3.0` 匿名 manifest 200；release.yml 同样双推） | — | `DOCKERHUB_USERNAME`/`DOCKERHUB_TOKEN`（已配） |
| MCP 生态注册表 | ✅ 官方 registry 已收录（`io.github.dreamor/memvault` v0.3.0，status=active）；Glama/mcp.so 已自动同步实测命中（2026-09-11，官方 registry 爬源生效）；PulseMCP **已停止收录入口**（2026-09-11 确认，从清单移除）；Smithery **不适用**（现行模型仅收 Streamable HTTP 公网 URL / MCPB，与本地优先·数据不出机的 stdio 自托管形态冲突，早先草拟的 smithery.yaml 已删）