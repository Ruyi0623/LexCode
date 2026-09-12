# Lex Code — Agent 指引

类 Claude Code 的 CLI 编程 agent(Rust)。两大差异化:可插拔多 provider(Anthropic 格式 + OpenAI 兼容格式)、DeepSeek 前缀缓存优化。当前完成 Phase 1(Anthropic 闭环),Phase 2-5 见下方路线图。

## 必读文档

- `docs/superpowers/specs/2026-09-12-lex-code-design.md` — 权威设计文档(架构、三级权限、缓存策略、三平台适配)
- `docs/requirements/harness-dev-brief-for-claude-code.md` — 原始需求任务书(分阶段交付与非功能硬约束)
- `examples/smoke/README.md` — 真实 API 冒烟步骤与联调结论
- `assets/coding-agent-system-prompt.md` — 运行时系统提示词(外部资源,**禁止**把内容硬编码进代码)

## 构建与测试

```bash
export PATH="$HOME/.cargo/bin:/d/mingw64/bin:$PATH"   # Git Bash 下通常需要
cargo test --workspace        # 全部测试(当前 41 个)
cargo test -p lex-core        # 仅核心库
cargo build --release -p lex-cli   # 产物在 D:/lexcode-target/release/lex-code.exe
```

工具链:rustup `stable-x86_64-pc-windows-gnu` + WinLibs gcc(`D:\mingw64\bin`)。cargo 走 rsproxy 镜像(`~/.cargo/config.toml`)。

## 环境坑点(重要)

- **仓库路径含中文**(`D:\项目\`),GNU dlltool 不兼容 → `.cargo/config.toml` 已把 target-dir 重定向到 `D:/lexcode-target`。**不要删除该配置**。
- 运行时按以下顺序查找系统提示词:配置 `system_prompt_path` → `<cwd>/assets/` → `<exe目录>/assets/`。对 release 二进制冒烟时需把 `assets/coding-agent-system-prompt.md` 复制到 exe 旁。
- 配置优先级:env(`LEX_*`)> 项目级 `lex-code.toml` > 内置默认。**Key 只从环境变量读**(`LEX_ANTHROPIC_API_KEY`),TOML 不放凭证;base_url/model 必须显式配置,代码零硬编码默认 URL/模型。

## 架构边界(依赖单向)

`lex-cli → lex-core`;core 内 `agent → provider/tools/security/context`,反向禁止。

- `lex-core/src/message.rs` — 中立消息模型(`Block::Thinking/ToolUse/ToolResult`),全项目唯一消息表示;adapter 不得丢弃或重排 Thinking 块。
- `lex-core/src/provider/` — `Provider` trait + `AnthropicAdapter`(SSE 流式)。`sse.rs` 是纯函数增量解析器。**改流式解析必须跑 `tests/sse_replay.rs`**(真实抓包回归,覆盖多种 chunk 切分)。
- `lex-core/src/tools/` — `Tool` trait + 注册表;`file_read.rs` 的 `require_str`/`resolve_path` 被其他工具复用。
- `lex-core/src/security.rs` — 权限确认回调。有副作用的工具执行**必须**经过 `execute_tool_call` 内部的权限检查,该路径不可被上层绕过;拒绝/失败降级为 `ToolResult{is_error}` 回填模型,不上抛。
- `lex-cli/src/` — 渲染与 UI;错误用 anyhow,提示词/交互输出用中文,anstream 输出 ANSI。确认与 REPL **共享同一个** `CliInput`(stdin BufReader 分开会丢预读输入)。

## 硬性约束(任务书规定,违反即返工)

- 非测试代码禁止裸 `unwrap()`/`expect()`(`unwrap_or` 等显式处理允许)。
- 进入请求 payload 的 serde 结构体:字段顺序 = derive 声明顺序;动态 JSON 用 `Value`/`Vec`,禁止 `HashMap`。
- 所有 provider HTTP 调用必须流式(SSE 逐事件),禁止攒完整响应。
- 多轮历史的 tool_result 必须合入**单条 user 消息**;DeepSeek 兼容端点要求 `thinking` 块原样回传(空 signature 可接受)。
- 路径一律 `PathBuf`;`file_edit` 保留文件原行尾(CRLF/LF);三平台(Linux/Windows/macOS)行为一致,bash_exec 平台默认 Unix `sh -c` / Windows `cmd /C`。
- Forbidden 级安全规则(删 `.git`、force push、敏感文件外发)用户配置不可静默覆盖。

## 路线图(后续 Phase)

- Phase 2:抽 Provider 泛化落定 + `OpenAICompatibleAdapter`(tool_calls 数组 + `role:"tool"`;**`reasoning_content` 回传必须有单测**)。
- Phase 3:`grep_search`(内置 ignore+regex,不调外部 grep)/`todo_write`、三级权限(Forebidden 正则规则表)、只读工具同轮并发。
- Phase 4:AGENTS.md 注入、token 阈值触发一次压缩、`ImplicitPrefixCacheStrategy`(字节级前缀校验 + `prompt_cache_hit_tokens` 遥测)。
- Phase 5:错误边界打磨、可观测性、配置文档。

不做:GUI/IDE 插件、服务化、CI/CD、sub-agent 实现(仅预留 `ToolRegistry` 扩展点)。
