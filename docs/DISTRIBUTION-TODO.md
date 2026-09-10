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
| crates.io | ⏳ 未发布（元数据已就绪，可直接执行） | 无（不依赖仓库可见性，建议完善后一并发布） | `CRATES_IO_TOKEN` |
| VS Code Marketplace | ⏳ 未发布 | CI 只打 `.vsix`，发布需手动 | Azure PAT |
| Open VSX | ⏳ 未发布（workflow 已就绪） | 无 | `OPEN_VSX_TOKEN` |
| Obsidian 社区插件 | ⏳ 未提交（BRAT 资产已自动生成） | 需手动 PR 到 `obsidianmd/obsidian-releases` | GitHub 账号 |
| npm（dsh 插件） | ⏳ 未发布（workflow 已就绪） | 无 | `NPM_TOKEN` |
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
- [ ] `cargo package -p <crate> --allow-dirty` 逐个通过（四 crate）（2026-09-10：core ✅；cli/mcp/proxy 因 `memvault-core` 不在 crates.io 结构性失败——Phase 2 按 core→顺序发布后即可通过，非打包配置问题）
- [ ] 本地 `cargo publish --dry-run` 四 crate（确认 readme/license/repository 元数据正确）（2026-09-10：core 完整通过含 verify 编译，exit 0；其余三个同上依赖阻塞）
- [ ] （可选）先发 crates.io 私有验证 `cargo install memvault-cli`（公开后代码托管不一定需要验证，此处仅验证发布链路）
- [x] 三个 workflow YAML 语法与 job 逻辑复查（2026-09-10：ci/publish/release 均解析通过，publish.yml secret 守卫 + workflow_dispatch 确认在位）

其余完善项（按需）：
- [ ] 依赖安全基线已启用：Dependabot **security updates**（仅 CVE 安全公告触发修复 PR，平常不消耗 CI 额度）+ vulnerability alerts（2026-08-28 已开启）；做依赖完善/升级时留意告警
- [x] 依赖许可证合规扫描（2026-09-07）：新增 `deny.toml`（`cargo-deny`），allow-list 覆盖依赖树里实际出现的全部许可证（MIT/Apache-2.0/BSD-2/3-Clause/0BSD/BSL-1.0/CC0-1.0/CDLA-Permissive-2.0/ISC/Unicode-3.0/Unlicense/Zlib/MPL-2.0），未发现 GPL/AGPL 族；`r-efi` 的 `MIT OR Apache-2.0 OR LGPL-2.1-or-later` 走 MIT 分支满足，LGPL 分支未被触发、也未加入 allow-list。CI 新增 `license-check` job（`EmbarkStudios/cargo-deny-action`），随 `changes.core` 触发。
- [x] `dashboard`/`obsidian-plugin`/`vscode-extension` 的 `package.json` 补齐 `license: "MIT"`（此前只有 `dsh-plugin` 有，2026-09-07）
- [x] 根 `Cargo.toml` `[workspace.package]` 补齐 `authors`/`keywords`/`categories`（四个 crate 均已 `.workspace = true` 继承，2026-09-07）

- [ ] README 安装链路最终核对（含 Windows PowerShell 路径分隔符）
- [x] 决定首个正式版本号：`v0.3.0`（`v0.2.0` 已占用 pre-release；workspace `Cargo.toml`、dashboard、vscode-extension、obsidian-plugin 已同步提升到 0.3.0，`CHANGELOG.md` 已切出对应 `[0.3.0]` 章节，2026-09-07）

### Phase 1 — 转 public（你确认时机后执行）

- [ ] `gh repo edit dreamor/memvault --visibility public`
- [ ] 验证匿名下载：HEAD 请求 release 资产应 200
- [ ] 端到端验证 `brew install memvault`（Apple Silicon）
- [ ] 端到端验证 `curl -fsSL .../scripts/install.sh | bash`（Linux / macOS）
- [ ] （Windows 机器）验证 `install.ps1`

### Phase 2 — 各渠道正式发布

