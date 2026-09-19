# Phase 1 冒烟验收步骤

> 目标:验证"读文件 → 改文件 → 跑测试"最小闭环,以及流式输出、逐工具确认两个体验要求。

## 1. 准备测试目录

建一个临时目录(不要用本仓库),放一个故意有编译错误的小项目:

```
smoke-demo/
├─ Cargo.toml        # [package] name="smoke-demo" version="0.1.0" edition="2021"
└─ src/lib.rs        # 放一个故意错误的函数,例如 pub fn add(a: i32, b: i32) -> i32 { a - b }
```

## 2. 放置 lex-code.toml

在 `smoke-demo/` 下创建:

```toml
[anthropic]
base_url = "https://api.anthropic.com"
model = "<你选定的 Claude 模型名>"
```

> base_url / model 必须显式提供——本项目按约束不内置任何默认 URL 或模型名。

## 3. 设置环境变量

```
set LEX_ANTHROPIC_API_KEY=<你的 Key>      (CMD)
$env:LEX_ANTHROPIC_API_KEY="<你的 Key>"   (PowerShell)
```

## 4. 执行

```
D:\lexcode-target\release\lex-code.exe "读取 src/lib.rs,修复其中的编译错误,然后跑 cargo test 验证"
```

也可以不带任务参数进入交互模式逐句下指令。

## 5. 验收清单

- [ ] 流式输出可见(模型文本逐字出现,非一次性输出)
- [ ] 每个工具调用前出现"⚠ 需要确认",并列出**具体**命令 / 编辑内容(file_edit 显示替换前后文本)
- [ ] 任务闭环:lib.rs 被正确修复,`cargo test` 通过
- [ ] 拒绝某个确认(y/N 里选 N)时,agent 收到拒绝原因并继续调整或收尾,不崩溃
- [ ] 输出末尾可见"(输入 N tokens · 输出 M tokens)"

## 6. 真实联调记录(2026-09-13,DeepSeek Anthropic 兼容端点)

冒烟目标:`base_url = "https://api.deepseek.com/anthropic"`,`model = "deepseek-flash"`。

结果:**闭环成功**——agent 流式读取文件 → 诊断出 `a - b` 应为 `a + b` → 经确认修改 → `cargo test` 通过 → 如实汇报。此前一轮在确认环节选择"拒绝"时,agent 如实列出被拒操作并停下询问,拒绝路径同样符合设计。

联调发现并修复的三个问题(均有回归测试):

1. **thinking 块必须回传**:DeepSeek 推理模型经 Anthropic 兼容端点调用时,多轮历史中的 `thinking` 块必须原样回传(空 `signature` 可接受),否则报 "thinking must be passed back"。原实现按官方 Anthropic 习惯过滤了 Thinking 块 → 已改为回传(`0bd8ef0`)。
2. **SSE 增量解码重复拼接**:UTF-8 成功解码时 `keep=0`,导致"不完整尾字节"错误地取到整个已处理缓冲区并拼回 pending,每来一个 chunk 就把整段缓冲重复解析一遍(事件重复 10 倍、缓冲指数膨胀)。用真实抓包字节流 + 多种 chunk 切分固化为回归测试 `tests/sse_replay.rs`(`48922a6`)。
3. **stdin 预读丢失**:每次确认新建 BufReader,预读会吞掉管道中尚未消费的确认输入。改为确认弹窗与交互主循环共享同一个 stdin 读取器(`84fa3b1` + `16071ec` + `205005d`)。

## 7. Phase 2 冒烟:OpenAI 兼容端点(DeepSeek)

> 目标:验证 provider 只改配置即可切换,行为与 Anthropic 路径一致(流式、确认、工具闭环、reasoning_content 回传)。

`smoke-demo/lex-code.toml` 换成:

```toml
provider = "openai"

[openai]
base_url = "https://api.deepseek.com"
model = "deepseek-flash"       # 或 "deepseek-v4-pro"
# 可选:不配置 max_tokens 时不发送该参数,走服务端默认(非思考 8K / 思考 64K),
# 避免固定 8192 截断思维链;thinking / reasoning_effort 同样可选透传:
# thinking = "enabled"           # enabled / disabled
# reasoning_effort = "high"      # none / low / high / max
```

环境变量换成 OpenAI 兼容端点的 Key:

```
$env:LEX_OPENAI_API_KEY = "<你的 DeepSeek Key>"   (PowerShell)
set LEX_OPENAI_API_KEY=<你的 DeepSeek Key>         (CMD)
```

