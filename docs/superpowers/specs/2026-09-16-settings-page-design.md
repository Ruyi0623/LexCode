# 设置页面(/settings)设计

> 日期:2026-09-16 · 状态:待评审 · 前置:无(不依赖 Phase 6 sub-agent/TUI 计划)

## 背景与目标

lex-code 目前所有配置(`lex-code.toml` + 环境变量)只能靠读文档和手改文件,REPL 内没有任何配置可视化入口。本设计在交互会话中新增一个 `/settings` 设置页面:先搭好页面骨架与四个占位模块,**只读展示当前真实配置快照**,编辑能力后续逐模块填充。

## 非目标(本期不做)

- 任何配置编辑/写回 `lex-code.toml`(占位模块只展示)。
- API Key 的显示或修改——硬约束:Key 只从环境变量读,TOML 不存凭证;页面只显示"来自环境变量 LEX_*",绝不回显值。
- TUI(ratatui)版本设置页;但数据模型按可复用设计,Phase 6 TUI 落地后直接搬。

## 载体与入口

- 交互会话主循环(`lex-cli/src/main.rs` 的 `interactive_session`)在输入提交后、喂给模型前做斜杠命令分发:
  - 精确匹配 `/settings` → 进入设置页,返回后继续主循环。
  - 其他 `/xxx` → 打印一行"未知命令,可用:/settings"后回输入盒。
  - 普通文本路径完全不变。
- 分发函数做成纯函数 `parse_command(&str) -> Option<Command>`(本期 `Command::Settings` 一个变体),为后续 `/clear`、`/help` 留扩展点。

## 页面结构与交互

- 新模块 `lex-cli/src/ui/settings.rs`,独立于 `ui/events.rs`(事件流渲染与全屏交互页生命周期不同,不混放)。
- 两级结构:
  1. **模块列表页**:清屏渲染四张模块卡片(标题 + 一行摘要),`↑/↓` 或数字 `1-4` 选择,`Enter` 进详情,`q`/`Esc` 返回 REPL。
  2. **模块详情页**:该模块的完整快照字段逐行展示,底部统一一行 `⚙ 该模块的编辑能力待实现(当前只读展示)`,`q`/`Esc` 返回列表页。
- 交互基建复用 `ui/input.rs` 既有件:`event_bus()`(单例按键读取线程)与 `RawGuard`(Drop 恢复 raw mode)从私有改为 `pub(crate)`;设置页短暂启用 raw mode,退出即恢复,不留残屏(整屏清除需在 `theme.rs` 新增常量后使用;raw mode 期间换行显式 `\r\n`)。
- 非 TTY(stdin/stdout 任一非 TTY,与 `read_input` 判定一致)自动降级:不打全屏页,直接按"模块标题 → 字段行"线性打印一次只读摘要——反正只读,无需交互。

## 数据快照 SettingsView

`lex-cli/src/ui/settings.rs` 内定义纯展示结构,一次性快照,不持有 `Config`/`AgentLoop` 引用:

```
模型与 Provider:provider(anthropic/openai)、模型名、base_url、max_tokens、
                openai.thinking / reasoning_effort(仅 openai 时显示)、API Key 来源提示
权限与安全:forbidden / confirm / auto 三级规则条数、
           敏感文件外发拦截("已启用")说明、三级权限模型一句话解释
上下文与压缩:limit、enabled、本会话是否已触发压缩、当前历史 token 本地估算
外观·日志·关于:蓝色主题说明、当前日志级别(LEX_LOG > RUST_LOG > warn)、
               版本(env!("CARGO_PKG_VERSION"))、项目类型、工作目录
```

- 构造:`SettingsView::from(config: &Config, runtime: SettingsRuntime)`;`SettingsRuntime { compress_attempted: bool, token_estimate: u64 }` 由 `interactive_session` 从 `AgentLoop` 两个 pub 字段(`compress_attempted`、`context::compress::estimate_tokens(&history)`)读取。
- 纯数据 + 纯渲染,lex-core 零改动(`estimate_tokens` 已是 pub)。

## 渲染约束(硬约束延续)

- 颜色/ANSI 只从 `ui/theme.rs` 取,settings.rs 禁止裸 `\x1b[`。
- raw mode 期间换行显式 `\r\n`。
- 提示词/文案全中文。
- 渲染写成纯函数:`render_list(&SettingsView, selected: usize) -> Vec<String>`、`render_detail(&SettingsView, module: usize) -> Vec<String>`,返回带主题色的行;终端集成层只负责清屏与逐行输出,不掺格式逻辑。

## 错误处理

- 设置页内按键通道关闭(读线程终止/EOF):直接返回 REPL,不 panic。
- raw mode 启用失败:打印一行错误后返回 REPL(与 `boxed_input` 的 `LexError::Io` 处理同风格)。
- 全程只读:任何失败路径都不触碰 `lex-code.toml` 与环境变量。

## 测试

- `parse_command`:匹配 `/settings`、未知 `/foo`、普通文本不误判、大小写敏感(`/Settings` 算未知)。
- `SettingsView::from`:Config 各段字段正确映射;openai 段 thinking 仅在 provider=openai 时填充。
- `render_list` / `render_detail`:输出行包含模块标题、关键字段值(如模型名、limit)、"待实现"占位行;不依赖终端。

## 后续演进(记录,不实现)

- 逐模块填充编辑能力(写回 `lex-code.toml` + 热生效),届时每模块一个"读-改-写"单元。
- Phase 6 TUI 落地后,`SettingsView` 与渲染纯函数直接复用为 TUI 设置页数据源。
