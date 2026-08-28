# DeepSeek Harness (dsh) 原生桥接插件 —— 设计文档

> 状态:**已实现并验证**(2026-08-17~18,见 §7 实测记录与 §7.4)。本文档是当初的设计备忘,v2 已对照 `docs/deepseek-harness/`(用户 clone 的官方源码)校正、§7 附真实运行验证(真实 dsh v0.1.0-rc.6 + 真实 `memvault-proxy`)。原 §1 所述"Rust 侧不需要改动"的唯一例外——`/health` 端点——已于 2026-08-18 补上,见 §7 TODO。dsh 快速迭代到 v0.1.1-rc.2 后已复核兼容性,见 §7.5——结论是代码不用改,只是 `dsh-plugin` 的 devDependencies 版本号过期了。
> 关联:[`docs/INSTALL.md` §2.5](INSTALL.md#25-deepseek-harness-dsh) 中记录的通用 MCP 接入方式仍然有效且更简单,本设计是在其之上追加的**深度集成**选项。

## 0. 背景与动机

[DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness)(`dsh`)是 DeepSeek 官方开源的 Agent Harness,底层构建在 **Cordis** 插件元框架之上("一切皆插件"):模型适配器、工具注册表、系统提示词组装、会话日志、甚至 Agent Loop 本身都是可替换的 Cordis 插件。

MemVault 目前对 dsh 的"支持"仅是 README/INSTALL.md 里的一句"✅ 标准 MCP stdio 配置"——本质是把 `memvault-mcp`/`memvault-proxy` 当作**任意一个通用 MCP server**接入,dsh 侧完全不知道 MemVault 是什么。这带来两个实际缺口:

1. **注入是"拉"不是"推"**:MUST 级记忆只有在 agent 主动读取 `memory://session-inject` 资源或调用 `session_start` 工具时才会出现在上下文里,agent 可能根本不去读。
2. **抽取闭环靠 agent 自觉**:`notify_response` 工具需要 agent 在每轮回复后**主动调用**才会触发记忆抽取,没有任何机制强制或自动完成这一步。

要真正解决,需要把 MemVault 接到 dsh 的 Cordis 事件系统里,在 dsh 组装 system prompt 和结束一轮对话的**那个时间点**主动介入,而不是被动等待 agent 调用工具。

## 1. 结论:Rust 侧不需要改动

`crates/memvault-proxy` 已经具备本设计所需的全部能力:`session_start` 工具、`memory://session-inject` 资源(`InjectionEngine`)、`notify_response` 工具(`ResponseExtractor`)、`--transport sse` 网络传输。本设计**不新增/修改任何 Rust 代码**,只新增一个 TypeScript 包。

唯一算得上"顺手可做"的 Rust 侧改进(非必需):给 `memvault-proxy` 的 SSE server 加一个 `/health` 端点,方便桥接插件做进程就绪探测。

## 2. 已对照真实源码校正(v1 → v2 变更记录)

v1 版本完全基于二手博客/教程写成。用户把 `deepseek-ai/deepseek-harness` clone 到了 `docs/deepseek-harness/` 后,逐项核对了源码,以下是**修正结果**(逐条给出真实文件路径与代码引用):

| # | v1 的假设(博客来源) | 校对结果 | 真实情况 |
|---|---|---|---|
| 1 | 插件是 `export const name/inject` + `export function apply(ctx, config)` | ✅ 基本正确 | `vendor/cordis/src/registry.ts`:`Plugin.Function`/`Plugin.Object` 接口,`resolve()` 认出"有 `.apply` 方法的对象"——一个 ESM 模块把 `name`/`inject`/`apply` 都作为具名导出,整个模块被当作 `Plugin.Object` 传给 `ctx.plugin()` |
| 2 | `ctx.systemPrompt`、`ctx.sessions`、`ctx.tools`、`ctx.agentLoop`、`ctx.llm` 都是真实服务键 | 部分确认 | `ctx.systemPrompt` **实锤确认**(`packages/core/system-prompt/src/index.ts:14-16` 的 `declare module` 增强);`ctx.tools` 高置信(`ctx.tools.register(defineTool(...))` 在仓库里被用了 100+ 次);`ctx.sessions`/`ctx.agentLoop`/`ctx.llm` 只看到了对应的包目录(`packages/core/session`、`agent-loop`、`llm`),没有直接读到 `declare module` 那一行确认具体 key 名 |
| 3 | `ctx.effect(() => cleanup)`、`ctx.on(name, listener)` | ✅ 确认 | `vendor/cordis/src/fiber.ts`(`Fiber.effect()`,`Context extends Pick<Fiber, 'effect'>`)与 `vendor/cordis/src/events.ts`(`EventsService.on()`) |
| 4 | 存在线性事件流水线 `turn/start -> agent/pre-step -> ... -> turn/end` | ❌ **推翻** | 见下方 §3 详细说明——真实机制是"一个 Cordis 事件 `session/event` + 一个判别式 `type` 字段",不是十几个独立的 Cordis 事件 |
| 5 | `system-prompt/assemble` 监听器签名是 `(sections, next)` | ❌ **推翻** | 真实签名是 `(assembly: PromptAssembly, context: AssembleContext, next)`,而且**根本不需要监听这个事件**——有更简单的注册 API `ctx.systemPrompt.section(...)`,见下方 §4.3 |
| 6 | `turn/end` 事件里能拿到 "本轮 assistant 文本" | ❌ **推翻** | `turn/end` 的 payload 只有 `{ turn: number, reason: TurnEndReason }`,不含任何消息内容;assistant 文本要从另一个事件 `assistant/message` 里单独收集,见下方 §3 |
| 7 | `ctx.tools.register(defineTool({ name, description, parameters, output, execute }))` | ✅ 确认 | `packages/core/tools/src/schema.ts:545` 的 `defineTool()`,真实包名 `@deepseek-ai/dsh-tools`,在仓库里有上百个真实调用点(如 `packages/shell/tool-bash/src/index.ts:242`) |
| 8 | dsh 原生 MCP 客户端能力,配置嵌套在某个"具名插件"下 | ✅ 确认且更具体 | 真实包 `@deepseek-ai/dsh-mcp-client`(`packages/mcp/mcp-client/src/index.ts`),**每个上游 MCP server 对应一个独立插件实例**,不是一个 `mcpServers` 列表;详见下方 §5 |
| 9 | vendor 后的 Cordis 改名为 `@deepseek-ai/cordis` | ✅ 确认 | `vendor/cordis/package.json`:`"name": "@deepseek-ai/cordis"`, `"version": "4.0.1"` |
| 10 | 仓库结构类似博客描述(`packages/`、`apps/`、`vendor/`、`python/`、`native/`) | ✅ 确认 | 顶层目录:`apps/ assets/ docs/ examples/ native/ packages/ patches/ python/ scripts/ vendor/ website/`,`packages/` 下 40+ 分组(`core/`、`mcp/`、`shell/`、`fs/`、`subagent/`……) |

## 3. 真实机制:`session/event`,不是一堆独立的 turn 事件

`packages/core/session/src/index.ts` 顶部注释原话:

> "Event-sourced session service: append-only session log, in-memory store, and the derived LLM message history. **Persistence is a plugin concern (subscribe to `session/event`, drain on `session/flush`)**."

也就是说,dsh 会话系统的对外接口是**一个** Cordis 事件 `session/event`,携带一个判别式联合类型的 payload(`packages/core/session/src/types.ts:236` 的 `SessionEventMap`)。`type` 字段决定 `data` 的形状。与本设计相关的两个真实类型:

```ts
// packages/core/session/src/types.ts:245-273
'turn/end': { turn: number; reason: TurnEndReason }
'assistant/message': { turn: number; step: number; message: AssistantMessage; usage?: TokenUsage }
```

`TurnEndReason` 是判别式联合(`types.ts:155-176`):`completed | aborted | blocked | error | max-tokens | interrupted`。**`turn/end` 本身不带任何消息内容**——要拿到"这一轮 assistant 说了什么",必须在监听 `session/event` 期间,按 `turn` 号把陆续到达的 `assistant/message` 事件缓存下来,等对应 `turn/end` 到达且 `reason.kind === 'completed'` 时才取出使用(`aborted`/`error`/`interrupted` 等异常结束不应该喂给抽取管线,这是比 v1 设计更严谨的一点)。

真实的监听代码形态(替代 v1 里错误的 `ctx.on('turn/end', ...)`):

```ts
const pendingAssistantText = new Map<number, string>() // turn -> 最新 assistant 文本

ctx.on('session/event', (session, event) => {
  if (event.type === 'assistant/message') {
    pendingAssistantText.set(event.data.turn, extractText(event.data.message))
  } else if (event.type === 'turn/end') {
    const text = pendingAssistantText.get(event.data.turn)
    pendingAssistantText.delete(event.data.turn)
    if (text && event.data.reason.kind === 'completed') {
      void client.notifyResponse(text, config.agentId)
    }
  }
})
```

**更新(已实现并验证)**:`extractAssistantText(message: AssistantMessage)` 的内容块结构已对照 `@deepseek-ai/dsh-llm`(`npm install` 装下来的真实包,`lib/types/message.d.ts` + `lib/types/types.d.ts`)确认——`AssistantMessage.content: ContentBlock[]`,`ContentBlock = TextBlock | ReasoningBlock | ImageBlock | ToolCallBlock | ToolResultBlock` 判别式联合,`TextBlock = { type: 'text', text: string }`。实现只取 `type === 'text'` 的块拼接,跳过 `reasoning`(模型思考过程,非用户可见输出)等其他块类型。见 `dsh-plugin/src/extract-text.ts`。

## 4. `dsh-plugin/` 包设计

新增顶层目录,与现有 `vscode-extension/`、`obsidian-plugin/` 平级,npm 包名 `@memvault/dsh-memvault`。

```
dsh-plugin/
  package.json          # "dsh": { "bundle": { "patch": "./cordis.patch.yml" } }
  cordis.patch.yml        # 声明插件 id 与默认 config
  src/
    config.ts             # Config schema(Schemastery,见 §4.1)
    process-manager.ts      # spawn/attach memvault-proxy,健康检查,ctx.effect 生命周期
    mcp-client.ts             # 薄 MCP 客户端:session_start / notify_response / 读资源
    index.ts                  # apply(ctx, config) 插件入口
```

### 4.1 配置(`config.ts`)—— 改用 Schemastery,不是裸 TS interface

dsh 生态的插件 Config 一律是 `@deepseek-ai/schemastery` 的 schema 对象(不是 zod,也不是普通 TS interface + 默认值对象)——`vendor/cordis/src/registry.ts` 的 `resolveConfig()` 通过 `runtime.Config['~standard'].validate(config)` 校验,这是 `@standard-schema/spec` 接口,Schemastery 的 `z.object()` 实现了它。真实例子见 `packages/mcp/mcp-client/src/index.ts` 的 `Config`:

```ts
import z from '@deepseek-ai/schemastery'

export const Config = z.object({
  mode: z.union([z.const('spawn'), z.const('attach')]).default('spawn'),
  db: z.string().default('~/.memvault/data.db'),
  url: z.string(),
  binaryPath: z.string(),
  agentId: z.string().default('dsh'),
  injectOnAssemble: z.boolean().default(true),
  extractOnTurnEnd: z.boolean().default(true),
})
```

### 4.2 进程管理(`process-manager.ts`)

与 v1 相同,未受本次校对影响:
- `mode = 'spawn'`:读取/合并 `~/.memvault/proxy.yaml`(`proxy.transport: sse`,`proxy.db`),`child_process.spawn('memvault-proxy')`,轮询就绪,`ctx.effect(() => () => child.kill())`。
- `mode = 'attach'`:直接使用 `config.url`。

### 4.3 系统提示词注入(`index.ts`)—— 用 `ctx.systemPrompt.section()`,不是监听 waterfall

v1 设计里"监听 `system-prompt/assemble` 事件,往 `sections` 数组 push"这条路**技术上仍然可行**(真实签名见下),但 dsh 提供了更简单、更符合 Cordis "注册即可逆" 风格的 API:`SystemPrompt.section()`(`packages/core/system-prompt/src/index.ts:338` 起的 `SystemPrompt extends Service` 类):

```ts
section(section: PromptSection): () => void
// PromptSection = { name: string; order: number; text: string | ((context: AssembleContext) => string); complete?: boolean }
```

调用一次 `ctx.systemPrompt.section({...})` 就完成注册,disposer 由 Cordis 的 effect 系统自动管理,插件卸载时自动撤销——**不需要手动挂 `ctx.effect()`**(`section()` 内部已经是 `this.layers.effect(...)`)。

**关键限制**:`text` 字段的函数形式是**同步**的(`(context: AssembleContext) => string`),不能返回 `Promise`。而我们从 `memvault-proxy` 读取注入内容是一次异步 MCP 调用。解法是维护一个本地缓存,后台异步刷新,`text` 函数只读缓存——这正好是 `memvault-proxy` 自己的 `InjectionEngine.refresh_if_needed()` / `get_current_injection()` 缓存模式的镜像实现:

```ts
import { Context, Service } from '@deepseek-ai/cordis'

export const name = 'memvault'
export const inject = ['systemPrompt']
export const Config = /* 见 §4.1 */

let cachedInjection = ''

export function apply(ctx: Context, config: Config) {
  if (config.mode === 'spawn') startProxy(ctx, config)
  const client = createMcpClient(ctx, config)

  if (config.injectOnAssemble) {
    ctx.systemPrompt.section({
      name: 'memvault:injection',
      order: 1, // persona(order 0)之后,工具说明(order 100+)之前
      text: () => cachedInjection,
    })
    // 后台刷新缓存 —— 可以挂在 session/event 上按需刷新,或用定时器兜底
    ctx.on('session/event', async (session, event) => {
      if (event.type === 'turn/start') {
        cachedInjection = await client.readSessionInject()
      }
    })
  }

  if (config.extractOnTurnEnd) {
    const pendingAssistantText = new Map<number, string>()
    ctx.on('session/event', (session, event) => {
      if (event.type === 'assistant/message') {
        pendingAssistantText.set(event.data.turn, extractText(event.data.message))
      } else if (event.type === 'turn/end') {
        const text = pendingAssistantText.get(event.data.turn)
        pendingAssistantText.delete(event.data.turn)
        if (text && event.data.reason.kind === 'completed') {
          void client.notifyResponse(text, config.agentId)
        }
      }
    })
  }
}

function extractText(message: unknown): string { /* 见 §3 更新说明与 §7.4——实现已完成并改名为 extractMessageText,现在同时用于 assistant 与 user 消息 */ }
```

如果不追求"缓存"这点复杂度,waterfall 方式仍是可行的备选(签名已确认):

```ts
export const inject = ['systemPrompt']
// PromptAssembly = { sections: AssembledSection[]; contexts: AssembledContext[]; tools: ToolSchema[]; variables: Record<string, string|undefined> }
ctx.on('system-prompt/assemble', async (assembly, context, next) => {
  assembly.sections.push({ name: 'memvault:injection', text: await client.readSessionInject() })
  return next()
})
```
两种方式的取舍:`section()` 更符合仓库既有风格、写法更少,但需要自己管理缓存刷新时机;waterfall 方式能拿到当次assembly 的实时数据(`context.scope` 等),但每次 assemble 都要等一次异步调用,可能拖慢 system prompt 组装的关键路径。**推荐优先用 `section()` + 缓存**。

### 4.4 MCP 客户端(`mcp-client.ts`)

与 v1 相同:用标准 `@modelcontextprotocol/sdk` HTTP client 连接 `memvault-proxy` 的 `/mcp` 端点(`rmcp` 实现的标准协议,无私有扩展),封装 `sessionStart` / `notifyResponse` / `readSessionInject` 三个方法,连接生命周期包在 `ctx.effect()` 里。

### 4.5 分发(`package.json` + `cordis.patch.yml`)

```jsonc
// dsh-plugin/package.json(节选)
{
  "name": "@memvault/dsh-memvault",
  "type": "module",
  "dsh": { "bundle": { "patch": "./cordis.patch.yml" } }
}
```

```yaml
# dsh-plugin/cordis.patch.yml
- id: memvault
  name: '@memvault/dsh-memvault'
  config:
    mode: spawn
    db: '~/.memvault/data.db'
    agentId: dsh
```

## 5. 零代码替代方案:直接用 dsh 原生 `@deepseek-ai/dsh-mcp-client`

这一节完全替换 v1 里"INSTALL.md §2.5 该怎么写不确定"的猜测——现在有了确切答案。

`packages/mcp/mcp-client/src/index.ts` 是 dsh 官方的 MCP 客户端桥接插件(`name = 'mcp-client'`,`inject = ['tools']`)。**每个上游 MCP server 对应一个独立的插件实例**,不是一份 `mcpServers` 列表;它把每个上游工具注册为 `mcp__<serverName>__<原始工具名>`。Config 是判别式联合:

```ts
export type Config =
  | { transport: 'stdio'; serverName: string; command: string; args: string[]; env: Record<string,string>; cwd: string; toolCallTimeoutMs: number; failOnStartupError: boolean; reconnect?: ReconnectConfig }
  | { transport: 'streamable-http'; serverName: string; url: string; headers: Record<string,string>; toolCallTimeoutMs: number; failOnStartupError: boolean; reconnect?: ReconnectConfig }
```

对应到 MemVault,不写任何代码,只用一段 `cordis.patch.yml` 就能拿到全部 16 个工具(作为 `mcp__memvault__save_memory`、`mcp__memvault__session_start` 等):

```yaml
# stdio 方式(spawn memvault-proxy 本身)
- id: memvault-mcp
  name: '@deepseek-ai/dsh-mcp-client'
  config:
    transport: stdio
    serverName: memvault
    command: /absolute/path/to/memvault-proxy
    args: []

# 或者连接已经在跑的 SSE 实例
- id: memvault-mcp
  name: '@deepseek-ai/dsh-mcp-client'
  config:
    transport: streamable-http
    serverName: memvault
    url: http://127.0.0.1:3778/mcp
```

**这条路径应该写进 `docs/INSTALL.md` §2.5,取代目前那段"具体嵌套方式请以官方文档为准"的免责声明**——见 §7 TODO。

## 6. 与现状的关系(更新后的对比表)

| | §5 零代码(`@deepseek-ai/dsh-mcp-client`) | §4 本插件(`@memvault/dsh-memvault`) |
|---|---|---|
| 接入方式 | dsh 官方 MCP 客户端插件 + 一段 config | 独立 Cordis 插件 |
| 拿到 16 个工具 | ✅(`mcp__memvault__*` 前缀) | ✅(内部也是同一套 MCP 协议) |
| MUST 记忆自动进 system prompt | ❌ | ✅(`ctx.systemPrompt.section()`) |
| 每轮自动抽取记忆 | ❌ | ✅(`session/event` 監聽 + `turn/end` 判定 `completed`) |
| 依赖未经验证的 API | 否(已对照源码确认) | 一处小空白(`AssistantMessage` 内容块结构,见 §3) |
| 实现/维护成本 | 零 | 一个新 npm 包 |

## 7. 验证计划与 TODO(状态:1、2、3 已完成)

1. ~~**MCP 客户端层**~~ —— 已实现(`dsh-plugin/src/mcp-client.ts`),但尚未跑一次真实的 `memvault-proxy --transport sse` 联调(见下方"仍未做")。
2. ~~**Cordis 接线层**~~ —— **已完成并通过**。`dsh-plugin/src/index.smoke.test.ts` 用 `npm install` 装下来的真实 `@deepseek-ai/cordis@4.0.1` + `@deepseek-ai/dsh-system-prompt` 搭建一个真实 `Context`,`ctx.plugin()` 挂载本插件,断言 `ctx.systemPrompt.assemble()` 的结果里真的包含 `memvault:injection` 这个 section。`npm run build`(`tsc --strict`)与 `npm test` 均通过——这不是针对 mock 类型跑的,是针对 dsh 团队发布到 npm 的真实包(`@deepseek-ai/cordis`、`dsh-session`、`dsh-system-prompt`、`dsh-llm`,均为 `0.0.1-rc.1`)跑的。
3. ~~**剩余空白**~~ —— 已解决,见 §3 更新说明。
4. **真实集成(已完成)**——用户本机 `npx @deepseek-ai/dsh web` 装好了真实 dsh(v0.1.0-rc.6),完整跑通:插件被 `dsh plugin --profile web add` 装进 web profile、被 dsh 加载、spawn 了 `memvault-proxy` 二进制并成功监听 3778;一次真实对话后,直接从 `~/.dsh/sessions/.../session.jsonl.zstd` 里读出的 `request/header.data.header.system` 原文**确认包含**了 MemVault 注入的 MUST/REF 记忆;直接对真实运行中的 `memvault-proxy` 发 `notify_response` **确认**把新记忆写进了数据库(`memvault-cli list` 从 8 条变成 10 条)。详见下方"7.1-7.3 实测记录"——这个过程本身又挖出并修了三个额外的真实 bug。

### 7.1 实测记录(2026-08-17,针对真实 dsh v0.1.0-rc.6)

装/配的过程本身就发现并修正了两处此前设计文档没预料到的真实坑:

1. **`dsh plugin --profile <name> add <path>` 会自动把包名写进该 profile `package.json` 的 `dsh.profile.bundles` 列表**——不需要手工编辑 profile 自己的 `cordis.patch.yml` 去引用新插件,`pnpm add` 之外这一步是 `dsh` CLI 自己做的。
2. **一条裸的 `- id: memvault ...` patch 是"覆盖"语义,不是"插入"语义**——`@deepseek-ai/cordis-plugin-include` 的 `applyEntryPatches`(`vendor/include/src/index.ts`)只有在 entry 列表里已经存在同 `id` 的条目时才会应用覆盖;对一个还不存在的 `id` 用裸写法,会直接报错 `patch: entry "memvault" not found` 并整条跳过。**新增条目必须包一层 `insert:`**:
   ```yaml
   - insert:
       - id: memvault
         name: '@memvault/dsh-memvault'
         config: { ... }
   ```
   `dsh-plugin/cordis.patch.yml`(§4.5)在 v2 版本里就有这个 bug(裸 `id`,没有 `insert`),已经在这次实测里修正。本地开发时想覆盖单个字段(比如把 `binaryPath` 指向本机编译的二进制)则反过来要用**不带 `insert` 的裸 `id` 覆盖 patch**,并且要把整个 `config` 重新写一遍(覆盖是整体替换 `config`,不是逐字段合并)。

实测命令序列(供复现):
```bash
cd ~/.dsh/profiles/web
npx @deepseek-ai/dsh plugin --profile web add /path/to/MemVault/dsh-plugin   # pnpm add + 自动写入 bundles
# 手工在 profile 的 cordis.patch.yml 里加一条覆盖 patch,把 binaryPath 指到本机编译的二进制
npx @deepseek-ai/dsh --profile web --dump-config   # 确认组合后只有一条 id: memvault,字段正确
npx @deepseek-ai/dsh web --port 0                  # 真实启动,观察日志
```
最后一步的真实日志里能直接看到 Rust 侧 `memvault_proxy`/`memvault_core` 的 tracing 输出(`loading agent registry`、`MCP Proxy (SSE) listening on http://127.0.0.1:3778/mcp`),证明 `dsh-plugin/src/process-manager.ts` 的 `startProxy()` 确实把 dsh 传入的 config(`db`/`port`/`binaryPath`)正确转成了子进程参数并成功拉起了真实二进制。

### 7.2 第二轮实测:embedding provider 环境变量泄漏 + 二进制过期

真实跑起来后,Rust 侧日志报了一个跟本插件本身无关但会让人误以为插件坏了的错:`memvault_core::embedding: embedding API error status=401 Unauthorized`。根因分两层:

1. `memvault-proxy` 的 embedding provider 选择逻辑读 `OPENAI_API_KEY`/`OPENAI_API_BASE` 环境变量(`crates/memvault-core/src/embedding.rs` 的 `build_embedder_from_env()`)。插件用 `child_process.spawn(binaryPath, [], { stdio: 'inherit' })` 拉子进程时**没有显式传 `env`**,于是继承了 dsh 自己进程环境里残留的 `OPENAI_API_KEY`(大概率是配置 dsh 自身模型时留下的,跟 MemVault 的 embedding 凭据完全是两个东西),导致 memvault-proxy 误用这个 key 去调 OpenAI embedding 接口,拿到 401。**修复**:`config.ts` 新增 `embeddingProvider`(默认 `'native'`),`process-manager.ts` spawn 时显式传 `env: { ...process.env, MEMVAULT_EMBEDDING_PROVIDER: config.embeddingProvider }`,确保不管 dsh 进程环境里有什么,都强制走本地 fastembed 模型。
2. 修完第一层后日志仍然复现同样的 401——排查发现本机 `target/release/memvault-proxy` 二进制的编译时间(17:33)早于 `native` embedding 支持代码的最后修改时间(`embedding.rs` 17:57,`native_embedding.rs` 18:03)。**二进制本身就没编译进这个特性**,所以传入的 `MEMVAULT_EMBEDDING_PROVIDER=native` 被旧二进制的 `match` 语句当成未知值,落进兜底分支,又把它当成一个自定义 OpenAI-compatible provider 名字,继续用默认端点和默认模型、继续复用泄漏的 key。**修复**:`cargo build --release --bin memvault-proxy --bin memvault-mcp` 重新编译。重新跑一次,日志变成 `memvault_core::native_embedding: native embedding provider initialized model="v1.5 release of the small Chinese model" dimension=512`,无网络调用,无 401。

### 7.3 第三轮实测:注入完全没生效 —— 两个真实的时序 bug

前两轮修完、插件加载和 proxy 启动都正常之后,让用户在真实 dsh web 里发消息,再用 `zstd -d` 解出 `~/.dsh/sessions/.../session.jsonl.zstd` 直接读 `request/header` 事件里的 `system` 原文——**完全没有**任何 MemVault 注入内容,而且连续两轮(turn 1、turn 2)都是这样。日志本身没有任何报错,插件看起来"正常"。

加了一行临时的 `appendFileSync` 调试(直接写文件,绕开所有日志级别/可见性的不确定性)后,5 秒内就定位到了两个真实 bug:

1. **注入缓存的启动竞态**:插件 `apply()` 里"立即"触发第一次 `readSessionInject()`,但这时候 `memvault-proxy` 子进程刚被 `spawn()`,还没真正监听端口(从 spawn 到 `MCP Proxy (SSE) listening on ...` 之间有几百毫秒),于是第一次连接直接 `fetch failed`。
2. **比第一个更严重**:`mcp-client.ts` 里 `connected ??= (async () => {...})()` 这个记忆化写法,只处理了"还没连接"和"已经连上"两种状态,**没处理"连接失败"**——一旦第一次连接失败,`connected` 就被赋值成一个已经 reject 的 Promise,**永久缓存下来**。之后每一次调用(每轮 `turn/end` 的刷新、每次 `notify_response`)都会直接复用这个已经失败的 Promise 并立刻 reject,哪怕 proxy 早就已经正常监听了也不会重试。这解释了为什么连 turn 2(此时 proxy 显然已经启动完成)也拿不到任何注入内容。

**修复**:
- `index.ts`:`apply()` 现在会等 `startProxy()` 返回的 `ready` Promise resolve 之后才发起第一次 `readSessionInject()`,而不是立即发起。
- `mcp-client.ts`:`ensureConnected()` 在连接失败时把 `connected` 重置为 `undefined`,让下一次调用重新尝试连接,而不是永久复用失败的 Promise。

修完后重新验证:真实 session log 里 `request/header.system` 出现了完整的 `[MEMORY CONTEXT - session: inj_xxx] ... [MUST] ... [REF] ...` 注入内容;直接对真实运行的 `memvault-proxy` 调用 `notify_response`(用含"我偏好"/"我们的项目"这类抽取器能识别的第一人称表述)后,`memvault-cli list` 的记忆总数从 8 条变成 10 条。两条链路都在真实环境里跑通,不是纸面推断。

（一个顺带的发现,跟插件无关但值得记一下:`extractor.rs` 的偏好抽取信号词只认第一人称"我偏好/我喜欢/我习惯",不认"你偏好"这种转述用户偏好的句式——用"你偏好使用 tabs"去测会被"合理拒绝",换成"我偏好使用 tabs"才会被抽取。这是抽取器本身的设计,不是本插件的问题。）

- [x] 实现 `dsh-plugin/` 各文件(§4)
- [x] 补全 `extractAssistantText()`(§3)
- [x] 针对真实 npm 包跑通 build + 单测 + Cordis 挂载 smoke test
- [x] 修正 `cordis.patch.yml` 的 insert/override 语义 bug(§7.1)
- [x] 在真实 dsh v0.1.0-rc.6 里验证插件加载 + `memvault-proxy` 真实 spawn 成功
- [x] 修正 embedding provider 环境变量泄漏 + 重新编译过期二进制(§7.2)
- [x] 修正注入缓存启动竞态 + MCP 客户端失败后永久卡死的记忆化 bug(§7.3)
- [x] 用真实对话 + 真实 `notify_response` 调用确认注入与抽取两条链路端到端可用
- [x] (可选,Rust 侧)给 `memvault-proxy` SSE server 加 `/health` 端点 —— 已实现(2026-08-18,`crates/memvault-proxy/src/main.rs` 的 `health()` handler + `/health` 路由 + 2 个单测),`dsh-plugin` 的 `waitForPort` 已改为探测 `/health` 而非伪造 `/mcp` 握手
- [x] 更新 `docs/INSTALL.md` §2.5:用 §5 的具体 config 取代现有的"以官方文档为准"免责声明,并补充本插件作为深度集成选项
- [x] 扩大抽取召回率:信号词第二/三人称镜像 + 直接抽取用户原话(§7.4)

### 7.4 第四轮改动:信号词覆盖 + 直接抽取用户原话

§7.3 末尾提到的"抽取器只认第一人称"不只是备注,后续复盘发现这是个真实的召回率问题,不是"设计如此所以没问题":库里已有的种子记忆全部是第三人称"用户……"措辞,assistant 真实确认用户偏好时最自然的说法是第二人称"你偏好……"——这两种最常见的真实措辞都完全绕过了原来的信号词列表。更根本的是,只抽 assistant 转述本质上是在"猜 assistant 有没有认真复述用户的话",而用户自己说话天然是第一人称("我喜欢用tabs"),直接抽用户原话反而完美匹配现有信号词设计,不需要额外改信号词就有更高召回率,也是让 MemVault 区别于"只会记 assistant 说了什么"的记忆工具的关键。

做了两件互补的事:

1. **`crates/memvault-core/src/extractor.rs`**:`preference_signals`/`fact_signals` 追加第二/三人称镜像(`你偏好`/`你喜欢`/`你是`/`你的项目`,`用户偏好`/`用户喜欢`/`用户是`/`用户的项目`……),`to_instruction` 的 `PREFIXES` 同步加对称条目。这一改动对全部 4 个抽取入口(CLI `extract`、`memvault-mcp` 的 `extract_memories`、`memvault-proxy` 的 `notify_response`)都自动生效,因为它们共享同一个 `Extractor::extract`。
2. **`crates/memvault-proxy/src/extraction.rs` + `handler.rs`**:`notify_response` 的 `NotifyResponseParams` 新增可选字段 `user_text`,在 `response_text` 之外**额外**跑一次针对用户原话的抽取(`ResponseExtractor::extract_and_save_from_user`),两路结果的 `extracted`/`saved`/`skipped` 相加返回。两条路径都会给存下的记忆打 `source:assistant`/`source:user` 标签,方便区分来源(实现上抽成了一个私有的 `extract_and_save_labeled`,`extract_and_save` 的公开签名不变,不影响 `extraction.rs` 自己原有的 4 个单测)。

**dsh-plugin 侧同步跟进**,让自动抽取真的把用户原话传过去:

- `extract-text.ts`:`extractAssistantText(message: AssistantMessage)` 泛化成 `extractMessageText(message: Message)`——`Message` 是 `AssistantMessage`/`UserMessage` 的公共父类型,`content: ContentBlock[]` 字段结构完全一致,同一套过滤逻辑天然复用。
- `mcp-client.ts`:`notifyResponse` 加第三个可选参数 `userText?`。
- `index.ts`:`session/event` 监听里新增关键点——`user/message` 事件的 `data` 本身**没有 `turn` 字段**(跟 `assistant/message`/`turn/start`/`turn/end` 不一样,这是解真实 session log 时确认的真实事件形状,不是猜的),所以要用 `turn/start` 更新的 `currentTurn` 变量去关联;并且只收 `event.data.source.kind === 'user'` 的真人消息,跳过 `source.kind: 'plugin'` 的插件注入合成内容(sandbox 策略、skill 列表等,这些也会以 `user/message` 形式出现在事件流里)。

**验证**(手写探针脚本对真实运行的 `memvault-proxy` 直接调 `notify_response`,验证完删脚本):

- 场景一:`response_text: "好的，明白了"`(不可抽取) + `user_text: "我偏好用 4 个空格缩进，不用 tab。"`(可抽取)→ 抽取成功,新记忆打了 `source:user` 标签。
- 场景二:`response_text: "好的，我记住了：你偏好使用 Rust 而不是 Go 来写后端服务。"`(§7.3 记录里"合理拒绝"的那个第二人称场景)→ 现在能抽取成功,打了 `source:assistant` 标签。
- 场景三:`response_text: "收到，用户偏好深色主题的编辑器。"`(第三人称)→ 同样能抽取成功。

三个场景跑完,`memvault-cli list` 记忆总数从 10 条变成 13 条,`list_inbox` 工具返回里能看到对应的 `source:user`/`source:assistant` 标签——不是纸面推断。Rust 侧新增单测(`extractor.rs` 的 `test_extract_preference_second_person`/`test_extract_preference_user_prefix`/`test_extract_fact_second_person`/`test_to_instruction_strips_second_person_prefix`,`extraction.rs` 的 `test_extract_and_save_tags_source_assistant`/`test_extract_and_save_from_user_tags_source_user`,`handler.rs` 的 `test_tool_notify_response_extracts_from_user_text_too`)与 dsh-plugin 侧新增单测(`extract-text.test.ts` 的 user message 场景)均已跑绿。

### 7.5 dsh 新版本兼容性复核(2026-08-22,针对真实 dsh v0.1.1-rc.2)

dsh 从本文档 §7.1 测试时的 `v0.1.0-rc.6` 快速迭代到了 `v0.1.1-rc.2`(中间经过 rc.7/rc.8,rc.8 带来了多模态图片输入、Claude Code/Codex 子代理化、SQLite 会话存储格式变更等改动),官方持续声明"开发者预览阶段,不保证向后兼容"。复核结论:**`dsh-plugin/src/*` 代码无需改动,仅 `package.json` 的 devDependencies 版本号是过期的**。

排查过程:

1. **devDependencies 严重滞后**:`@deepseek-ai/dsh-llm`/`dsh-session`/`dsh-system-prompt`/`dsh-scope` 四个包在 npm 上早已从 `0.0.1-rc.x` 系列整体切到 `0.1.0-rc.x` 再到当前 `0.1.1-rc.2`(四个包版本号完全同步发布,确认是同一个 dsh monorepo 里锁步发布的组件)。原来 `package.json` 里 `^0.0.1-rc.1` 的 semver range 对 `0.1.x` 系列完全不匹配(major.minor.patch 三元组不同,caret range 在 `0.x` 上极窄),`npm install` 只会解析到早已过期的 `0.0.1-rc.5`,本地类型检查/测试实际上一直跑在一个和真实用户环境相差好几个 rc 版本的旧类型定义上——**这是本次唯一确认的真实问题**,已把这四个包(连同 `@deepseek-ai/cordis`/`@deepseek-ai/schemastery`)的 devDependencies 改成精确匹配当前 npm 最新版本(`0.1.1-rc.2`/`4.0.1`/`3.18.1`);`peerDependencies` 本来就是 `"*"` 通配,不受影响,不用改。
2. **实际 API 面逐项比对(装真实 `0.1.1-rc.2` 包的 `.d.ts` 核实,非猜测)**:
   - `ctx.systemPrompt.section({ name, order, text, complete? })` 的 `PromptSection` 接口形状未变。
   - `session/event` 的 `turn/start`/`turn/end`(`{ turn, reason }`,`TurnEndReason` 的 `{ kind: 'completed' }` 等变体)、`assistant/message`(`{ turn, step, message, usage?, interrupted? }`)形状未变;`user/message` 的事件负载仍然是裸的 `UserMessage`(没有自己的 `turn` 字段)—— `index.ts` 靠 `turn/start` 维护 `currentTurn` 去关联的写法仍然成立。
   - `MessageSourceMap`(`{ kind: 'user' }` / `{ kind: 'plugin', plugin }`)未变,`index.ts` 里 `event.data.source.kind === 'user'` 的判断仍然有效。
   - rc.8 引入的多模态改动给 `ContentBlockMap` 新增了 `'image': ImageBlock` 变体,但 `extract-text.ts` 的 `extractMessageText` 本来就是按 `block.type === 'text'` 做穷尽过滤(设计时就没有假设"只有 text/reasoning 两种"),新增的 `image` block 会被安全跳过,不需要改。
   - rc.8 的 SQLite 会话存储格式变更是 dsh 自己的 `~/.dsh/sessions/...` 会话日志存储(`SESSION_FORMAT_VERSION`,预览期固定为 `0`,不提供迁移),跟 MemVault 自己的 `~/.memvault/data.db` 完全无关,不涉及 Rust 侧任何改动。
3. **验证**:`dsh-plugin` 目录内删掉 `node_modules`/`package-lock.json` 后用新 `package.json` 重新 `npm install`(装到真实 `0.1.1-rc.2`),`npm run build`(`tsc --strict`)与 `npm test`(含 `index.smoke.test.ts`——用真实 `@deepseek-ai/cordis`+`@deepseek-ai/dsh-system-prompt` 搭一个真实 `Context` 挂载本插件)全部通过,无需改动任何 `.ts` 源码。

**结论**:这次 dsh 发新版本,MemVault 这边不用跟着改代码,只需要把 `dsh-plugin/package.json` 的版本号跟上(已完成并提交)。§7.1-7.4 记录的行为(注入、抽取、`/health` 探活)在新版本下原样有效。

### 7.6 复核（2026-08-28）：npm 无新版本，GitHub 有 alpha 未发布

用户提示「dsh 发了新版本」后核查：

- npm 上无 scope 的 `dsh` 包 `latest=1.0.1`（"A shell written in JavaScript"）是**无关包**——包名被 DeepSeek Harness 之外的项目占用；DeepSeek Harness 实际主包为 `@deepseek-ai/dsh`，**不要按 `dsh` 的版本号判断**。
- `@deepseek-ai/dsh` 及组件 `dsh-llm`/`dsh-session`/`dsh-system-prompt`/`dsh-scope` 在 npm 上最新均为 **`0.1.1-rc.2`**（`dist-tags.latest`/`next` 一致），与 `dsh-plugin/package.json` 当前精确 pin **完全一致**——无需改动，代码与 smoke 测试维持现状。
- GitHub 官方仓库最新 tag 为 `dsh-v0.1.2-alpha.1`（alpha 预览，**未发布到 npm**）。官方声明开发者预览期不保证向后兼容；待组件包发布正式 rc 后，若类型面有变化，再按 §7.5 的方法复核（装真实包核对 `.d.ts` + `npm run build` + `npm test`）。

**结论**：本轮不构成适配项，无需改任何代码；本段作为核查留痕。

