# Lex Coding Harness — 开发任务书

> 本文件是提交给 Claude Code 的项目实现任务书，不是运行时 system prompt（运行时 prompt 见同批交付的 `coding-agent-system-prompt.md`，本项目需要把它作为外部资源加载并支持模板变量替换，不要把其内容硬编码进代码逻辑）。

## 项目目标

用 Rust 实现一个类 Claude Code 的 CLI 编程 agent harness。核心差异化目标：
1. **可插拔多 provider 架构**——同时兼容 Anthropic 消息格式和 OpenAI 兼容格式（DeepSeek / Kimi / GLM 等走后者），切换 provider 只改配置，不改 Agent Loop 代码。
2. **DeepSeek 磁盘前缀缓存专项优化**——通过确定性序列化和 append-only 历史管理，最大化缓存命中率并降低成本。

## 技术栈

- Rust，edition 2021 或更新
- 异步运行时：`tokio`
- CLI 解析：`clap`
- HTTP 客户端：`reqwest`，需要支持流式响应（SSE / chunked）
- 序列化：`serde` + `serde_json`——**凡是会进入模型请求 payload 的结构体，字段顺序必须显式可控**（用带序号的 derive 顺序或手写 Serialize，不要依赖 HashMap 的不确定顺序），这是保证 DeepSeek 前缀缓存生效的硬性要求
- 错误处理：`thiserror`（库内部错误类型）+ `anyhow`（应用层）
- 日志/可观测性：`tracing` + `tracing-subscriber`
- 可选：`ratatui`，如果后续要做比纯文本流更丰富的终端界面（MVP 阶段不需要）

## 整体架构（自顶向下五层，详见架构图）

1. CLI / 交互层——负责用户输入解析和流式输出渲染
2. Agent 核心循环——状态机：组装请求 → 调用 provider → 解析响应 → 执行工具 → 回填结果 → 循环
3. Provider 适配层（可插拔）——统一内部消息表示，双向转换到各 provider 的原生格式
4. 工具系统与执行沙盒——工具注册、执行、权限检查
5. 上下文与记忆管理——项目记忆文件加载、长会话压缩

## 详细模块需求

### 1. CLI / 交互层
- 支持单次任务模式（`harness "任务描述"`）和交互式会话模式
- 流式渲染模型输出，工具调用过程要有明确的视觉区分（比如"正在执行: xxx"）
- 需要确认的操作要清楚展示将要执行的具体内容，而不是笼统提示

### 2. Agent 核心循环
- 用状态机而不是简单的 for 循环实现，状态至少包括：等待输入、组装请求、等待模型响应、执行工具、等待权限确认
- 只读工具调用允许在同一轮内并发执行，有副作用的工具必须串行
- 需要支持"自主多轮执行直到完成或需要用户介入"的模式，不是每轮都必须停下来等确认

### 3. Provider 适配层（本项目的核心模块，优先级最高）
- 定义统一的 `Provider` trait，抽象出 `send_request(ctx) -> Stream<AgentEvent>` 这样的接口，内部消息用你自己定义的中立结构体，不直接暴露任何 provider 原生类型给上层
- 至少实现两个 adapter：
  - `AnthropicAdapter`：system 独立字段，tool_use/tool_result content block，支持显式 `cache_control` 断点插入（system prompt 结尾、工具定义结尾、历史倒数第二轮结尾这几个位置可配置）
  - `OpenAICompatibleAdapter`：tool_calls 数组 + role:tool 消息；DeepSeek / Kimi / GLM 复用同一个 adapter，provider 间的细微差异（比如是否支持某个参数）用配置项区分，不要为每个都单独写一个 adapter
- `OpenAICompatibleAdapter` 必须正确处理 DeepSeek 推理模型的 `reasoning_content`：一旦某轮出现了工具调用，后续请求要把完整的 `reasoning_content` 原样带回去，缺失会导致 API 返回 400——这个逻辑要有单元测试覆盖，因为是纯正确性问题，不是优化项
- 定义 `CacheStrategy` trait，每个 adapter 持有自己的实现：
  - `AnthropicCacheStrategy`：在指定断点插入 `cache_control: {type: "ephemeral"}` 标记
  - `ImplicitPrefixCacheStrategy`（DeepSeek 用）：不插入任何标记，而是在请求组装阶段做前缀稳定性保证——校验 system prompt/工具定义序列化结果和上一次请求的对应部分逐字节一致，历史消息只允许追加不允许改写；从响应的 `usage` 字段里读取 `prompt_cache_hit_tokens` / `prompt_cache_miss_tokens` 并记录到 `tracing` 里，方便后续观察命中率