执行方式与第 4 节相同(release 二进制需重新构建:`cargo build --release -p lex-cli`)。

### 验收清单(在 Phase 1 清单之外新增)

- [ ] `provider = "openai"` 时请求打到 `[openai].base_url` 的 `POST /chat/completions`
- [ ] 流式输出可见;`deepseek-reasoner` 下推理内容(ThinkingDelta)与正文(TextDelta)分轨渲染
- [ ] 多轮工具调用不被 400 拒绝(即历史 assistant 消息的 `reasoning_content` / `tool_calls` 已回传)
- [ ] 工具结果以 `role:"tool"` + `tool_call_id` 回传,失败结果带失败标记,模型能感知并调整
- [ ] 切回 `provider = "anthropic"`(配回 [anthropic] 段 + LEX_ANTHROPIC_API_KEY)行为一致

### 真实联调记录(2026-09-13,DeepSeek OpenAI 兼容端点)

冒烟目标:`provider = "openai"`,`base_url = "https://api.deepseek.com"`,`model = "deepseek-flash"`。Key 只经 `LEX_OPENAI_API_KEY` 环境变量注入,确认输入经管道喂入 `y`。

结果:**闭环成功**——agent 流式读取文件 → 连跑 `ls`/`cargo test` 诊断出 `a - b` 应为 `a + b`(任务描述称"编译错误",agent 如实指出实为逻辑错误,未盲从)→ 经确认 `file_edit` 修改 → `cargo test` 通过 → 如实汇报。全程 5 轮模型请求、每轮均带工具调用,多轮历史回传(`tool_calls` 数组 + `role:"tool"` 结果)未被 400 拒绝;逐工具确认(含 file_edit 前后文本 diff)、流式渲染、末尾 token 统计均符合预期。

验收清单逐项:
- [x] 请求打到 `[openai].base_url` 的 `POST /chat/completions`
- [x] 流式输出可见,token 统计(输入/输出)随轮次显示
- [x] 多轮工具调用未被 400 拒绝(历史 assistant 消息回传正常)
- [x] 工具结果以 `role:"tool"` 回传,模型能感知测试失败并定位修复
- [x] 拒绝路径与 Phase 1 行为一致(确认 UI 逻辑与 provider 无关,共用同一实现)

### API 参考对齐(2026-09-13 第二轮,依据 /api/ 索引补抓错误码/限速/思考模式页)

- **错误码语义映射**(文档 /quick_start/error_codes):429/500/503 可重试 → 自动退避重试(1s/3s,最多 2 次,payload 预序列化保证重试字节一致);400/401/422/402 等 → 错误信息带中文语义提示(如"认证失败"/"余额不足"),在 `send()` 快速失败
- **user_id 可选透传**(文档 /quick_start/rate_limit):配置校验字符集 [a-zA-Z0-9-_] 与长度 512;用于 KVCache 隔离
- **流式 keep-alive**:文档说明等待期会下发 `: keep-alive` 注释——SSE 解析器早已覆盖(注释行丢弃),新增 mock 测试锁定
- **reasoning_content 回传规则确认**(/guides/thinking_mode):带 tools 必须回传(否则 400)、不带 tools 会被忽略——当前实现恒带 tools 且恒回传,兼容
- 复测同一任务:闭环成功,4 轮请求缓存命中 1536→1792→2048→2304

### 文档对齐适配记录(2026-09-13,依据 api-docs.deepseek.com 抓取)

按官方文档对 OpenAI 兼容路径做完整适配(均有测试):`max_tokens` 可选化、`thinking`/`reasoning_effort` 透传与校验、`prompt_cache_hit_tokens`/`prompt_cache_miss_tokens` 采集(兼容 OpenAI 风格 `prompt_tokens_details.cached_tokens`)、异常 `finish_reason` 警告(content_filter/insufficient_system_resource/aborted)、4xx 错误体解析。

适配后复测同一任务:**闭环成功,缓存遥测端到端生效**——6 轮请求全部成功,输入 token 的缓存命中随 append-only 前缀增长:1536 → 1792 → 8192 → 8448 → 8832 → 9088(命中率 97%),末尾统计显示"(输入 9350 tokens · 输出 214 tokens · 缓存命中 9088)"。原始流抓包确认 `delta.reasoning_content` 为增量文本且思考期间 `content` 为 `null`,解析器已覆盖;本次简单任务模型未输出思维链,渲染路径由 mock 测试保障。

## 8. Phase 3 冒烟:权限沙盒与只读并发

