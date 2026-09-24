# Lex Code — Agent 指引

类 Claude Code 的 CLI 编程 agent(Rust)。两大差异化:可插拔多 provider(Anthropic 格式 + OpenAI 兼容格式)、DeepSeek 前缀缓存优化。Phase 1(Anthropic 闭环)、Phase 2(OpenAI 兼容 adapter)、Phase 3(三级权限 + grep_search/todo_write + 只读并发)、Phase 4(AGENTS.md 注入 + 上下文压缩 + 前缀缓存校验/遥测)均已完成并真实联调;Phase 5(错误边界、可观测性、配置文档)已完成;Phase 6 模块一(sub-agent 派生机制)、模块二(ratatui TUI 全屏界面)均已完成,见路线图。此外 `/settings` 设置页已落地(REPL 内斜杠命令,只读配置快照,四模块,非 TTY 降级)。

目录:`lex-core/src/` 核心库(message / provider / agent / tools / security / context / config / prompt)、`lex-cli/src/` 终端 UI(main / confirm / ui:theme / banner / input / markdown / events / settings / tui)、`docs/` 设计文档与需求任务书、`examples/smoke/` 真实联调步骤、`assets/` 运行时系统提示词、`tests/`(位于各 crate)按真实抓包/mock 固化回归。

## 必读文档

- `docs/superpowers/specs/2026-09-12-lex-code-design.md` — 权威设计文档(架构、三级权限、缓存策略、三平台适配)
- `docs/requirements/harness-dev-brief-for-claude-code.md` — 原始需求任务书(分阶段交付与非功能硬约束)
- `README.md` — 使用与配置文档(lex-code.toml 全字段、环境变量、三级权限、LEX_LOG 可观测性)
- `examples/smoke/README.md` — 真实 API 冒烟步骤与联调结论
- `assets/coding-agent-system-prompt.md` — 运行时系统提示词(外部资源,**禁止**把内容硬编码进代码)

## 构建与测试

```bash
export PATH="$HOME/.cargo/bin:/d/mingw64/bin:$PATH"   # Git Bash 下通常需要
cargo test --workspace        # 全部测试(当前 214 个)
cargo test -p lex-core        # 仅核心库
cargo build --release -p lex-cli   # 产物在 D:/lexcode-target/release/lex-code.exe
```

工具链:rustup `stable-x86_64-pc-windows-gnu` + WinLibs gcc(`D:\mingw64\bin`)。cargo 走 rsproxy 镜像(`~/.cargo/config.toml`)。

测试夹具若用固定临时目录(如 `lex-core/src/config.rs` 的 `tests::temp_dir(tag)` → `%TEMP%\lex-config-test-{tag}`,且先 `remove_dir_all`),**新测试必须用互不相同的 tag** —— 共用 tag 会在并行跑测试时互相删目录(本项目据此出过 flake)。

**`cargo test` 不重建可执行产物**(它只建测试 harness):改动后要对二进制冒烟,必须先 `cargo build --release -p lex-cli`,否则跑的是**旧二进制**(本项目据此误跑过一次冒烟);debug 的 `lex-code.exe` 同理不随 `cargo test` 刷新。

## 环境坑点(重要)

