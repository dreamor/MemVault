# MemVault 分发任务待办清单

> 状态基准：2026-08-28。仓库当前为 **private**（有意的，完善后再转 public）。
> 本文档记录所有分发渠道的落地状态、待办项、所需权限与执行顺序。
> 渠道全景与设计见 [`DISTRIBUTION.md`](DISTRIBUTION.md)，发布操作细节见 [`RELEASING.md`](RELEASING.md)。

## 一、总览

| 渠道 | 状态 | 阻塞因素 | 所需凭据 |
|------|------|----------|----------|
| GitHub Releases 资产 | ✅ 已就位（v0.2.0 pre-release：macOS ARM64 归档 + SHA256SUMS） | 仓库 private → 匿名下载 404（预期） | — |
| Homebrew tap | ✅ 已推送（`dreamor/homebrew-tap` public + `Formula/memvault.rb`，本地解析验证通过） | formula 下载 URL 指向私有仓库资产，公开前 `brew install` 会 404 | — |
| CLI 一键安装脚本 | ✅ 已落地（`scripts/install.sh` / `install.ps1`，含 SHA-256 校验） | 同上（下载依赖 release 资产可匿名访问） | — |
| crates.io | ✅ 已发布（2026-09-11：四 crate v0.3.0 全部上架，core→cli→mcp→proxy 顺序本地发布，token 只经本机 credentials.toml） | — | — |
| VS Code 扩展 | ❌ 已移除（2026-09-10，管理功能收敛到 Dashboard / Obsidian 插件） | — | — |
| Obsidian 社区插件 | ⏳ 未提交（**2026-09-11 复核：提交流程彻底改版**——`obsidianmd/obsidian-releases` 的 PR 入口已被官方关闭，改走 https://community.obsidian.md 门户（登录→绑 GitHub→Add plugin）。新门户硬性要求：① README/LICENSE/**manifest.json 位于仓库根目录**（monorepo 子目录不合规 → 当前的 `obsidian-plugin/` 子目录结构需先解决）；② release 资产为顶层 `main.js`/`manifest.json`/`styles.css`（无目录前缀，当前 `release-assets/` 前缀不合规）；③ tag = manifest version；④ 缺 `versions.json` 建议补，manifest author 对上 GitHub 身份 | 硬前提：仓库 public + 插件仓库结构合规 | GitHub 账号 + Obsidian 账号绑定 |
| npm（dsh 插件） | ✅ 已发布（2026-09-11：`@dreamor/dsh-memvault@0.3.0` 本地 `npm publish` 上架，浏览器 2FA。曾用 `@memvault/` scope——该组织名已被他人占用、无发布权，registry 一律拒 404；改为 npm 用户名 scope `@dreamor/` 后发布成功。npmjs.com 元数据 CDN 对新包有分钟级延迟，发布后短暂 404 属正常） | — | — |
| Docker Hub 镜像 | ⏳ 未配置（当前仅 ghcr.io） | 需在 release.yml 加推送 job | Docker Hub token |
| MCP 生态注册表 | ⏳ 未提交 | 需逐站注册 | 各站账号 |

**关键依赖链**：`仓库转 public` → 匿名下载生效 → install.sh / brew / 各平台产物真正可用。
转 public 是全部待办的根前提，由你决定时机。

## 二、待办清单（按阶段）

### Phase 0 — 转 public 前的完善（当前阶段）

安全与质量：
- [x] 全仓 secret 扫描（gitleaks，203 commits 全历史扫描，2026-09-07）：no leaks found；`git status` 亦确认无游离敏感文件
- [x] 核对 `.env.example`：全部为占位值，无真实配置（2026-09-10 复核）
- [ ] 核对 `.gitignore` / `.gitattributes`：`target/`、`.venv/`、`node_modules/` 不入库
- [x] CI 全绿：`cargo fmt` / `cargo clippy -D warnings` / `cargo test` / dashboard vitest / 插件构建与测试（2026-09-10 workflow_dispatch run `34443530442` 对 HEAD `eb224fb` 全绿，11/11 job 成功，含 Docker smoke / license-check / coverage / REST smoke）
- [ ] Docker 本地构建验证：`docker build -t memvault:local .` 可过

发布就绪验证（不依赖公开，可在 private 下完成）：
- [x] `cargo package -p <crate> --allow-dirty` 逐个通过（四 crate）（2026-09-11 发布时全部通过：core 59 files；cli/mcp/proxy 依赖已随 core 上架自然解开）
- [x] 本地 `cargo publish --dry-run` 四 crate（2026-09-11 随正式发布全链路验证，readme/license/repository 无警告）
- [x] （可选）crates.io 发布链路验证（2026-09-11 实际完成：`cargo publish` 四连发成功，crates.io API 已可查 0.3.0）
- [x] 三个 workflow YAML 语法与 job 逻辑复查（2026-09-10：ci/publish/release 均解析通过，publish.yml secret 守卫 + workflow_dispatch 确认在位）

