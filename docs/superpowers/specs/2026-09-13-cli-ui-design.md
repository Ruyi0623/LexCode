# Lex Code CLI 界面打磨(仿 Claude Code)— 设计文档

日期:2026-09-13
状态:已确认(用户逐节确认:全套仿 Claude Code、蓝色主题、crossterm 真编辑器、可打断不退出)

## 1. 目标与非目标

目标:把 lex-code 的终端界面打磨到 Claude Code 的观感与手感——品牌启动画面、蓝框输入盒(逐键编辑 + 历史)、工具活动行、思考流暗色渲染、token 尾注,外加"任务执行中 Ctrl+C 可打断不退出"。主题强调色为蓝色。

非目标(YAGNI,明确不做):

- 不引入 ratatui / alternate screen,不做全屏 TUI;保持终端滚动回看的内联范式。
- 不做 `/help` 等 slash 命令系统。
- 不改一次性任务模式(`lex-code 任务描述`)的输出样式;该模式不画 banner、不画输入盒。
- 不改权限确认的行式交互本质(必须继续走共享 `CliInput`,AGENTS.md 硬约束);仅做样式美化。

## 2. 总体架构

新增 `lex-cli/src/ui/` 模块(四个子模块),`render.rs` 重构迁入;`lex-core` 仅新增一个公开方法 `AgentLoop::recover_interrupt()`。依赖单向边界不变:`lex-cli → lex-core`,UI 改动不触碰 provider/security/context。

```
lex-cli/src/
  main.rs          组装:banner、交互循环(select! 竞速 run_turn 与 Ctrl+C)
  confirm.rs       确认弹窗样式美化(逻辑不变)
  ui/
    mod.rs         模块出口
    theme.rs       蓝色主题:全部 ANSI 颜色唯一定义处
    banner.rs      启动画面(仅交互模式)
    input.rs       crossterm 单行编辑器 + 圆角输入盒 + 历史记录
    events.rs      ProviderEvent 渲染:工具活动行 / 思考流 / 尾注 / 思考中占位
```

新依赖:`crossterm`(workspace 级,lex-cli only;features 按 event 读取所需最小集)。

## 3. 主题 `ui/theme.rs`

- 强调蓝:RGB(59, 130, 246);辅色:dim 灰、错误红、警告黄、成功绿。
- 暴露 `accent()/dim()/error()/warn()/success()` 等返回包裹后字符串的函数,以及原始 ANSI 前缀常量供逐 token 流式输出(流式场景不能整段包裹,需前缀 + 复位逐块输出)。
- 全部经 `anstream` 输出,老终端自动降级;代码中禁止 theme 之外出现裸 `\x1b[`。

## 4. 启动画面 `ui/banner.rs`

仅交互模式展示,一次性输出,不进入任何请求 payload:

- 蓝色 ██ 块字画 "LEX CODE" + `v0.1.0`(版本取自 Cargo pkg_version 或 clap version,取实现简单者)。
- 元信息行:provider、model、工作目录、项目类型(复用 main.rs 现有 `detect_project_type`)。
- dim 提示行:`? Ctrl+C 清空/退出 · Ctrl+D 退出 · ↑↓ 翻历史`。

## 5. 输入盒 `ui/input.rs`

- 渲染:蓝色圆角框 `╭─╮ │ │ ╰─╯`,宽度 = `min(终端宽 - 2, 80)`;内容行 `› ` + 文本;长文本水平滚动,视口跟随光标。
- 编辑能力:←→/Home/End 移动光标,Backspace/Delete,Ctrl+U 清行,↑↓ 在内存历史(本次会话输入,Vec<String>)中翻阅,Enter 提交。
- 退出语义:**Ctrl+C 输入非空 → 清空;输入已空 → 第二次 Ctrl+C 退出;Ctrl+D(EOF 语义)→ 退出**。
- raw mode 仅在等输入时启用,`InputGuard` 的 Drop 负责恢复(panic/错误路径也不残留 raw mode 与残帧)。
- TTY 探测:stdin 或 stdout 非 TTY(管道/重定向)时降级为现有 `CliInput::read_line` 行式读取,无框,仅 `› ` 前缀。
- 编辑器核心逻辑(插入/删除/光标移动/水平滚动视口计算)拆为纯函数 + 单测,不依赖终端。
- 确认弹窗(`? 允许执行 … [y/N]`)保持行式与共享 `CliInput`,样式改为问句蓝点前缀;不进 raw mode,避免破坏 stdin 缓冲约定。

