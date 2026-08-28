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
- [ ] 全仓 secret 扫描（gitleaks / trufflehog），确认无明文 token、密钥、内网地址
- [ ] 核对 `.env.example`：全部为占位值，无真实配置
- [ ] 核对 `.gitignore` / `.gitattributes`：`target/`、`.venv/`、`node_modules/` 不入库
- [ ] CI 全绿：`cargo fmt` / `cargo clippy -D warnings` / `cargo test` / dashboard vitest / 插件构建与测试
- [ ] Docker 本地构建验证：`docker build -t memvault:local .` 可过

发布就绪验证（不依赖公开，可在 private 下完成）：
- [ ] `cargo package -p <crate> --allow-dirty` 逐个通过（四 crate）
- [ ] 本地 `cargo publish --dry-run` 四 crate（确认 readme/license/repository 元数据正确）
- [ ] （可选）先发 crates.io 私有验证 `cargo install memvault-cli`（公开后代码托管不一定需要验证，此处仅验证发布链路）
- [ ] 三个 workflow YAML 语法与 job 逻辑复查

其余完善项（按需）：
- [ ] 依赖安全基线已启用：Dependabot **security updates**（仅 CVE 安全公告触发修复 PR，平常不消耗 CI 额度）+ vulnerability alerts（2026-08-28 已开启）；做依赖完善/升级时留意告警

- [ ] README 安装链路最终核对（含 Windows PowerShell 路径分隔符）
- [ ] 决定首个正式版本号（`v0.2.0` 已占用 pre-release，正式发布建议 `v0.2.1` 或重建 tag）

### Phase 1 — 转 public（你确认时机后执行）

- [ ] `gh repo edit dreamor/memvault --visibility public`
- [ ] 验证匿名下载：HEAD 请求 release 资产应 200
- [ ] 端到端验证 `brew install memvault`（Apple Silicon）
- [ ] 端到端验证 `curl -fsSL .../scripts/install.sh | bash`（Linux / macOS）
- [ ] （Windows 机器）验证 `install.ps1`

### Phase 2 — 各渠道正式发布

- [ ] 推正式 tag（如 `v0.2.1`）触发 `release.yml`，确认产物：4 平台归档 + 各 `.sha256` + `SHA256SUMS` + ghcr.io 镜像 + dashboard `dist` + `.vsix` + Obsidian 资产
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
- [ ] `CHANGELOG.md` 补正式版条目（当前 0.2.0 为 pre-release）
- [ ] 建立反馈渠道（Issues / Discussions）并写入 SECURITY.md / CONTRIBUTING.md
- [ ] **恢复全量 Dependabot 版本更新**（当前为「仅安全更新」模式）：把 `.github/dependabot.yml` 加回仓库（完整配置在 git 历史 `5526e3d^:.github/dependabot.yml`），公开/生产后开启，避免漏掉非安全但重要的依赖升级（如 Rust minor 修复、工具链演进）

- [ ] 监控：crates.io 下载量、GitHub Release 下载量、Docker 拉取量

## 三、所需 Secrets 配置清单

仓库 `Settings → Secrets and variables → Actions` 添加（当前均未配置）：

| Secret | 用途 | 来源 |
|--------|------|------|
| `CRATES_IO_TOKEN` | `cargo publish`（`publish.yml` job：crates-io） | crates.io 账号 → Account tokens |
| `OPEN_VSX_TOKEN` | Open VSX 发布（job：open-vsx） | open-vsx.org → Manage Access |
| `NPM_TOKEN` | npm 发布（job：npm-dsh） | npm 账号 → Access Tokens |
| Azure DevOps PAT | VS Code Marketplace（本地 `vsce publish` 用） | Azure DevOps → Personal Access Tokens |

> `publish.yml` 三个 job 均以「secret 存在才执行」保护，未配置前合入不会报错。

## 四、已知约束

- **Intel macOS（x86_64）无预编译**：`fastembed` 内嵌 ONNX Runtime 无 `x86_64-apple-darwin` 产物，
  CI 无法构建该平台；Intel 用户走源码构建（`install.sh` 与 Homebrew formula 均显式提示）。
- **Windows ARM64 未构建**：release 矩阵仅 `x86_64-pc-windows-msvc`，需按需扩展。
- **private 期间分发受限**：install.sh / brew / 匿名下载 404 属预期，非缺陷。
- **未签名二进制**：GitHub Releases 资产无代码签名 / notarization，Gatekeeper 首次打开需右键确认（与常见 OSS CLI 一致）。
