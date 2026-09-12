# Lex Code 设计文档

> 日期:2026-09-12
> 状态:已与需求方逐节确认
> 需求来源:`docs/requirements/harness-dev-brief-for-claude-code.md`(开发任务书)
> 运行时系统提示词:`assets/coding-agent-system-prompt.md`(外部资源,不硬编码)

## 1. 项目目标

用 Rust 实现类 Claude Code 的 CLI 编程 agent,项目名 **Lex Code**,二进制名 `lex-code`。两大差异化目标:

1. **可插拔多 provider 架构**——统一内部消息表示,双向适配 Anthropic 消息格式与 OpenAI 兼容格式(DeepSeek / Kimi / GLM 共用一个 adapter,差异走配置)。切换 provider 只改配置,不改 Agent Loop 代码。
2. **DeepSeek 磁盘前缀缓存专项优化**——通过确定性序列化与 append-only 历史管理,最大化缓存命中率并降低成本。

跨平台硬性要求:**Linux / Windows / macOS 三平台行为一致**(详见第 8 节)。

## 2. 已确认的关键决策

| 决策点 | 结论 |
|---|---|
| 工程结构 | Cargo workspace 双 crate:`lex-core`(纯库)+ `lex-cli`(二进制) |
| bash_exec shell | 可配置 + 平台默认(Unix:`/bin/sh -c`;Windows:`cmd /C`) |
| provider 联调 | 需求方提供真实 API Key,每个 Phase 结束跑真实冒烟(Phase 1 Anthropic,Phase 2 DeepSeek) |
| 搜索实现 | `grep_search` 内置实现(walkdir/ignore + regex),不调用外部 grep/rg |
| 命名 | crate `lex-core` / `lex-cli`,bin `lex-code` |

## 3. 工程结构与依赖方向

```
lexcode/
├─ Cargo.toml                 # workspace
├─ lex-core/                  # 纯库:不含终端渲染,不依赖 clap
│  └─ src/
│     ├─ lib.rs
│     ├─ message.rs           # 中立消息模型(全项目唯一消息表示)
│     ├─ agent/               # 核心循环状态机 + 事件流
│     ├─ provider/            # Provider trait、两个 adapter、CacheStrategy
│     │  ├─ anthropic.rs
│     │  ├─ openai_compat.rs  # DeepSeek/Kimi/GLM 共用,差异走 ProviderProfile
│     │  └─ cache.rs
│     ├─ tools/               # Tool trait、注册表、5 个内置工具
│     ├─ security/            # 三级权限、可配置正则规则表、检查器
│     ├─ context/             # AGENTS.md 加载、token 估算、压缩触发
│     ├─ prompt.rs            # 系统提示词加载 + {{VAR}} 替换
│     ├─ config.rs            # TOML 解析 + 环境变量注入
│     └─ error.rs             # thiserror 错误类型
├─ lex-cli/                   # 二进制:clap 解析、流式渲染、确认 UI、anyhow
├─ assets/coding-agent-system-prompt.md
└─ docs/
```

依赖方向单向:`lex-cli → lex-core`;core 内部 `agent → provider/tools/security/context`,反向无依赖。

技术栈:tokio、clap、reqwest(stream 特性,SSE 流式)、serde/serde_json、thiserror(core)+ anyhow(cli)、tracing/tracing-subscriber、toml、dirs、anstream、walkdir/ignore、regex。MVP 不引入 ratatui 与 tokenizer 依赖。

## 4. 中立消息模型与 Provider 接口

### 4.1 消息模型(message.rs)

```rust
enum Role { User, Assistant }

enum Block {
    Text { text: String },
    Thinking { reasoning_content: String },   // DeepSeek reasoning_content 的中立表示
    ToolUse { id: String, name: String, input: serde_json::Value },
    ToolResult { tool_use_id: String, content: String, is_error: bool },
}

struct Message { role: Role, content: Vec<Block> }   // Vec 保序,满足 append-only
```

