# Lex Code

类 Claude Code 的终端编程 agent(Rust)。两大差异化:**可插拔多 provider**(Anthropic 格式 + OpenAI 兼容格式)与 **DeepSeek 前缀缓存优化**。

- 单一 Agent Loop,只改配置即可在 Claude 端点与 DeepSeek 等 OpenAI 兼容端点间切换
- 全链路 SSE 流式:思考、正文、工具调用增量渲染
- 仿 Claude Code 终端界面:蓝色主题启动画面、圆角输入盒(逐键编辑 / ↑↓ 历史)、工具活动行(`● 工具(参数)` + `⎿ 结果首行`)、任务执行中 Ctrl+C 可打断不退出
- 三级权限沙盒(Auto / Confirm / Forbidden),危险命令硬拦截不可绕过
- 内置 5 个工具:`file_read` / `file_edit` / `bash_exec` / `grep_search` / `todo_write`
- AGENTS.md 项目指引自动注入、上下文自动压缩、前缀缓存一致性校验与命中率遥测

## 构建

```bash
cargo build --release -p lex-cli   # 产物:target/release/lex-code(.exe)
cargo test --workspace             # 全量测试
```

运行时按以下顺序查找系统提示词文件 `assets/coding-agent-system-prompt.md`:
配置 `system_prompt_path` → `<工作目录>/assets/` → `<可执行文件目录>/assets/`。对 release 二进制做冒烟时,需把 `assets/coding-agent-system-prompt.md` 复制到 exe 旁。

## 快速开始

**1. 配置 API Key(只从环境变量读取,配置文件不放凭证)**

```bash
export LEX_ANTHROPIC_API_KEY=sk-...   # Anthropic 格式端点
# 或
export LEX_OPENAI_API_KEY=sk-...      # OpenAI 兼容端点(如 DeepSeek)
```

**2. 在项目根目录写一份 `lex-code.toml`**(最少只需 base_url + model)

```toml
# 方式一:Anthropic 格式端点
provider = "anthropic"
[anthropic]
base_url = "https://api.anthropic.com"
model = "claude-sonnet-4-5"
```

```toml
# 方式二:OpenAI 兼容端点(DeepSeek)
provider = "openai"
[openai]
base_url = "https://api.deepseek.com"
model = "deepseek-chat"
```

**3. 运行**

```bash
lex-code 修复 tests/foo.rs 里的失败用例   # 一次性任务,完成后退出
lex-code                                  # 交互模式(REPL)
lex-code -C D:/my/project 梳理项目结构     # 指定工作目录
```

交互模式提供仿 Claude Code 的终端界面:蓝色主题启动画面、圆角输入盒(`↑↓` 翻历史、`Ctrl+C` 清空/退出、`Ctrl+D` 退出)、工具活动行(`● 工具(参数)` + `⎿ 结果首行`)与 token 尾注;任务执行中按 `Ctrl+C` 可打断当前轮(自动清理历史,可直接继续)。管道/重定向(非 TTY)自动降级为行式输入。
- /settings:打开设置页面(只读快照,四模块:模型与 Provider / 权限与安全 / 上下文与压缩 / 外观·日志·关于;↑↓/数字选择,Enter 详情,q/Esc 返回;非交互终端自动降级为纯文本摘要)。配置编辑能力待后续版本。

## TUI 模式(全屏交互界面)

交互终端下默认启用 ratatui 全屏界面;`--plain` 强制纯文本流;stdin/stdout 任一非 TTY(管道/重定向)自动降级为行式输入。

```bash
lex-code            # 交互终端 → 全屏 TUI
lex-code --plain    # 强制纯文本流(与 Phase 5 行为一致)
lex-code | cat      # stdout 非 TTY → 自动降级纯文本
```

**布局**:左侧对话主输出区、右侧待办面板(`todo_write` 结果到达即刷新,`☐` 待办 / `◐` 进行中 / `☑` 已完成)、底部输入盒与状态行(左侧状态含思考片段,右侧 token 用量与缓存命中)。

**权限确认改为居中弹层**:`bash_exec` 展示**命令原文**,`file_edit` 展示带色 diff(增行绿、删行红);不再使用纯文本的 `[y/N]` 行式提问。

| 按键 | 作用 |
| --- | --- |
| `Enter` | 提交输入(空输入不提交) |
| `Ctrl+C` | 任务执行中:打断本轮(清理历史后可直接继续);空闲时:退出 |
| `Ctrl+U` | 清空输入盒 |
| `y` / `n` / `Esc` | 确认弹层:允许 / 拒绝 |