- **本文件会被自身产品加载**:lex-code 启动时把项目根 `AGENTS.md` 注入模型系统提示词(`context/agents_md.rs`)。改这里 = 改运行时模型行为;冒烟时模型会读它,措辞要当"给模型的指令"对待。
- **本文件与 `CLAUDE.md` 必须逐字节相同**:两者是同源副本(`CLAUDE.md` 是 Claude Code 读的那份,`AGENTS.md` 是产品运行时注入的那份),由 `lex-core/tests/docs_consistency.rs` **强制**——只改一份会让 `cargo test` 直接失败。**改完 `AGENTS.md` 后把整份内容覆盖写入 `CLAUDE.md`**(不要手工在两份里各改一遍,行尾/空格差异即失败),可用 `Get-FileHash AGENTS.md, CLAUDE.md -Algorithm SHA256` 自证。
- **仓库路径含中文**(`D:\项目\`),GNU dlltool 不兼容 → `.cargo/config.toml` 已把 target-dir 重定向到 `D:/lexcode-target`。**不要删除该配置**。
- 运行时按以下顺序查找系统提示词:配置 `system_prompt_path` → `<cwd>/assets/` → `<exe目录>/assets/`。对 release 二进制冒烟时需把 `assets/coding-agent-system-prompt.md` 复制到 exe 旁。
- 配置优先级:env(`LEX_*`)> 项目级 `lex-code.toml` > 内置默认。**Key 只从环境变量读**(`LEX_ANTHROPIC_API_KEY` / `LEX_OPENAI_API_KEY`),TOML 不放凭证;base_url/model 必须显式配置,代码零硬编码默认 URL/模型。本机已在用户级环境变量永久配置这两把 Key(同一把 DeepSeek Key,对 OpenAI 端点与 Anthropic 兼容端点通用),新终端可直接跑冒烟。**注意环境变量只对"新起的进程"生效**:已在长驻的进程里(如 agent harness、旧 shell)看不到后配的 Key —— 冒烟若报"缺少 API Key",用 `$env:LEX_OPENAI_API_KEY = [Environment]::GetEnvironmentVariable('LEX_OPENAI_API_KEY','User')` 显式注入,**绝不打印其值**。

## 架构边界(依赖单向)

`lex-cli → lex-core`;core 内 `agent → provider/tools/security/context`,反向禁止。

- `lex-core/src/provider/cache.rs` — `CacheStrategy` trait + `ImplicitPrefixCacheStrategy`(前缀缓存一致性校验 + 命中率遥测,两 provider 通用)。**messages 前缀比对必须逐条消息序列化后做列表级比对**——整体 JSON 数组序列化会因结尾 `]` 永远不构成字节前缀。压缩成功必须调用 `invalidate()` 重置基线。
- `lex-core/src/context/` — AGENTS.md 加载注入(`agents_md.rs`,一次性组装进 system prompt 缓存前缀)、token 估算(字符÷4,只计历史消息)与历史压缩(`compress.rs`,摘要器提示词允许内置)。
- `lex-core/src/message.rs` — 中立消息模型(`Block::Thinking/ToolUse/ToolResult`),全项目唯一消息表示;adapter 不得丢弃或重排 Thinking 块。
- `lex-core/src/provider/` — `Provider` trait + `AnthropicProvider` / `OpenAiCompatProvider`(均 SSE 流式;切换只改配置 `provider = "anthropic"|"openai"`)。HTTP 客户端只设 connect_timeout(30s)防连接无界阻塞,**不设读取超时**(SSE 长流不能被总超时截断)。OpenAI 路径按 DeepSeek 官方文档完整适配:`max_tokens`/`thinking`/`reasoning_effort`/`user_id` 可选透传,Usage 采集 `prompt_cache_hit_tokens`/`prompt_cache_miss_tokens`(Anthropic 侧映射 `cache_read_input_tokens`);两 provider 共用 `post_stream_with_retry`(429/500/503 自动退避,payload 预序列化保证重试字节一致;错误信息带错误码语义提示)。`sse.rs` 是纯函数增量解析器。**改流式解析必须跑 `tests/sse_replay.rs` 与 `tests/openai_sse_mock.rs`**(真实抓包/mock 回归,覆盖多种 chunk 切分)。
- `lex-core/src/agent/mod.rs` — AgentLoop 状态机;`on_tool_result: Option<ToolResultHook>` 工具结果回调(CLI 渲染 `⎿` 结果行用,无订阅者零开销);`recover_interrupt()` 供 Ctrl+C 打断后调用——清除历史尾部悬空 tool_use(无对应 tool_result),否则下轮请求被两端点 400 拒绝。
- `lex-core/src/tools/` — `Tool` trait + 注册表;6 个内置工具 file_read/file_edit/bash_exec/grep_search/todo_write/spawn_subagent;`file_read.rs` 的 `require_str`/`resolve_path` 被其他工具复用;`grep_search` 是内置 ignore+regex 实现(不调外部 grep);只读工具同轮 join_all 并发,混入副作用严格串行(`agent/mod.rs`)。**spawn_subagent 必须经 `SpawnSubagent::new(max_children_per_turn)` 构造**(不再是单元结构):每轮派生上限写进它的工具 description 供模型自知,**禁止**在别处再硬编码一份上限文案或散落的默认值。**sub-agent 的依赖方向**:`tools/` 只定义 `SubagentSpawner` trait 与 `ToolContext.spawner`(面向抽象),实现在 `agent/subagent.rs`(`SubagentRuntime`)——`agent → tools` 单向不变,依赖不从 tools 反向指回 agent。
- `lex-core/src/agent/subagent.rs` — `SubagentRuntime`(`SubagentSpawner` 实现)。子 agent = 独立 `AgentLoop`(独立历史/todos/`SecurityGuard`,但**规则表与 handler 从父级 clone,权限不高于父级**);注册表 = `ToolRegistry::subset(allowed_tools)`,spawn_subagent 永不随 `allowed_tools` 传入;深度硬上限 `MAX_SPAWN_DEPTH = 2`(主 0 → 子 1 → 孙 2),只有显式 `allow_nested` 且未到上限才给子级派生器,孙代结构性拿不到;system = 主 prompt 原文逐字节前缀 + 任务限定追加在末尾(`compose_subagent_system`)。子 agent `cache_strategy: None`——子注册表是 `subset`,tools 段必然与父级不同,前缀比对失去意义,故子 agent 不产生缓存遥测;派生只把 `run_turn` 的最终文本(结构化摘要)交回父级,子历史留在子 AgentLoop 内。
- `lex-core/src/security/` — 三级权限(Auto/Confirm/Forbidden)。检查器内置于 `execute_tool_call` 执行路径,上层不可绕过;内置 Forbidden 默认规则(删 .git / force push / rm -rf 高危目标)用户配置**不可静默移除**;敏感文件读取后同轮网络外发命令启发式硬拦截(状态每轮 `reset_turn`)。`[security]` 段三级正则数组只能追加。有副作用的工具执行**必须**经过 `execute_tool_call` 内部的权限检查,该路径不可被上层绕过;拒绝/失败降级为 `ToolResult{is_error}` 回填模型,不上抛。
- `lex-cli/src/` — 渲染与 UI;错误用 anyhow,提示词/交互输出用中文,anstream 输出 ANSI。确认与 REPL **共享同一个** `CliInput`(stdin BufReader 分开会丢预读输入)。UI 硬约束:所有颜色/ANSI 序列只从 `ui/theme.rs` 取,其他文件禁止裸 `\x1b[`;raw mode 期间换行必须显式 `\r\n`(`\n` 不回车,输入盒边框会错位);流式正文经 `ui/markdown.rs` 行缓冲渲染(粗体/斜体/行内代码/标题/列表/围栏,纯函数有单测,改渲染先跑 `editor_tests`/markdown 测试);提交输入盒 = 折叠(留 `› 回执`),退出 = 整盒清除。`ui/settings.rs` 是 `/settings` 设置页:`parse_command` 斜杠分发(唯一前缀命中,如 `/setting`)→ `SettingsView` 只读快照 → 渲染/状态机纯函数可单测;非 TTY 自动降级线性摘要;Key 只显示"环境变量 LEX_*",绝不回显;按键复用 `input.rs` 的 `event_bus()`/`RawGuard`。`tui/` 是 ratatui 全屏交互界面(`diff`/`event`/`state`/`draw`/`confirm`/`run`,交互终端下默认启用,`--plain` 与非 TTY 自动降级到上述纯文本路径);TUI 硬约束:**颜色只取 `theme.rs` 的 `ratatui::style::Color` 常量(`C_*`),同样禁止裸 `\x1b[`**;**确认弹层必须展示命令原文或 diff,禁止"是否继续?"式笼统提示**;`run_tui` 的待办句柄**只能从 `agent.tool_ctx.todos` 派生**(另建 `Arc` 会让面板永远空白且单测发现不了);TUI 模式下 `build_loop` **不得注入纯文本渲染钩子**(工具/子 agent 活动行会往 stdout 打 ANSI 破坏画面);状态变更只在 UI 线程发生(`AppState::apply`),渲染与 agent 事件流经 channel 解耦。

## 硬性约束(任务书规定,违反即返工)

- 非测试代码禁止裸 `unwrap()`/`expect()`(`unwrap_or` 等显式处理允许)。
- 进入请求 payload 的 serde 结构体:字段顺序 = derive 声明顺序;动态 JSON 用 `Value`/`Vec`,禁止 `HashMap`。
- 所有 provider HTTP 调用必须流式(SSE 逐事件),禁止攒完整响应。
- 多轮历史的 tool_result 必须合入**单条 user 消息**;DeepSeek 兼容端点要求 `thinking` 块原样回传(空 signature 可接受),OpenAI 兼容端点要求 assistant 历史 `reasoning_content` 原样回传(缺失 400)。
- 路径一律 `PathBuf`;`file_edit` 保留文件原行尾(CRLF/LF);三平台(Linux/Windows/macOS)行为一致,bash_exec 平台默认 Unix `sh -c` / Windows `cmd /C`。
- Forbidden 级安全规则(删 `.git`、force push、敏感文件外发)用户配置不可静默覆盖。
- 提交用**显式 `git add <文件>`**,**禁止 `git add -A`**;提交信息用**中文 conventional commits**(如 `feat(agent): …` / `fix(cli): …` / `docs: …` / `test(agent): …`)。

## 配置:子 agent 派生预算(`[agent]` 段)

```toml
[agent]
max_children_per_turn = 4   # 每轮最多派生多少个子 agent;0 = 禁止派生(不是"无限")
```

- **口径是"整棵派生树在主循环一轮内获准占名额的派生数"**,不是父子各自计数,也不是"派生尝试数":`SpawnState::try_admit` 先加后判,**只有越限被拒的那次回滚不计入**;获准后子 agent 自身失败的那次**仍计入**(名额在准入那一刻即被消耗,否则失败的子 agent 可以无限重试绕过上限)。根级 `run_turn` 起始清零,子 agent 的轮次**不清零**父级计数。
- 上限值与配置项名同时出现在拒绝文案里、并写进 `spawn_subagent` 的工具 description(模型据此自行合并任务,避免反复撞墙)。
- 改上限时只改配置:上限从构造期传入 `SpawnSubagent::new(...)`,不要在代码里另留一份默认值。改 `description` 文案会变更请求 payload 的 tools 段,使 DeepSeek 前缀缓存**失效一次**。

## 路线图(后续 Phase)

- ~~Phase 2:抽 Provider 泛化落定 + `OpenAICompatibleAdapter`~~(已完成并通过 DeepSeek 端点真实冒烟,见 `examples/smoke/README.md` 第 7 节)。
- ~~Phase 3:grep_search / todo_write、三级权限、只读并发~~(已完成,见 `examples/smoke/README.md` 第 8 节)。
- ~~Phase 4:AGENTS.md、压缩触发、`ImplicitPrefixCacheStrategy`~~(已完成并通过 DeepSeek 端点真实冒烟,见 `examples/smoke/README.md` 第 9 节)。压缩要点:阈值 `context.limit × 0.8`、**会话内只成功触发一次**(无可切分历史/摘要失败不消耗机会)、摘要并入下一条 user 消息(保持角色交替,Anthropic 端点要求)、`[context]` 段 limit(默认 64000)/enabled 可配。
- ~~Phase 5:错误边界打磨、可观测性、配置文档~~(已完成:provider 客户端 connect_timeout 防无界阻塞;`execute_tool_call` 记录工具耗时/结果日志;日志级别 `LEX_LOG` > `RUST_LOG` > warn(仅 stderr,不污染渲染);根目录 `README.md` 覆盖配置全字段、环境变量、安全模型、日志事件表)。
- ~~Phase 6 模块一:sub-agent 派生机制~~(已完成,计划 `docs/superpowers/plans/2026-09-13-phase6-subagent.md`)。要点:`ToolRegistry` 已改 Arc 存储并新增 `names`/`subset`;`Tool` trait 已加 `parallel_safe`(并发判定与只读语义解耦);`ThrottledProvider` 在 provider 层节流并发在途流(默认 3,许可持有至流结束);`spawn_subagent` 已进注册表,`build_loop` 已改为注入 handler/todos/spawner;跨层回归见 `lex-core/tests/subagent_e2e.rs`(主循环派发 + 只回摘要、Forbidden 规则在子 agent 内仍拦截)。
- ~~派生预算 + 结构化子事件(Phase 6 模块一加固)~~(已完成,计划 `docs/superpowers/plans/2026-09-19-subagent-budget-and-child-events.md`)。要点:新增 `[agent] max_children_per_turn`(默认 4,0 = 禁止派生),计数口径为**整棵派生树在主循环一轮内获准占名额的派生数**(`SpawnState::try_admit` 先加后判、越限回滚,故被拒的不计入、获准后子 agent 自身失败仍计入,根级 `run_turn` 起始清零);上限写进 `spawn_subagent` 工具 description 与拒绝文案(模型据此合并任务);子 agent 的工具活动改走独立的 `ChildEvent` 通道(不再经父级 `on_tool_result`,故**不触碰父级的 token 尾注计数**),CLI 以 `⤷ [子N] 派生` / `● [子N] 工具` / `⎿ 结果` 带归属前缀渲染(缩进一级 + DIM 色,子级错误用 ERROR 色),子 agent 的结局由父级那条 `⎿`(摘要首行)体现;`Started` 与 `Finished` 成对发射(失败路径也发 `Finished`)。
- ~~Phase 6 模块二:ratatui TUI~~(已完成,计划 `docs/superpowers/plans/2026-09-13-phase6-tui.md`)。要点:多区域布局(对话主区 / 待办面板 / 输入盒 / 状态行)+ 权限确认居中弹层(`bash_exec` 命令原文、`file_edit` 带色 diff);渲染跑独立线程、与 agent 事件流经 channel 解耦(20ms 轮询排空后重绘),UI→agent 用 tokio mpsc、agent→UI 用 std mpsc,确认裁决经 `oneshot` 回执;Ctrl+C 打断走 `tokio::select!`(select 的 future 在语句结束即析构,故 `recover_interrupt()` 必须放在 select **之后**调用,否则 `&mut agent` 借用冲突);`--plain` 与非 TTY 自动降级,单任务模式恒为纯文本;`detect_project_type` 改 `pub(crate)` 供状态行复用。**TUI 的交互验收需真实终端,清单见 `examples/smoke/README.md` 第 11 节**(自动化部分:非 TTY 降级 + 单任务纯文本路径已真实联调,按键/布局/diff 着色由 `TestBackend` 单测覆盖)。
- `/settings` 设置页(已完成):规格 `docs/superpowers/specs/2026-09-16-settings-page-design.md`,计划 `docs/superpowers/plans/2026-09-16-settings-page.md`;后续按模块填充编辑能力(写回 lex-code.toml + 热生效)。

不做:GUI/IDE 插件、服务化、CI/CD(任务书明确:若启动需独立任务书,不与 sub-agent/TUI 混批)。