- 所有进入请求 payload 的结构体:字段顺序由 derive 声明顺序显式确定,禁止 HashMap 进入 payload 路径(用 `BTreeMap` 或 `Vec<(String, Value)>` 承载动态 JSON)。
- `Thinking` block 的存在与顺序是 DeepSeek `reasoning_content` 回传正确性的载体,adapter 不得丢弃或重排。

### 4.2 Provider trait

```rust
trait Provider: Send + Sync {
    fn send(&self, ctx: &RequestContext) -> Result<BoxStream<'static, Result<ProviderEvent, ProviderError>>, ProviderError>;
}

struct RequestContext {
    system: String,                 // 已组装(含 AGENTS.md 注入)
    tools: Vec<ToolDefinition>,     // 已由注册表导出的中立定义
    messages: Vec<Message>,         // append-only 历史
}

enum ProviderEvent {
    TextDelta(String),
    ThinkingDelta(String),
    ToolUseStart { id: String, name: String },
    ToolUseDelta { id: String, partial_json: String },
    ToolUseComplete { id: String, name: String, input: Value },
    Completed { usage: Usage },
    Failed(ProviderError),
}
```

- 原生 provider 类型只存在于 adapter 内部;上层只见中立类型。
- 所有 HTTP 调用流式处理(SSE 逐事件解析),禁止攒完整响应再返回。
- API key 只从环境变量读取(`LEX_ANTHROPIC_API_KEY`、`LEX_OPENAI_API_KEY`);base_url / model / 参数来自配置;代码零硬编码凭证与默认 URL。

### 4.3 OpenAICompatibleAdapter 要点

- `tool_calls` 数组 ⇄ `ToolUse` block;`role:"tool"` 消息 ⇄ `ToolResult` block。
- **`reasoning_content` 回传(正确性,非优化)**:一旦历史中出现 tool call,后续请求必须把对应 assistant 消息的完整 `reasoning_content` 原样带回,缺失会被 DeepSeek 拒绝(400)。必须有单测覆盖。
- Provider 间差异(参数支持与否)由 `ProviderProfile` 配置区分,不为每个 provider 写 adapter。

### 4.4 CacheStrategy

```rust
trait CacheStrategy {
    fn prepare(&self, req: &mut OutgoingRequest);   // 请求组装点介入
    fn observe(&self, usage: &Usage);               // 从响应 usage 采集
    fn invalidate(&self);                            // 压缩后"缓存链断开"信号
}
```

- **AnthropicCacheStrategy**:在配置断点(system prompt 末尾、工具定义末尾、历史倒数第二轮末尾)插入 `cache_control: {type:"ephemeral"}`;断点位置可配置。
- **ImplicitPrefixCacheStrategy**(DeepSeek 用):不插标记。`prepare` 时将 system 段、tools 段、messages 段分别序列化为字节串,与上次请求比对:system/tools 必须**逐字节一致**,messages 必须是上次的**严格前缀追加**;不一致记 warning 与遥测计数(不阻断请求)。`observe` 读取 `prompt_cache_hit_tokens` / `prompt_cache_miss_tokens` 输出到 tracing。`invalidate()` 后重置基线并记"缓存链断开"日志,不做恢复逻辑。

## 5. Agent Loop 状态机

状态:`AwaitingInput → AssemblingRequest → AwaitingModel → ExecutingTools ⇄ AwaitingPermission →(回 AssemblingRequest)→ AwaitingInput`

