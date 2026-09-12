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