渲染跑在独立线程,与 agent 事件流经 channel 解耦(20ms 轮询排空事件后重绘),agent 不被渲染阻塞;退出时离开备用屏幕并关闭 raw mode。TUI 的按键、布局、diff 着色由 `TestBackend` 单测覆盖,交互式验收清单见 `examples/smoke/README.md` 第 11 节。

## 配置参考

优先级:**环境变量(`LEX_*`)> 项目级 `lex-code.toml`(工作目录)> 内置默认**。
`base_url` 与 `model` 必须显式提供(环境变量或 TOML),项目不内置任何默认 URL / 模型名。

| 字段 | 默认 | 说明 |
| --- | --- | --- |
| `provider` | `"anthropic"` | `"anthropic"` 或 `"openai"`(OpenAI 兼容端点) |
| `system_prompt_path` | 无 | 系统提示词文件路径;不填按上述顺序自动查找 |
| `max_turns` | `50` | 单轮任务内模型↔工具循环上限,超过报错终止 |
| `[anthropic].base_url` / `model` / `max_tokens` | 必填 / 必填 / `8192` | Anthropic 端点配置 |
| `[openai].base_url` / `model` | 必填 / 必填 | OpenAI 兼容端点配置 |
| `[openai].max_tokens` | 不发送 | 不设置走服务端默认(DeepSeek 非思考 8K / 思考 64K) |
| `[openai].thinking` | 不发送 | DeepSeek 思考模式:`"enabled"` / `"disabled"` |
| `[openai].reasoning_effort` | 不发送 | DeepSeek 思考强度:`"none"` / `"low"` / `"high"` / `"max"` |
| `[openai].user_id` | 不发送 | DeepSeek user_id(仅字母/数字/`-`/`_`,≤512 字符,勿含隐私) |
| `[shell].command` / `args` | 平台默认 | 覆盖 bash_exec 外壳。默认 Windows `cmd /C`,Unix `/bin/sh -c`;Windows 上可设 `command="bash", args=["-lc"]` 切 Git Bash |
| `[context].limit` | `64000` | 上下文 token 估算上限(本地按字符÷4 估算),超过 `limit × 0.8` 触发一次历史压缩 |
| `[context].enabled` | `true` | `false` 关闭上下文压缩 |
| `[agent].max_children_per_turn` | `4` | 每轮最多派生多少个子 agent;`0` 表示禁止派生 |
| `[security].forbidden` / `confirm` / `auto` | 内置默认 | 三级权限正则规则表(见下节),用户配置只能**追加** |

环境变量覆盖(优先于 TOML):`LEX_ANTHROPIC_BASE_URL`、`LEX_OPENAI_BASE_URL`。
API Key 环境变量:`LEX_ANTHROPIC_API_KEY` / `LEX_OPENAI_API_KEY`(同一段 DeepSeek Key 对两个端点通用时,两把都设为同一值即可)。

## 安全模型(三级权限)

每个有副作用的工具调用在执行前必经权限检查器,该路径不可被上层绕过;拦截/拒绝/失败都降级为错误 `ToolResult` 回填模型,不会中断会话。

- **Auto**:只读工具(`file_read` / `grep_search` / `todo_write`)直接放行
- **Confirm**:写文件、执行命令;需用户确认
- **Forbidden**:硬拦截,不接受任何确认

内置 Forbidden 默认规则:删除 `.git` 目录、`git push --force` 变体、`rm -rf` 高危目标、敏感文件(`.env`、`*.pem`、`id_rsa*` 等)读取后同轮出现网络外发命令(启发式)。内置规则用户配置**不可静默移除**;`[security]` 三段正则只能追加:

```toml
[security]
forbidden = ["terraform\\s+destroy"]   # 追加 Forbidden 规则(正则)
confirm = ["git\\s+rebase"]
auto = ["cargo\\s+(fmt|clippy)"]       # 只读判定外,额外放行的命令模式
```

子 agent(`spawn_subagent`)不构成权限旁路:

- **权限等级继承父级**:子 agent 使用与父级**同一份规则表 + 同一个确认处理器**,不允许通过任务描述或 `allowed_tools` 配置降级;子 agent 的工具调用走同一条 `execute_tool_call` 路径,父级 Forbidden 规则在子 agent 内同样硬拦截(回归见 `lex-core/tests/subagent_e2e.rs`)。
- **派生深度硬上限 2 层**:主循环为第 0 层,最多派生出子(1)与孙(2);`allow_nested` 只在未达上限时授予下一层派生器,孙代结构性拿不到 `spawn_subagent` 工具,无法越过上限。
- **每轮派生数量上限**:整棵派生树在主循环一轮内最多派生 `[agent] max_children_per_turn` 个子 agent(默认 `4`,`0` 表示禁止派生),越限的派生被拒绝;该上限也写进 `spawn_subagent` 的工具说明,模型据此自行合并任务。