其余完善项（按需）：
- [ ] 依赖安全基线已启用：Dependabot **security updates**（仅 CVE 安全公告触发修复 PR，平常不消耗 CI 额度）+ vulnerability alerts（2026-08-28 已开启）；做依赖完善/升级时留意告警
- [x] 依赖许可证合规扫描（2026-09-07）：新增 `deny.toml`（`cargo-deny`），allow-list 覆盖依赖树里实际出现的全部许可证（MIT/Apache-2.0/BSD-2/3-Clause/0BSD/BSL-1.0/CC0-1.0/CDLA-Permissive-2.0/ISC/Unicode-3.0/Unlicense/Zlib/MPL-2.0），未发现 GPL/AGPL 族；`r-efi` 的 `MIT OR Apache-2.0 OR LGPL-2.1-or-later` 走 MIT 分支满足，LGPL 分支未被触发、也未加入 allow-list。CI 新增 `license-check` job（`EmbarkStudios/cargo-deny-action`），随 `changes.core` 触发。
- [x] `dashboard`/`obsidian-plugin`/`vscode-extension` 的 `package.json` 补齐 `license: "MIT"`（此前只有 `dsh-plugin` 有，2026-09-07）
- [x] 根 `Cargo.toml` `[workspace.package]` 补齐 `authors`/`keywords`/`categories`（四个 crate 均已 `.workspace = true` 继承，2026-09-07）

- [ ] README 安装链路最终核对（含 Windows PowerShell 路径分隔符）
- [x] GUI 面层收敛(2026-09-10):删除 `vscode-extension/`(VS Code 扩展),Obsidian 插件收敛为「Vault 同步 + 查看/搜索/选区捕获」,管理类功能交由 Web Dashboard / CLI;CI 与文档引用同步清理
- [x] 决定首个正式版本号：`v0.3.0`（`v0.2.0` 已占用 pre-release；workspace `Cargo.toml`、dashboard、vscode-extension、obsidian-plugin 已同步提升到 0.3.0，`CHANGELOG.md` 已切出对应 `[0.3.0]` 章节，2026-09-07）

### Phase 1 — 转 public（你确认时机后执行）

- [ ] `gh repo edit dreamor/memvault --visibility public`
- [ ] 验证匿名下载：HEAD 请求 release 资产应 200
- [ ] 端到端验证 `brew install memvault`（Apple Silicon）
- [ ] 端到端验证 `curl -fsSL .../scripts/install.sh | bash`（Linux / macOS）
- [ ] （Windows 机器）验证 `install.ps1`

### Phase 2 — 各渠道正式发布

- [ ] 推正式 tag `v0.3.0` 触发 `release.yml`，确认产物：4 平台归档 + 各 `.sha256` + `SHA256SUMS` + ghcr.io 镜像 + dashboard `dist` + Obsidian 资产
- [ ] 处理 v0.2.0 pre-release：转正式或删除（若以新 tag 为准）
- [x] crates.io：首版已完成（2026-09-11，四 crate 顺序本地发布）；后续版本跑 **Publish (manual)** 或配 trusted publishing
- [ ] Obsidian：走 https://community.obsidian.md 门户提交（obsidian-releases 的 PR 流程已官方废弃，`community-plugins.json` 老 PR 法勿用）。前置：仓库结构合规（根目录 README/LICENSE/manifest.json + 顶层三资产，当前 `obsidian-plugin/` 子目录结构需独立仓库或迁移）+ BRAT 验证
- [x] npm：首版已完成（2026-09-11，`@dreamor/dsh-memvault@0.3.0` 本地 `npm publish` 走浏览器 2FA）；后续版本再跑 **Publish (manual)**——注意同版本重复 publish 会报 409，0.3.0 不要再手动触发
- [ ] Homebrew：`./scripts/update-homebrew-formula.sh <正式tag>` 重新生成 formula（SHA-256 会变），推送到 `dreamor/homebrew-tap`
- [ ] Docker Hub（可选）：release.yml 加 `docker/login-action` + 镜像推送 `docker.io/dreamor/memvault`
- [ ] MCP 注册表（可选）：官方 registry（`modelcontextprotocol/registry` PR）、smithery.ai、mcp.so、Glama、PulseMCP

### Phase 3 — 发布后收尾

