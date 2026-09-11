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
| Obsidian 社区插件 | ⏳ 仅剩门户提交（**2026-09-11：材料全就位；已过门户新流程整改——首轮自动审核仅报 description 不得含 "Obsidian"（manifest.json:6），0.3.1 修 description 后按复审建议升级 `0.3.2`：release 资产补构建溯源 attestation（attest-build-provenance@v4.2.2，注意需 `attestations: write` + `id-token: write` 双权限）并从 release 移除 versions.json（目录只消费 main.js/manifest.json/styles.css 三件，仓库文件保留），monorepo 副本同步 0.3.2；第二轮全量审核（Error×5）整改为 0.3.3：minAppVersion 提到 1.7.2（revealLeaf 1.7.2/createFolder·fileManager 1.4，本地用 obsidianmd 官方 eslint 套件复现并清零全部 Error）、Settings 页 heading 规范化、孤儿笔记删除改 FileManager.trashFile、any/unsafe 全类型化、浮动 Promise 全治理、原生 confirm 换 Modal、测试同步 trashFile 断言；monorepo 副本同步 0.3.3，待门户复审**——官方门户 community.obsidian.md 要求 manifest 在仓库根目录 → 提交仓库改用独立仓库 `dreamor/memvault-obsidian`（public，自包含：根目录 README/LICENSE/manifest.json/versions.json + 自带 tag 触发的 release CI）。release `0.3.0` 已发布并实查验证：顶层 `main.js`/`manifest.json`/`styles.css`/`versions.json`，tag=manifest version，独立树 typecheck+build+31 测试全绿；`obsidian-releases` 的 PR 流程已官方废弃，fork 侧旧条目作废可删） | 只剩门户人工步骤：community.obsidian.md 登录（Obsidian 账号）→ 绑定 GitHub 账号 → Add plugin → 看自动审核反馈 | GitHub 账号 + Obsidian 账号绑定 |
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
- [x] 核对 `.gitignore` / `.gitattributes`：`target/`、`.venv/`、`node_modules/` 不入库（2026-09-11 复核：规则齐备，`git ls-files` 零误跟踪；`.gitattributes` 行尾规范化在位）
- [x] CI 全绿：`cargo fmt` / `cargo clippy -D warnings` / `cargo test` / dashboard vitest / 插件构建与测试（2026-09-10 workflow_dispatch run `34443530442` 对 HEAD `eb224fb` 全绿，11/11 job 成功，含 Docker smoke / license-check / coverage / REST smoke）
- [x] Docker 验证（2026-09-11）：本机无 docker CLI，以用户视角替代验证——ghcr.io/dreamor/memvault:latest 匿名 token + manifest 拉取实测 200（多平台 OCI index）；CI docker smoke 全绿（run 34443530442）兜底本地构建

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

- [x] README 安装链路最终核对（2026-09-11）：install.sh/brew/crates.io 三渠道命令与实际产物比对通过；**顺手补上 README 缺失的 Homebrew 渠道行、修正 "once published to crates.io" 过时措辞**（README.md + README.zh-CN.md）
- [x] GUI 面层收敛(2026-09-10):删除 `vscode-extension/`(VS Code 扩展),Obsidian 插件收敛为「Vault 同步 + 查看/搜索/选区捕获」,管理类功能交由 Web Dashboard / CLI;CI 与文档引用同步清理
- [x] 决定首个正式版本号：`v0.3.0`（`v0.2.0` 已占用 pre-release；workspace `Cargo.toml`、dashboard、vscode-extension、obsidian-plugin 已同步提升到 0.3.0，`CHANGELOG.md` 已切出对应 `[0.3.0]` 章节，2026-09-07）

### Phase 1 — 转 public（你确认时机后执行）

- [x] 转 public（2026-09-11 生效，`gh repo view` 确认 PUBLIC）
- [x] 验证匿名下载（2026-09-11：release 资产 302→206、raw install.sh 200、ghcr 镜像 manifest 200，全部未带凭据）
- [x] 端到端验证 `brew install dreamor/tap/memvault`（Apple Silicon，v0.3.0，brew test 绿）
- [x] 端到端验证 `curl -fsSL .../scripts/install.sh | bash`（macOS ARM64 实测安装 0.3.0 并可执行）
- [ ] （Windows 机器）验证 `install.ps1`——唯一剩余项；脚本已静态核对通过（路径分隔符 / Join-Path / TLS12 / SHA 校验逻辑规范，zip 结构与 `tar -a` 产物吻合）

### Phase 2 — 各渠道正式发布