- 流式事件实时上抛给 CLI 层渲染;响应结束后收集本轮全部 tool_use。
- **并发规则**:只读工具(`file_read` / `grep_search` / 只读性质的 bash 命令)同轮 `join_all` 并发;有副作用的工具严格串行。
- **权限检查不可绕过**:检查器在 tools 执行路径内部调用(工具执行器统一入口),CLI 层只消费其结果;确认交互通过 `PermissionHandler` trait 回调上抛,CLI 渲染将执行的具体命令 / diff 后等待用户确认或白名单命中。
- **自动多轮**:工具执行完直接回 `AssemblingRequest` 继续,直到模型产出无 tool_call 的文本回复,或权限被拒/用户中止。
- 工具失败结果作为 `ToolResult { is_error: true }` 回填历史,由模型自行决策重试或报告。

## 6. 工具系统

```rust
trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn schema(&self) -> &ToolSchema;      // 中立 JSON Schema,可序列化为 Anthropic 与 OpenAI function 两种格式
    fn read_only(&self) -> bool;
    async fn execute(&self, input: Value, ctx: &ToolContext) -> Result<ToolOutcome, ToolError>;
}
```

核心工具集:`file_read`、`file_edit`、`bash_exec`、`grep_search`、`todo_write`。

- `file_edit`:精确旧串定位替换;旧串必须唯一命中,否则报错并列出冲突位置;不做整体覆盖。
- `grep_search`:内置实现(`ignore` 目录遍历尊重 .gitignore + regex),跨平台行为一致,只读、可并发。
- `bash_exec`:可配置 shell + 平台默认(见第 8 节);只读判定依据权限规则表,不靠工具自身猜测。
- `ToolRegistry` 提供 `register(Box<dyn Tool>)` 与 `iter()`,为 sub-agent 工具预留扩展点(本期不实现 sub-agent)。

## 7. 安全模型(三级权限)

- 级别:`Auto`(只读,直接放行)/ `Confirm`(写文件、执行命令;需用户确认或命中白名单)/ `Forbidden`(硬拦截,不接受任何确认)。
- 规则表来自 `lex-code.toml` `[security]` 段,三级各为正则数组;内置默认规则作为缺省值合并。用户可追加,**不可静默覆盖 Forbidden 默认项**(显式覆盖需配置里声明,且日志警示)。
- Forbidden 默认覆盖:删除 `.git` 目录;`git push --force` / `-f` 变体;`rm -rf` 高危变体;敏感文件(`.env`、`*.pem`、`id_rsa*` 等命名模式)读取后外发。
- 敏感信息外发判定为**模式级启发式**:同一轮中读取了敏感文件且出现 curl/网络上传类命令即拦截。设计上明确这是启发式而非数据流追踪,后续可增强。
- 检查顺序:Forbidden → Confirm → Auto;每次有副作用的工具调用执行前必经检查器,路径不可绕过。

## 8. 三平台适配(Linux / Windows / macOS)

- **bash_exec**:`tokio::process::Command` 统一执行。默认:Linux `/bin/sh -c`,macOS `/bin/sh -c`(不用 zsh 作默认,`sh` 语义三平台最稳),Windows `cmd /C`。TOML 可显式指定(如 Windows 上 `command="bash", args=["-lc"]` 切 Git Bash)。
- **路径**:全部 `PathBuf`,不手写分隔符。
- **文件文本细节**:`file_edit` 检测并保留文件原行尾(CRLF/LF),diff 展示前归一化;UTF-8 读写,保留既有 BOM。
- **搜索**:内置实现(见第 6 节),不依赖外部 grep。
- **终端渲染**:ANSI 颜色经 `anstream` 输出(老式 conhost 自动降级)。
- **配置路径**:`dirs` crate——Windows `%APPDATA%\lex-code`,macOS `~/Library/Application Support/lex-code`,Linux `~/.config/lex-code`;项目级 `lex-code.toml` 以 CWD 优先。
- **中断**:Ctrl+C 走 tokio signal,终止正在执行的子进程,确认点安全退出。
- **提示词变量**:`{{OS}}` 填真实值 `windows` / `macos` / `linux`,模型据此选择命令语法。
- **明确不做**:chmod 权限位、符号链接特殊处理、进程组信号语义差异——遇到时按平台原样报错给模型。