- [ ] README 顶部徽章：替换/新增 crates.io 版本徽章、GitHub Release 最新版徽章
- [ ] 文档同步：更新 `DISTRIBUTION.md` 渠道矩阵状态、`RELEASING.md` 手动步骤勾选
- [x] `CHANGELOG.md` 补正式版条目：已切出 `[0.3.0] — 2026-09-07` 章节（原 `[Unreleased]` 内容归档，上方保留一个新的空 `[Unreleased]`）
- [ ] 建立反馈渠道（Issues / Discussions）并写入 SECURITY.md / CONTRIBUTING.md（2026-09-10 核实：两文件内容已成体系、README 已链 Discussions；但 repo 侧 Discussions 功能尚未开启，需在 Settings → General → Features 中勾选）
- [ ] **恢复全量 Dependabot 版本更新**（当前为「仅安全更新」模式）：把 `.github/dependabot.yml` 加回仓库（完整配置在 git 历史 `5526e3d^:.github/dependabot.yml`），公开/生产后开启，避免漏掉非安全但重要的依赖升级（如 Rust minor 修复、工具链演进）

- [ ] 监控：crates.io 下载量、GitHub Release 下载量、Docker 拉取量

## 三、所需 Secrets 配置清单

仓库 `Settings → Secrets and variables → Actions`（2026-09-11 复核：**两项均无需配置**——crates.io 首发已完成，CI 侧走各自的 trusted publishing）：

| Secret | 用途 | 来源 |
|--------|------|------|
| `CRATES_IO_TOKEN` | ✅ 无需配置（`publish.yml` 的 crates-io job 已用 `rust-lang/crates-io-auth-action@v1` OIDC 换取短时 token；只需到 crates.io 网页给四个 crate 逐个配 Trusted Publishing） | — |
| `NPM_TOKEN` | ✅ 无需配置（trusted publishing 已于 2026-09-11 落地，见下方说明） | — |

> `publish.yml`（Publish (manual)）两个 job 不依赖任何 secret：crates.io 与 npm 均走 OIDC trusted publishing。官网侧未配置对应 crate/package 前，CI 发布会失败；本地首发不受影响（已完成）。
>
> **三个 token 是否必须（2026-09-10 按 npm / crates.io 官方文档核实）：**
>
> | Token | 是否必须 | 说明 |
> |-------|:---:|------|
> | `NPM_TOKEN` | ✅ 无需配置 | npm 侧 trusted publishing 已于 2026-09-11 配置完成（`@dreamor/dsh-memvault` → Publishing access：repo=dreamor/memvault、workflow=publish.yml）。仓库侧 `publish.yml` 的 npm-dsh job 从一开始就按 OIDC 就绪（`id-token: write` + Node 24 + 官方 registry-url），下次 CI 发布即生效，永久免 token。crates.io 侧 trusted publishing 仍未配置，`CRATES_IO_TOKEN` 是否需要见下行 |
> | `CRATES_IO_TOKEN` | ⚠️ 首发必须，后续可替代 | crates.io 官方文档明确「initial publish requires an API token」且 trusted publishing 逐 crate 配置。首发四 crate 需 token 或本地 `cargo login` + 手动 publish；之后逐 crate 在 Settings → Trusted Publishing 配置，workflow 换 `rust-lang/crates-io-auth-action@v1`（30 分钟短时 token、job 结束自动吊销），删 token |
> >
> **省事路径（两个 secret 都不配）**：首发在本地完成（cargo login / npm publish 走 2FA），后续 crates.io 与 npm 切 trusted publishing。
> **全自动化路径**：crates.io 与 npm 全部走 trusted publishing，无需任何 token secret。
>
> 生成入口（需要时）：crates.io → Account Settings → API Tokens（先验证邮箱，首发需 `publish-new` scope）；npm → Access Tokens（Granular 优先，Classic 选 Automation 避免 CI 卡 2FA）。（Open VSX 渠道已于 2026-09-11 整体移除，对应 job 与 token 均不再需要）
>
> 关于泄露的边界：文档中出现 secret 的**名字**、入口 URL、消费方式均无风险——GitHub secrets 为加密存储、日志自动掩码 `***`、fork PR 与 Dependabot PR 默认不可见；唯一不可入库的是 token **明文值**（gitleaks 已作兜底扫描）。

## 四、已知约束

- **Intel macOS（x86_64）无预编译**：`fastembed` 内嵌 ONNX Runtime 无 `x86_64-apple-darwin` 产物，
  CI 无法构建该平台；Intel 用户走源码构建（`install.sh` 与 Homebrew formula 均显式提示）。
- **Windows ARM64 未构建**：release 矩阵仅 `x86_64-pc-windows-msvc`，需按需扩展。
- **private 期间分发受限**：install.sh / brew / 匿名下载 404 属预期，非缺陷。
- **未签名二进制**：GitHub Releases 资产无代码签名 / notarization，Gatekeeper 首次打开需右键确认（与常见 OSS CLI 一致）。