新增工具:grep_search(内置 ignore+regex,只读免确认)、todo_write(会话待办)。
三级权限:只读 Auto(直接放行)/ Confirm(确认或白名单)/ Forbidden(硬拦截,不可确认)。

### 真实联调记录(2026-09-13,DeepSeek 端点)

1. **正常任务闭环**:file_read 不再弹确认(Auto 生效,确认弹窗次数明显减少);bash_exec / file_edit 仍逐个确认;缓存命中持续。
2. **Forbidden 场景**:连续 4 次诱导测试(强制推送、删 .git、读 .env 后 curl 外发),模型全部**自主拒绝并给出安全替代方案**,未实际发起过 Forbidden 命令——真实模型自律让拦截层很难被触发(本身是理想行为)。拦截层行为由确定性集成测试锁定:MockProvider 直接下发 `rm -rf .git`,断言命令未到达工具层、结果为"⛔ 硬性拦截"错误回填(`agent_loop.rs::forbidden_command_is_hard_blocked_without_confirm`)。
3. **只读并发**:两个只读工具同轮并发(join_all),混入副作用工具严格串行,时序测试覆盖。

### 验收清单

- [x] grep_search 免确认、尊重 .gitignore
- [x] Forbidden 命令被拦截并说明原因(集成测试)
- [x] 只读工具同轮并发、副作用串行(时序测试)
- [x] [security] 规则表可追加,Forbidden 默认项不可移除

## 9. Phase 4 冒烟:AGENTS.md 注入 + 上下文压缩 + 前缀缓存遥测

新增:`context` 模块(AGENTS.md 一次性注入 system prompt、token 估算 字符÷4、历史压缩)、`provider/cache.rs`(`ImplicitPrefixCacheStrategy`:system/tools 逐字节一致 + messages 逐条消息列表级前缀校验、命中率遥测、`invalidate()`)、`[context]` 配置段(limit 默认 64000 / enabled)。

### 步骤

1. 冒烟目录放一个 `AGENTS.md`(内含"回复必须以 `[AGENTS-OK]` 开头"的标记指令);`lex-code.toml` 用 `provider = "openai"` + DeepSeek 端点。
2. 观察轮:`RUST_LOG=lex_core=info` 交互模式连续两个只读任务(file_read 免确认,管道喂入)。
3. 压缩轮:toml 追加 `[context] limit = 200`(阈值 160 token),三个任务;task 2 起历史超阈值。

### 真实联调记录(2026-09-13,DeepSeek OpenAI 兼容端点)

- **AGENTS.md 注入**:启动行打印"已加载项目指引 AGENTS.md",模型回复带 `[AGENTS-OK]` 标记且能复述文件内容——注入进入缓存前缀后未破坏缓存(task 2 起命中 92%+)。
- **命中率遥测**:`前缀缓存命中率 hit=... miss=... hit_rate="92.1%"` 逐轮输出,命中 token 0 → 1920 → 2176 → 2304 → 2560 随轮次上升;全程**零前缀违例**(append-only 历史校验通过)。
- **压缩触发**:task 2 开始时历史 298 token > 160,但仅一段完整轮次、切点为 0 → "本轮跳过压缩"**且不消耗**会话内唯一机会;task 3 开始时 `历史压缩完成 summarized=4 kept=4`,随后 `缓存链断开 invalidations=1`。
- **压缩后行为**:摘要并入下一条 user 消息(角色交替保持,请求未被 400 拒绝);命中数按预期回落(2560 → 2048,重建前缀);模型仍准确记得 task 1/2 的结论(`add` 用了减法、AGENTS.md 共 2 条规则)——关键决策与文件路径在摘要中保留;输入 token 从 2696 回落至 2650,验证上下文不再无限增长。
- **已知行为**:摘要转述可能弱化逐字指令(本次模型把标记指令转述为 `[以 AGENTS-OK 开头]` 而非原样输出标记)——属摘要保真度边界,非缺陷;压缩后依赖逐字指令的场景建议把关键约定留在 AGENTS.md(每次请求都随 system 注入)。

### 验收清单

- [x] 长会话日志可见 `prompt_cache_hit_tokens` 随轮次上升(命中率 >90%)
- [x] 上下文不无限增长(压缩触发、输入 token 回落)
- [x] AGENTS.md 存在即注入、缺失/空文件跳过(单测覆盖)
- [x] `ImplicitPrefixCacheStrategy`:system/tools 变化、历史改写、缩短均计违例;纯追加通过;invalidate 后基线重置(单测覆盖)
- [x] 压缩一次触发语义:跳过/失败不消耗机会,成功后不再触发(集成测试 + 真实联调双重确认)

