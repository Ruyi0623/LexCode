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