- [x] 推正式 tag `v0.3.0` 触发 `release.yml`（2026-09-11 完成并实测确认：14 资产 + ghcr 多平台镜像，见总览）
- [x] 处理 v0.2.0 pre-release：已删除（2026-09-11，release + tag 同步清理，删除前核对仅含旧 ARM64 归档 + SHA256SUMS）
- [x] crates.io：首版已完成（2026-09-11，四 crate 顺序本地发布）；后续版本跑 **Publish (manual)** 或配 trusted publishing
- [x] Obsidian：**已上架**（2026-09-11 通过 community.obsidian.md 门户复审上线；后续迭代见下一行源码归一项）（obsidian-releases 的 PR 流程已官方废弃，`community-plugins.json` 老 PR 法勿用）。~~仓库结构合规~~ ✅ 已用独立仓库 `dreamor/memvault-obsidian` 解决（根目录 README/LICENSE/manifest.json + 自带 release CI，release 0.3.0 资产已验证）。BRAT 验证：BRAT 加 `dreamor/memvault-obsidian` 即装
- [ ] Obsidian 插件的**源码归一**：`obsidian-plugin/`（monorepo）与 `dreamor/memvault-obsidian` 目前是两份拷贝，后续插件功能开发应落在独立仓库（canonical），monorepo 侧条目改为废弃指针或镜像；monorepo release.yml 的 `obsidian-package` job 与之重复，迁移后可裁掉

- [x] npm：首版已完成（2026-09-11，`@dreamor/dsh-memvault@0.3.0` 本地 `npm publish` 走浏览器 2FA）；后续版本再跑 **Publish (manual)**——注意同版本重复 publish 会报 409，0.3.0 不要再手动触发
- [x] Homebrew：v0.3.0 formula 已生成推送（tap commit `085d3f6`，SHA-256 `5b2bedea…`）并实测 `brew install` + `brew test` 全绿；修复生成脚本的非法 DSL `only_arm64`（`scripts/update-homebrew-formula.sh`）
- [x] Docker Hub：**端到端验证通过**（2026-09-11：secrets 配置 → rebuild-docker 双推 ghcr + docker.io → `docker.io/dreamor/memvault:0.3.0` 匿名 manifest 拉取 200）。**遗留坑已修**：workflow_dispatch 会静态拒绝 step-if 里的 `secrets` context（HTTP 422），rebuild-docker.yml 已改为检查 step 输出 `enabled=true/false` 门控；**release.yml 若未来加 dispatch 触发需同样改造**（现 tag 触发不受影响，其 job 内的 `if: secrets.DOCKERHUB_TOKEN != ''` 写法在 dispatch 场景会炸）
- [x] MCP 官方 registry：**已收录**（2026-09-11，记录 `io.github.dreamor/memvault` v0.3.0，status=active，OCI package 指向带验证注解的 ghcr 镜像；`mcp-publisher publish` 全程 API 无 PR）。镜像注解经 rebuild-docker workflow 重建进 0.3.0 tag（history 34573892668）。注意：publish 会校验 `description` ≤100 字符。其余四站待发起：Glama（GitHub 登录 claim，最快）、mcp.so、PulseMCP（网页表单）——官方 registry 是它们的爬源，已具备同步条件；Smithery 门槛最高放最后

### Phase 3 — 发布后收尾

- [ ] README 顶部徽章：替换/新增 crates.io 版本徽章、GitHub Release 最新版徽章
- [ ] 文档同步：更新 `DISTRIBUTION.md` 渠道矩阵状态、`RELEASING.md` 手动步骤勾选
- [x] `CHANGELOG.md` 补正式版条目：已切出 `[0.3.0] — 2026-09-07` 章节（原 `[Unreleased]` 内容归档，上方保留一个新的空 `[Unreleased]`）
- [x] 建立反馈渠道（2026-09-11：repo 侧 Discussions 已开启；SECURITY.md / CONTRIBUTING.md 内容已成体系、README 已链 Discussions，反馈链路齐备）
- [x] **恢复全量 Dependabot 版本更新** → 决定**不恢复**（2026-09-11 拍板：维持「仅安全更新」模式——CVE 公告才触发修复 PR、平时不消耗 CI 额度；非安全类依赖升级按需手动处理）。如未来翻案，完整配置仍在 git 历史 `5526e3d^:.github/dependabot.yml`

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
- **private 期间分发受限**：~~已解除~~（2026-09-11 转 public，全部渠道实测可用）；Windows 视觉化验证仍需真机。
- **未签名二进制**：GitHub Releases 资产无代码签名 / notarization，Gatekeeper 首次打开需右键确认（与常见 OSS CLI 一致）。