- [ ] 推正式 tag `v0.3.0` 触发 `release.yml`，确认产物：4 平台归档 + 各 `.sha256` + `SHA256SUMS` + ghcr.io 镜像 + dashboard `dist` + `.vsix` + Obsidian 资产
- [ ] 处理 v0.2.0 pre-release：转正式或删除（若以新 tag 为准）
- [ ] crates.io：`cargo publish -p memvault-core` → `memvault-cli` / `memvault-mcp` / `memvault-proxy`（顺序依赖），或跑 **Publish (manual)** workflow
- [ ] VS Code Marketplace：`vsce publish`（Azure PAT）
- [ ] Open VSX：跑 **Publish (manual)** 或 `ovsx publish`
- [ ] Obsidian：确认 BRAT 可用后，提交 PR 到 [`obsidianmd/obsidian-releases`](https://github.com/obsidianmd/obsidian-releases)
- [ ] npm：跑 **Publish (manual)** 或 `npm publish --access public`（`@memvault/dsh-memvault`）
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

仓库 `Settings → Secrets and variables → Actions` 添加（2026-09-10 复核：三项 Actions secrets 均仍未配置）：

| Secret | 用途 | 来源 |
|--------|------|------|
| `CRATES_IO_TOKEN` | `cargo publish`（`publish.yml` job：crates-io） | crates.io 账号 → Account tokens |
| `OPEN_VSX_TOKEN` | Open VSX 发布（job：open-vsx） | open-vsx.org → Manage Access |
| `NPM_TOKEN` | npm 发布（job：npm-dsh） | npm 账号 → Access Tokens |
| Azure DevOps PAT | VS Code Marketplace（本地 `vsce publish` 用） | Azure DevOps → Personal Access Tokens |

> `publish.yml` 三个 job 均以「secret 存在才执行」保护，未配置前合入不会报错。
>
> **三个 token 是否必须（2026-09-10 按 npm / crates.io 官方文档核实）：**
>
> | Token | 是否必须 | 说明 |
> |-------|:---:|------|
> | `NPM_TOKEN` | ❌ 可用 trusted publishing 替代 | npm OIDC（`permissions: id-token: write`）。前提：workflow 的 Node 需升到 24（Node 22 自带 npm 10.x 不支持，需 npm ≥ 11.5.1）；trusted publisher 在 **package settings** 配置，故 `@memvault/dsh-memvault` 首版仍需 token 或本地 `npm publish`（走 2FA），之后配置 repo=dreamor/memvault + workflow=publish.yml 即可删 token。`dsh-plugin/package.json` 的 `repository.url` 已精确匹配，无其他阻力 |
> | `CRATES_IO_TOKEN` | ⚠️ 首发必须，后续可替代 | crates.io 官方文档明确「initial publish requires an API token」且 trusted publishing 逐 crate 配置。首发四 crate 需 token 或本地 `cargo login` + 手动 publish；之后逐 crate 在 Settings → Trusted Publishing 配置，workflow 换 `rust-lang/crates-io-auth-action@v1`（30 分钟短时 token、job 结束自动吊销），删 token |
> | `OPEN_VSX_TOKEN` | ✅ 必须（若走 CI） | Open VSX 无 OIDC 机制，`ovsx` CLI 只认站点 PAT。发布频率低，可改为本地 `npx ovsx publish -p <token>` 手动发（token 不进 repo），则此 secret 也可省 |
>
> **省事路径（三个 secret 都不配）**：首发在本地完成（cargo login / npm publish 走 2FA / ovsx publish），后续 crates.io 与 npm 切 trusted publishing；Open VSX 保持本地发布。
> **全自动化路径**：仅配 `OPEN_VSX_TOKEN`；crates.io 与 npm 走 trusted publishing。
>
> 生成入口（需要时）：crates.io → Account Settings → API Tokens（先验证邮箱，首发需 `publish-new` scope）；open-vsx.org → Settings → Access Tokens；npm → Access Tokens（Granular 优先，Classic 选 Automation 避免 CI 卡 2FA）。
>
> 关于泄露的边界：文档中出现 secret 的**名字**、入口 URL、消费方式均无风险——GitHub secrets 为加密存储、日志自动掩码 `***`、fork PR 与 Dependabot PR 默认不可见；唯一不可入库的是 token **明文值**（gitleaks 已作兜底扫描）。

## 四、已知约束

- **Intel macOS（x86_64）无预编译**：`fastembed` 内嵌 ONNX Runtime 无 `x86_64-apple-darwin` 产物，
  CI 无法构建该平台；Intel 用户走源码构建（`install.sh` 与 Homebrew formula 均显式提示）。
- **Windows ARM64 未构建**：release 矩阵仅 `x86_64-pc-windows-msvc`，需按需扩展。
- **private 期间分发受限**：install.sh / brew / 匿名下载 404 属预期，非缺陷。
- **未签名二进制**：GitHub Releases 资产无代码签名 / notarization，Gatekeeper 首次打开需右键确认（与常见 OSS CLI 一致）。