- API Key / base_url / 模型名通过配置文件（建议 TOML）或环境变量注入，代码里不允许出现任何硬编码的凭证或默认 URL

### 4. 工具系统
- 核心工具集：`file_read`、`file_edit`（基于 diff/patch 定位替换，不做整体覆盖）、`bash_exec`、`grep_search`、`todo_write`
- 每个工具实现同一个 `Tool` trait，`schema()` 方法要能同时序列化成 Anthropic 工具定义格式和 OpenAI function 定义格式
- 工具注册表设计成可扩展的，为后续加 sub-agent 派生工具留好接口，但本期不需要实现 sub-agent 本身

### 5. 权限与安全沙盒
- 三级权限模型：`Auto`（只读操作，直接放行）、`Confirm`（写文件/执行命令，需要用户确认或匹配白名单规则）、`Forbidden`（硬拦截，不接受任何确认）
- `Forbidden` 规则至少要覆盖：删除 `.git` 目录、`git push --force`、对敏感文件（`.env`、私钥文件等常见命名模式）的读取后外发
- 危险命令识别用可配置的正则规则表，不要写死在逻辑里，方便后续调整
- 每一个有副作用的工具调用在执行前都要经过权限检查器，这一步不能被上层逻辑绕过

### 6. 上下文与记忆管理
- 会话开始时检测项目根目录是否存在 `AGENTS.md`，存在则读取并注入到 system prompt 之后
- 维护当前上下文的 token 估算，接近配置的阈值时触发摘要压缩（不要每轮都算一次压缩，只在跨过阈值时触发一次）
- 压缩操作执行后，要向 `ImplicitPrefixCacheStrategy` 发一个"缓存链已断开"的信号，用于遥测记录，不需要做特殊恢复逻辑

### 7. 系统提示词加载
- 从外部文件加载 system prompt（对应 `coding-agent-system-prompt.md`），启动时读取一次
- 实现 `{{VAR}}` 模板变量替换：`CWD`、`OS`、`PROJECT_TYPE`、`TOOL_TODO` 等，替换逻辑要保证除了这些变量本身，其余文本逐字节不变
- 变量替换必须在所有工具定义组装完成之后执行，且不允许把任何每次请求都会变化的内容（时间戳、随机数）混入这一层，否则会从这里开始打断 DeepSeek 的前缀缓存

## 建议分阶段交付

### Phase 1 — MVP 单 provider 闭环
只接入 Anthropic 一种格式，实现最基础的 CLI 输入输出 + Agent Loop + 三个工具（`file_read` / `file_edit` / `bash_exec`），权限先全部走确认（不区分三级）。
验收标准：能完成"读一个文件、按要求修改、跑一下测试"这种最小闭环任务。

### Phase 2 — Provider 适配层泛化
抽出 `Provider` trait，新增 `OpenAICompatibleAdapter` 并接入 DeepSeek，验证工具调用格式双向转换正确，`reasoning_content` 回传逻辑有测试覆盖。
验收标准：同一套 Agent Loop 代码，只改配置就能在 Claude 和 DeepSeek 之间切换，行为一致。

### Phase 3 — 权限沙盒 + 剩余工具
补齐 `grep_search` / `todo_write`，实现三级权限模型和危险命令拦截。
验收标准：能正确拦截 `Forbidden` 级别命令并说明原因，只读工具能在同一轮内并发执行。

### Phase 4 — 上下文管理 + DeepSeek 缓存优化落地
实现 `AGENTS.md` 加载、上下文压缩触发、`ImplicitPrefixCacheStrategy` 的确定性序列化保证和命中率遥测。
验收标准：长会话压测下能在日志里观察到 `prompt_cache_hit_tokens` 随对话轮次上升，且上下文长度不会无限增长。

### Phase 5 — 打磨
补齐错误处理边界情况、完善日志与可观测性、整理配置文件说明和使用文档。

## 非功能性要求

- 所有 provider 相关的 HTTP 调用都要支持流式响应，禁止先攒完整响应再一次性返回
- 所有可能失败的外部调用（网络请求、文件 IO、子进程执行）必须用 `Result` 显式处理，非测试代码中不允许裸用 `unwrap()` / `expect()`
- `CacheStrategy` 和权限检查器这两个模块需要单元测试覆盖，因为它们分别对应正确性（不能因为缓存策略把请求发错）和安全性（不能因为权限逻辑漏洞执行危险命令）的核心保证

## 本期明确不做

- 图形化界面 / IDE 插件集成
- 多用户协作或远程服务化部署
- CI/CD 自动化集成
- sub-agent 派生机制的具体实现（只需预留接口）