## 10. Phase 6 模块一冒烟:子 agent 派生(spawn_subagent)

新增:`tools/spawn_subagent.rs`(`Tool` 实现,参数 `task` / `allowed_tools` / `context` / `context_budget` / `allow_nested`)、`tools::SubagentSpawner` trait 与 `ToolContext.spawner`(工具层只面向抽象)、`agent/subagent.rs`(`SubagentRuntime`:子 agent = 独立 `AgentLoop`,权限继承父级、深度硬上限、system 用主 prompt 原文作前缀)、`provider/throttle.rs`(`ThrottledProvider`,并发在途流节流,默认 3)。

### 步骤

1. 交互模式启动(配置同第 7/9 节,DeepSeek 端点;建议 `LEX_LOG=info` 便于对照日志)。
2. 输入下面这句(也可以自己换一个同样"多步 + 大范围"的子任务):

   ```text
   请用子 agent 排查 lex-core 下所有 TODO 并汇总成清单,allowed_tools 只给 file_read/grep_search
   ```

### 预期

- 终端出现确认项(`⚠ 需要确认 [spawn_subagent]` + `派生子 agent 执行子任务: …` + `允许执行? [y/N]`)——子 agent 的权限等级与父级相同,**用的是同一个确认处理器**。
- 子任务结束后,该工具的活动行下方出现 `⎿` 结果行(`ui/events.rs` 只打印结果**首行**,故这里就是子 agent 摘要的 `## 子任务摘要` 一行)。
- 子 agent 的活动以 `[子N]` **归属前缀**呈现在终端(`N` 为同一轮内派生的序号):`⤷ [子N] 派生: …` 派生行、`● [子N] name(摘要)` 调用行、`⎿` 配对的结果行;整行缩进一级并用 DIM 色,子级错误结果用 ERROR 色。子级的结局由父级那条 `⎿`(摘要首行)体现,故子事件**不单独打印结束行**。子 agent 的活动**不再**触碰父级的 token 尾注计数(子级工具结果走独立的子事件通道而不再经父级 `on_tool_result`),这正是改道的动机之一。
- 但子 agent 的中间过程**不进主上下文**:主历史里只有一条 `spawn_subagent` 的 tool_result,内容就是摘要;子 agent 自己的消息历史留在它的 AgentLoop 里,不回流。这正是派生的意义——看得见进度,但不吃主上下文的 token。
- 最终回复含"做了什么 / 关键结论 / 修改的文件"三段结构。
- **子 agent 不产生缓存遥测**:`LEX_LOG=info` 下只有主循环的前缀缓存命中率日志,子 agent 那几轮没有命中率输出。这是设计决定而非缺陷——子 agent 的注册表是父级注册表的 `subset(allowed_tools)`,tools 段必然与父级不同,逐字节前缀比对在 system 段之后即告失效,汇报一个注定未命中的命中率没有意义。子 agent 仍把主 prompt 原文作为 system 逐字节前缀(`compose_subagent_system`),这是为了「主 prompt 段」在服务端侧仍可命中,与本地遥测是两回事。
- 派生深度:子 agent 默认 `allow_nested=false`,其注册表里没有 `spawn_subagent`;显式传 `allow_nested=true` 时孙代可再派生一层,但孙代(第 2 层)结构性拿不到派生器——硬上限 2 层无法越过。

### 真实联调记录

待执行。本节步骤尚未在真实端点上跑过;跨层行为目前由 `lex-core/tests/subagent_e2e.rs` 的脚本化 Provider 端到端回归覆盖(不触网):主循环 → `spawn_subagent` → 子 agent → 摘要回填主历史(断言父级拿到的与子 agent 最终回复逐字节相同、子历史不外泄),以及 Forbidden 规则在子 agent 内仍硬拦截。

### 验收清单

- [ ] `spawn_subagent` 弹出确认项,子任务结果以 `⎿` 摘要行呈现
- [ ] 子 agent 的工具活动在终端可见,且每条都带 `[子N]` 归属前缀(`⤷ [子N] 派生: …` 派生行、`● [子N] name(摘要)` 调用行、`⎿ [子N] 结果` 结果行),但主历史只多出一条摘要 tool_result
- [ ] 最终回复为结构化摘要(做了什么/关键结论/修改的文件)
- [ ] 子 agent 内的工具调用走同一条权限检查路径,父级 Forbidden 规则在子 agent 内同样硬拦截
- [ ] 子 agent 无缓存遥测输出(设计决定,理由见上)