## 6. 事件渲染 `ui/events.rs`

| 事件 | 渲染 |
| --- | --- |
| 请求发出、未收到首个 delta | dim `✻ 思考中…` 占位行;首 delta 到达即擦除该行 |
| `ThinkingDelta` | 暗色斜体流(现状保留) |
| `TextDelta` | 正文原色流(现状保留) |
| `ToolUseStart/Complete` | `● 工具名(参数摘要)` 蓝点行;参数摘要:bash 取命令、文件类取路径、grep 取 pattern,todo_write 取"更新待办" |
| 工具结果回填后 | 缩进 `⎿ 结果首行`(dim;is_error 用红 `⎿` + 错误首行)。工具结果当前不在 ProviderEvent 中,由 agent 在执行完成后以事件回调或渲染层旁路获取——实现取"渲染层旁路":main.rs 在收到 `ToolUseComplete` 时记录,工具执行在 confirm/render 侧已有回显路径,执行完成后由 CLI 捕获 ToolResult 内容首行(具体挂接点:在 `execute_tool_call` 返回后 CLI 无感知,故此信息经 `ProviderEvent` 之外新增 CLI 内部回调实现,不改动 lex-core 的 ProviderEvent 枚举) |
| `Completed` | dim `(输入 x tokens · 输出 y tokens · 缓存命中 z)` 尾注 |

若"工具结果首行"挂接实现中发现需要改 lex-core(如给 `run_turn` 加可选结果回调),允许新增带默认实现的方法,不破坏现有 `Provider` trait 与事件流语义。

## 7. 可打断:`lex-core::AgentLoop::recover_interrupt()`

- 交互循环:`tokio::select!` 竞速 `agent.run_turn(...)` 与 `ctrl_c`。Ctrl+C 取消本轮 future → 流被 drop(连接中断)、bash 子进程经 `kill_on_drop` 终止。
- 打断后 CLI 打印黄字 `⏹ 已中断本轮任务,可继续输入`,回到输入盒。
- `AgentLoop::recover_interrupt(&mut self)`(公开方法):扫描 history 尾部,移除悬空 tool_use——最后一条含 `Block::ToolUse` 的 assistant 消息若其后没有对应 tool_results user 消息,则从该消息剔除 `ToolUse` 块;剔除后该消息无任何块则整条删除。保证下轮请求对 Anthropic(工具对完整)与 DeepSeek(reasoning_content 连续性)都合法。
- 打断发生在 `pending_summary` 已置位但未并入时:摘要保留,下轮照常并入(无需特殊处理)。
- core 单测覆盖三种形态:①尾部轮次完整 → 不动;②尾部 assistant 含悬空 ToolUse、消息还有 text 块 → 仅剔除 ToolUse 块;③剔除后无块 → 整条删除。

## 8. 错误处理

- crossterm 初始化失败(如非 TTY):静默降级为行式输入,不报错退出。
- raw mode 恢复:Drop guard + `enable_raw_mode` 失败时的显式 `disable` 兜底。
- 渲染路径不 panic:所有颜色/擦行操作使用幂等 ANSI 序列;`Result` 全部显式处理,遵守"非测试代码禁止裸 unwrap/expect"硬约束。

## 9. 测试与验收

自动化:

- `ui/input.rs` 纯函数单测:插入/删除/光标/滚动窗口/历史游标。
- `lex-core` `recover_interrupt` 三形态单测。
- 全量 `cargo test --workspace` 回归(现有 111 个不得回退)。

手工冒烟清单(实施完成后逐项过):

1. 交互模式启动:banner 蓝色块字、元信息、提示行正确;一次性任务模式无 banner。
2. 输入盒:逐键编辑、↑↓ 历史、Ctrl+C 清空→再按退出、Ctrl+D 退出;管道 `echo 任务 | lex-code` 降级行式。
3. 工具活动行:bash_exec/file_read/file_edit/grep_search 参数摘要正确,错误结果红色 `⎿`。
4. 思考模式(DeepSeek thinking enabled):暗色斜体流;正文流正常分轨。
5. 长任务中 Ctrl+C:子进程被杀、无残留 raw mode、下轮请求不因悬空 tool_use 报 400。
6. 老式终端(可选):ANSI 降级不出现乱码。