## 上下文管理与 DeepSeek 前缀缓存

- 会话启动时检测项目根 `AGENTS.md`,存在则注入 system prompt(一次组装,进入缓存前缀后逐字节不变)
- 上下文估算超过 `context.limit × 0.8` 时,对早期历史生成一次摘要压缩,**会话内只触发一次**(失败不消耗机会);压缩摘要并入下一条用户消息,保持角色交替
- 压缩完成后向前缀缓存策略发"缓存链断开"信号(仅遥测记录,不做恢复)
- 前缀缓存校验:每轮请求将 system / tools / messages(逐条消息序列化)与基线做字节级前缀比对,历史被改写等破坏缓存的情况记 warning 与遥测计数,不阻断请求
- 缓存命中率从响应 Usage 采集(`prompt_cache_hit_tokens` / `prompt_cache_miss_tokens`,Anthropic 侧映射 `cache_read_input_tokens`),终端每轮显示,日志可查

## 日志与可观测性

日志全部写 stderr,不污染正文渲染。级别选择:`LEX_LOG` > `RUST_LOG` > 默认 `warn`。

```bash
LEX_LOG=info lex-code ...    # 观察工具耗时、缓存命中率、压缩、重试等
LEX_LOG=debug lex-code ...   # 追加 agent 状态机转移
```

关键日志事件:

| 事件 | 级别 |
| --- | --- |
| 工具执行完成(名称/耗时/是否错误) | info |
| 工具执行失败,降级为错误 ToolResult(属 agent 常规反馈) | debug |
| Forbidden 规则硬性拦截 | warn |
| 已读取敏感文件,同轮网络外发将被拦截 | warn |
| 服务端可重试错误自动退避(429/500/503) | warn |
| 前缀缓存一致性校验未通过(原因明细) | warn |
| 前缀缓存命中率(命中/未命中/命中率) | info |
| 历史压缩完成 / 压缩失败 | info / warn |
| 缓存链断开(压缩后基线重置) | warn |
| SSE 流结束时存在未解析残余 | warn |

错误处理边界:网络失败、SSE 解析异常、文件 IO、命令执行失败等均以 `Result` 显式处理;provider 端 4xx/5xx 带错误码语义提示(401 认证失败、402 余额不足、429 限速等)与自动退避重试;`lex-cli` 以完整错误链(`{:#}`)输出用户可读信息,不静默吞错。

## 测试

```bash
cargo test --workspace        # 全部测试(含真实抓包/mock 回归)
cargo test -p lex-core        # 仅核心库
```

流式解析改动必须跑 `lex-core/tests/sse_replay.rs` 与 `lex-core/tests/openai_sse_mock.rs`(真实抓包回放,覆盖多种 chunk 切分)。真实 API 冒烟步骤见 `examples/smoke/README.md`。

## 目录结构

```
lex-core/src/    核心库:message / provider / agent / tools / security / context / config / prompt
lex-cli/src/     终端 UI:main / confirm / ui(theme/banner/input/markdown/events/settings) / tui(diff/event/state/draw/confirm/run)
assets/          运行时系统提示词(外部资源,不硬编码进代码)
docs/            设计文档与需求任务书
examples/smoke/  真实 API 冒烟步骤与联调结论
```

## 当前状态

Phase 1–5 已完成:MVP 闭环、Provider 泛化(DeepSeek 真实冒烟,缓存命中 97%)、三级权限 + 只读并发、AGENTS.md 注入 + 上下文压缩 + 前缀缓存遥测、错误边界与可观测性打磨。

Phase 6 已完成:模块一 `spawn_subagent` 子 agent 派生机制(独立上下文的子 agent 复用同一 AgentLoop 状态机,权限继承父级、深度硬上限 2 层、并发派生受 provider 层节流,只把结构化摘要交回主对话);模块二 ratatui TUI(多区域布局、待办面板、权限确认弹层含命令原文/diff 着色、渲染线程与 agent 事件流 channel 解耦、`--plain` 与非 TTY 自动降级)。

明确不做:GUI / IDE 插件、多用户协作 / 服务化、CI/CD 集成。
