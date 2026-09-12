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