## 9. 上下文、提示词与配置

- **AGENTS.md**:会话启动检测项目根目录,存在则读取并注入 system prompt 之后(一次组装,进入缓存前缀)。
- **Token 估算**:字符数 ÷ 4 本地启发式;跨过 `context.limit × 0.8` 阈值**只触发一次**压缩(不每轮计算);压缩 = 调当前 provider 对早期历史生成摘要,替换为一条摘要消息(保留关键决策、文件路径、未完成待办);完成后调用 `ImplicitPrefixCacheStrategy.invalidate()`(仅遥测,无恢复)。
- **系统提示词**:外部文件 `assets/coding-agent-system-prompt.md`,配置项可指定路径(默认二进制旁 assets/);启动读取一次;`{{CWD}}` / `{{OS}}` / `{{PROJECT_TYPE}}` / `{{TOOL_TODO}}` 以**变量名白名单 + 字符串定位替换**实现,除变量本身外其余文本逐字节不变;替换在全部工具定义注册完成后执行;禁止注入时间戳、随机数等每请求变化内容。
- **配置**:环境变量 > 项目级 `lex-code.toml` > 全局配置 > 内置默认。TOML 只放 base_url / model / 采样参数 / shell / 安全规则 / 缓存断点 / 上下文阈值,**不放任何凭证**。

## 10. 错误处理

- `lex-core`:`thiserror` 定义 `LexError` 及子模块错误;非测试代码禁止裸 `unwrap()` / `expect()`。
- `lex-cli`:`anyhow` 包装,输出用户可读信息;provider 网络错误、权限拒绝、工具失败如实呈现,不静默吞掉。

## 11. 测试策略

- **单测(任务书硬性要求)**:
  - `AnthropicCacheStrategy` 断点插入位置;`ImplicitPrefixCacheStrategy` 字节级前缀校验(system/tools 变化报不一致、历史改写报不一致、纯追加通过、invalidate 后基线重置)。
  - 权限检查器:Forbidden 优先于 Confirm、三级分类正确、执行路径必须经过检查器(绕过不可行)。
  - `reasoning_content` 回传:构造带 tool call 的多轮历史,断言 OpenAICompatibleAdapter 生成的 payload 原样携带。
- **Mock provider**:脚本化 SSE 回放,覆盖状态机转移、只读并发/副作用串行、权限确认分支。
- **真实冒烟**:Phase 1 用 Anthropic Key,Phase 2 起 DeepSeek Key,验证流式、工具双向转换、缓存命中率日志随轮次上升。

## 12. 分阶段交付(对齐任务书)

1. **Phase 1 — MVP 单 provider 闭环**:workspace 骨架、中立消息模型、AnthropicAdapter(流式)、Agent Loop 状态机、`file_read`/`file_edit`/`bash_exec`,权限全部走确认。验收:完成"读文件→改文件→跑测试"最小闭环。
2. **Phase 2 — Provider 泛化**:抽 `Provider` trait 落定、OpenAICompatibleAdapter 接 DeepSeek、`reasoning_content` 单测。验收:只改配置即切换 Claude/DeepSeek,行为一致。
3. **Phase 3 — 权限沙盒 + 剩余工具**:`grep_search` / `todo_write`、三级权限、Forbidden 拦截、只读并发。验收:Forbidden 命令被拦截并说明原因;只读工具同轮并发。
4. **Phase 4 — 上下文 + 缓存优化**:AGENTS.md、压缩触发、`ImplicitPrefixCacheStrategy` 确定性保证与命中率遥测。验收:长会话日志可见 `prompt_cache_hit_tokens` 上升,上下文不无限增长。
5. **Phase 5 — 打磨**:错误边界、可观测性完善、配置说明与使用文档。

**本期不做**:GUI/IDE 插件、多用户/服务化、CI/CD 集成、sub-agent 实现(仅预留接口)。
