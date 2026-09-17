# Phase 6 · 模块一:Sub-agent 派生机制 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 主 Agent Loop 能通过 `spawn_subagent` 工具把独立子任务派发给拥有独立上下文的子 agent 执行,子 agent 只把结构化摘要返回主循环;权限继承父级、派生深度硬上限 2 层、多子任务并发派生且 Provider 层并发节流。

**Architecture:** 复用现有 `AgentLoop` 状态机作为子 agent 执行体(不另起一套逻辑)。依赖方向保持 `agent → tools/security/provider` 单向:`tools/mod.rs` 定义中立的 `SubagentSpawner` trait 与 `SubagentRequest`,`agent/subagent.rs` 的 `SubagentRuntime` 实现该 trait 并在内部构造子 `AgentLoop`;`tools/spawn_subagent.rs` 的 `SpawnSubagent` 工具只面向 trait,不感知 agent 模块。子 agent system prompt = 主 prompt 逐字节前缀 + 末尾追加任务范围限定(保住隐式前缀缓存)。

**Tech Stack:** Rust(tokio / async-trait / futures,均为现有依赖,不新增 crate)。

## Global Constraints(摘自 AGENTS.md,每个任务隐含遵守)

- 非测试代码禁止裸 `unwrap()`/`expect()`(`unwrap_or` 等显式处理允许;`unwrap_or_else(|p| p.into_inner())` 处理 Mutex 允许)。
- 进入请求 payload 的 serde 结构体字段顺序 = derive 声明顺序;动态 JSON 用 `Value`/`Vec`,禁止 `HashMap`。
- 所有 provider HTTP 调用必须流式;本计划不触碰 HTTP 序列化路径。
- 路径一律 `PathBuf`。
- 架构边界:`lex-core` 内 `agent → provider/tools/security/context`,反向禁止。`tools/` 不得引用 `agent/` 下任何类型。
- Forbidden 级安全规则不可被绕过:子 agent 必须走 `execute_tool_call` 同一条权限路径。
- 提示词/交互输出用中文。
- 构建测试命令:先 `export PATH="$HOME/.cargo/bin:/d/mingw64/bin:$PATH"`,再 `cargo test --workspace`(当前 111 个测试,不允许回归)。
- 每次 commit 前跑相关测试;commit 信息用中文 conventional commits(参照 `git log`)。

---

## 开工前已知偏差(2026-09-17 核对,先读本节再动手)

> 对全文约 75 处「关于现存代码」的断言逐条比对真实仓库:约 57 处一致,**20 处不符,其中 7 处阻塞**。以下按「不修就做不下去」排序。本节优先于下文正文。

### 阻塞项

1. **跨 crate 编译断链(最关键)**:Task 3 给 `ToolContext` 加 `spawner` 字段后,`lex-cli/src/main.rs:127` 立即编译失败——它在**生产函数 `build_loop` 内,不是测试代码**。而计划要到 Task 7 才改 main.rs。因此 **Task 3/4/5/6 各自 Step 4 的「`cargo test --workspace` 全部 PASS」不可达**。
   - 处理:Task 3~6 的验收改用 `cargo test -p lex-core`;或在 Task 3 就一并补 `main.rs:127`(则 Task 7 相应缩小)。
2. **`ToolContext` 字面量实为 13 处,不是下文所写的 10 处**,且含 1 处生产代码。完整清单:
   `lex-core/src/agent/mod.rs:279`、`security/mod.rs:190`、`tools/{grep_search:130, todo_write:88, bash_exec:93, file_read:47, file_edit:72}`、`tests/agent_loop.rs:65,126,158,199`、`tests/context_flow.rs:77`、**`lex-cli/src/main.rs:127`**。
3. **Task 6 的 `depth_cap_enforced_defensively` 测试与实现互斥**:测试用**空注册表** + `allowed_tools:["file_read"]`,断言错误文本含「上限」;但实现(L923-934)是**先查未知工具、后查深度** → 空注册表下 `file_read` 先被判为未知工具,断言永不成立。
   - 处理:测试改用**非空注册表**(先注册 `file_read`)再验深度;或调换实现的校验顺序,并同步改 `unknown_allowed_tool_is_rejected`。
4. **两处 `.boxed()` 缺 import**:Task 6 的 subagent.rs 测试(L686)与 Task 8 的 e2e(L1161)都用了 `.boxed()`,但 import 列表无 `futures::StreamExt` → E0599。对照 `lex-core/tests/sse_mock.rs:1` 等三处均为显式 import。
5. **e2e 裸 `Result` 未绑定**(L1159):import 列表只有全限定写法,`-> Result<StreamResult>` 的 `Result` 无来源 → E0412。
6. **`AGENTS.md` 已不存在**:Task 8 要求修改它,但该文件已从工作区删除(内容迁至未跟踪的 `CLAUDE.md`)。注意 `context/agents_md.rs:9` 只读 `cwd/AGENTS.md`,**新建它会被运行时重新注入进模型系统提示词**——这是有副作用的动作,不是纯文档编辑,动手前先定基准文件。
7. **`git add -A` 会误提**:工作区已存在未提交的 `D AGENTS.md` / `?? CLAUDE.md`(重命名),`git add -A` 会把它一并卷入本计划的提交。改为显式 `git add <具体文件>`。

### 语义失真(能编译,但方向错)

- **Task 2 与运行时提示词冲突**:`parallel_safe()` 意在让非只读工具并发,但 `assets/coding-agent-system-prompt.md:30` 明确对模型说「有副作用的操作(写文件、执行命令)必须串行执行」。Task 8 的文档清单未列这条同步,不改则模型仍按旧规则自我约束。
- **「保住隐式前缀缓存」无代码路径支撑**:子 agent 用 `subset()` 注册表,tools 段必然与父级不同(`provider/cache.rs:88-90` 逐字节比对即判「tools 段变化」);且子 AgentLoop 设了 `cache_strategy: None`,连 `prepare` 都不调用。故 L1266 要求的「子 agent 缓存遥测」与 L971 的 `None` 不可兼得;验收表(L1292)相关表述应删除或改为「不适用」。
- **todos 复用自相矛盾**:Task 7 称把 todos 提升到 `run()` 层注入 runtime,Task 6 实现却给子 loop 新建 `Arc::new(Mutex::new(Vec::new()))`,结构体 `todos` 字段全程不被读取。
- **任务编号错位 4 处**:L139 把 Task 4 的产出记作 Task 6;L33 把 Task 6 记作 Task 7;L305、L478 把 Task 7 的 main.rs 装配记作 Task 8。
- **反向依赖未标注**:Task 4 的 `tool_metadata` 测试消费 Task 2 的 `parallel_safe`,Interfaces 未标出。

### 基线

- 测试总数**实为 140**(Global Constraints 里写的 111、L1273 的「≥115」均为旧数)。本次核对时基线全绿。

---

### Task 1: ToolRegistry 内部改 Arc 存储,新增 `names()` / `subset()` / Clone

**Files:**
- Modify: `lex-core/src/tools/mod.rs`
- Test: `lex-core/src/tools/mod.rs`(`mod tests`)

**Interfaces:**
- Produces: `impl Clone for ToolRegistry`;`pub fn names(&self) -> Vec<String>`;`pub fn subset(&self, allowed: &[String]) -> ToolRegistry`(保持插入顺序,按工具名过滤)。`register`/`get`/`definitions` 对外签名不变。
- 消费方:Task 7 的 `SubagentRuntime`(构建子注册表)。

- [ ] **Step 1: 写失败测试**

在 `lex-core/src/tools/mod.rs` 的 `mod tests` 末尾追加:

```rust
    #[test]
    fn subset_filters_by_name_and_keeps_order() {
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(Dummy));
        reg.register(Box::new(FileRead));
        let sub = reg.subset(&["file_read".to_string()]);
        let names = sub.names();
        assert_eq!(names, vec!["file_read".to_string()]);
        assert!(sub.get("dummy").is_none());
        // subset 与原注册表互不影响
        assert!(reg.get("dummy").is_some());
    }

    #[test]
    fn names_lists_all_in_insertion_order() {
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(Dummy));
        reg.register(Box::new(FileRead));
        assert_eq!(reg.names(), vec!["dummy".to_string(), "file_read".to_string()]);
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p lex-core tools::tests::subset -- --nocapture`
Expected: FAIL,报 `names`/`subset` 方法不存在。

- [ ] **Step 3: 最小实现**

`lex-core/src/tools/mod.rs` 中,`ToolRegistry` 改为(注意 `get` 里 `as_ref()`):

```rust
#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: Vec<Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        ToolRegistry { tools: Vec::new() }
    }

    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.push(Arc::from(tool));
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.iter().find(|t| t.name() == name).map(|t| t.as_ref())
    }

    pub fn names(&self) -> Vec<String> {
        self.tools.iter().map(|t| t.name().to_string()).collect()
    }

    /// 按 allowed 工具名过滤出子注册表(保持插入顺序;工具实例经 Arc 共享,无重复构建)
    pub fn subset(&self, allowed: &[String]) -> ToolRegistry {
        ToolRegistry {
            tools: self
                .tools
                .iter()
                .filter(|t| allowed.iter().any(|a| a == t.name()))
                .cloned()
                .collect(),
        }
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .iter()
            .map(|t| ToolDefinition { name: t.name().to_string(), description: t.description().to_string(), input_schema: t.schema() })
            .collect()
    }
}
```

(原 `#[derive(Default)]` 换成 `#[derive(Clone, Default)]`;`new()` 保留。`Box<dyn Tool> → Arc<dyn Tool>` 用 `Arc::from`,无需改任何调用方。)

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p lex-core`
Expected: lex-core 全部 PASS。

- [ ] **Step 5: Commit**

```bash
git add lex-core/src/tools/mod.rs
git commit -m "refactor(tools): ToolRegistry 改 Arc 存储并新增 names/subset(为子 agent 注册表过滤铺路)"
```

---

### Task 2: Tool trait 新增 `parallel_safe()`,并发判定改用它

**Files:**
- Modify: `lex-core/src/tools/mod.rs:67`(Tool trait)
- Modify: `lex-core/src/agent/mod.rs:143`(并发判定)
- Test: `lex-core/src/tools/mod.rs`(`mod tests`)

**Interfaces:**
- Produces: `trait Tool { ... fn parallel_safe(&self) -> bool { self.read_only() } }`。Task 6 的 `SpawnSubagent` 覆写为 `true`。
- 语义:默认与 `read_only()` 一致,现有 5 个工具行为零变化。

- [ ] **Step 1: 写失败测试**

`mod tests` 追加:

```rust
    #[test]
    fn parallel_safe_defaults_to_read_only() {
        assert!(Dummy.parallel_safe());          // Dummy read_only = true
        assert!(!super::file_edit::FileEdit.parallel_safe()); // FileEdit read_only = false
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p lex-core tools::tests::parallel_safe`
Expected: FAIL,`parallel_safe` 不存在。

- [ ] **Step 3: 最小实现**

`Tool` trait(`lex-core/src/tools/mod.rs`)追加方法(带默认实现,放在 `read_only` 之后):

```rust
    /// 是否可与同轮其他工具并发执行。默认与 read_only 一致;
    /// spawn_subagent 覆写为 true(子任务在独立上下文内执行,多个独立子任务允许并发派生)。
    fn parallel_safe(&self) -> bool {
        self.read_only()
    }
```

`lex-core/src/agent/mod.rs` 的 `run_turn` 中,并发判定改用(变量名同步改为 `all_parallel`):

```rust
                let all_parallel = tool_uses.iter().all(|(_, name, _)| {
                    self.registry.get(name).map(|t| t.parallel_safe()).unwrap_or(false)
                });
                if all_parallel && tool_uses.len() > 1 {
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --workspace`
Expected: 全部 PASS(并发路径行为对现有工具不变)。

- [ ] **Step 5: Commit**

```bash
git add lex-core/src/tools/mod.rs lex-core/src/agent/mod.rs
git commit -m "feat(tools): Tool trait 新增 parallel_safe,并发判定与只读语义解耦(子 agent 并发派生前提)"
```

---

### Task 3: SubagentSpawner trait + SubagentRequest + ToolContext.spawner 字段

**Files:**
- Modify: `lex-core/src/tools/mod.rs`(新增 trait 与请求结构,ToolContext 加字段)
- Modify(机械更新,`ToolContext` 字面量补 `spawner: None`):`lex-core/src/agent/mod.rs:279`、`lex-core/src/tools/grep_search.rs:130`、`lex-core/src/tools/todo_write.rs:88`、`lex-core/src/tools/bash_exec.rs:93`、`lex-core/src/security/mod.rs:190`、`lex-core/src/tools/file_read.rs:47`、`lex-core/src/tools/file_edit.rs:72`、`lex-core/tests/agent_loop.rs:65,126,158,199`、`lex-core/tests/context_flow.rs:77`
- Test: `lex-core/src/tools/mod.rs`(`mod tests`)

**Interfaces:**
- Produces:
  - `pub struct SubagentRequest { pub task: String, pub allowed_tools: Vec<String>, pub context: Option<String>, pub context_budget: Option<u32>, pub allow_nested: bool }`
  - `#[async_trait] pub trait SubagentSpawner: Send + Sync { async fn spawn(&self, req: SubagentRequest) -> crate::error::Result<String>; }`
  - `ToolContext` 新增 `pub spawner: Option<Arc<dyn SubagentSpawner>>`(Default 为 None)。
- 消费方:Task 4(SpawnSubagent 读取 `ctx.spawner`)、Task 6(实现 trait)。

- [ ] **Step 1: 写失败测试**

`mod tests` 追加(验证默认 None、trait 可用对象化):

```rust
    struct NullSpawner;
    #[async_trait::async_trait]
    impl SubagentSpawner for NullSpawner {
        async fn spawn(&self, _req: SubagentRequest) -> crate::error::Result<String> {
            Ok("子任务摘要".into())
        }
    }

    #[tokio::test]
    async fn tool_context_spawner_defaults_to_none_and_is_callable() {
        let ctx: ToolContext = ToolContext::default();
        assert!(ctx.spawner.is_none());
        let ctx2 = ToolContext { cwd: PathBuf::from("."), shell: None, todos: Default::default(), spawner: Some(std::sync::Arc::new(NullSpawner)) };
        let s = ctx2.spawner.as_ref().unwrap().spawn(SubagentRequest {
            task: "t".into(), allowed_tools: vec![], context: None, context_budget: None, allow_nested: false,
        }).await.unwrap();
        assert_eq!(s, "子任务摘要");
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p lex-core tools::tests::tool_context_spawner`
Expected: FAIL,`SubagentSpawner`/`SubagentRequest`/字段不存在。

- [ ] **Step 3: 最小实现**

`lex-core/src/tools/mod.rs` 在 `ToolContext` 定义前追加:

```rust
/// 子 agent 派生请求(spawn_subagent 工具输入的规范化形态)
#[derive(Debug, Clone)]
pub struct SubagentRequest {
    pub task: String,
    pub allowed_tools: Vec<String>,
    /// 父级传入的必要上下文片段(并入子 agent 首条任务消息)
    pub context: Option<String>,
    /// 子 agent 上下文 token 上限(缺省继承父级)
    pub context_budget: Option<u32>,
    /// 是否允许子 agent 再派生下一层(默认 false;深度硬上限见 agent/subagent.rs)
    pub allow_nested: bool,
}

/// 子 agent 派生器抽象:由 agent 层实现,工具层只面向该抽象(保持 agent → tools 单向依赖)。
#[async_trait::async_trait]
pub trait SubagentSpawner: Send + Sync {
    /// 执行子任务,返回子 agent 的结构化摘要(绝不返回子 agent 的完整消息历史)。
    async fn spawn(&self, req: SubagentRequest) -> crate::error::Result<String>;
}
```

`ToolContext` 增加字段(保持 Default 派生):

```rust
#[derive(Debug, Clone, Default)]
pub struct ToolContext {
    pub cwd: PathBuf,
    pub shell: Option<ShellCommand>,
    /// 会话级待办清单(todo_write 的存储;不落盘,生命周期同 AgentLoop)
    pub todos: Arc<Mutex<Vec<Todo>>>,
    /// 子 agent 派生器;None = 当前环境不允许派生(深度达上限/未装配)
    pub spawner: Option<Arc<dyn SubagentSpawner>>,
}
```

然后按上面文件清单,给全部 10 处测试内 `ToolContext { ... }` 字面量统一补一行 `, spawner: None`(均在测试代码内,机械改动)。`ToolContext` 带 `Debug` 派生但字段是 trait 对象——`Option<Arc<dyn SubagentSpawner>>` 不满足 Debug,把 `ToolContext` 的 `Debug` 派生去掉(全仓库无 `{:?}` 打印 ToolContext 的调用点,已核对)。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --workspace`
Expected: 全部 PASS。

- [ ] **Step 5: Commit**

```bash
git add lex-core
git commit -m "feat(tools): 新增 SubagentSpawner trait 与 ToolContext.spawner(工具层面向抽象,agent 层实现)"
```

---

### Task 4: SpawnSubagent 工具(schema/输入解析/错误降级)

**Files:**
- Create: `lex-core/src/tools/spawn_subagent.rs`
- Modify: `lex-core/src/tools/mod.rs`(加 `pub mod spawn_subagent;`)
- Modify: `lex-core/src/security/mod.rs`(`describe_call` 补 `spawn_subagent` 分支)
- Test: `lex-core/src/tools/spawn_subagent.rs`(`mod tests`)

**Interfaces:**
- Consumes: `ToolContext.spawner`、`SubagentRequest`(Task 3)、`crate::tools::file_read::require_str`(现有 pub 工具函数,解析字符串参数)。
- Produces: `pub struct SpawnSubagent;` 实现 `Tool`:name=`"spawn_subagent"`,`read_only()=false`,`parallel_safe()=true`。
- 消费方:Task 6(child 注册表按需注册)、Task 8(main.rs 注册)。

- [ ] **Step 1: 写失败测试**

`lex-core/src/tools/spawn_subagent.rs`:

```rust
use crate::error::{LexError, Result};
use crate::tools::{SubagentRequest, SubagentSpawner, Tool, ToolContext};
use serde_json::{json, Value};

/// spawn_subagent:把独立、边界清晰的子任务派发给独立上下文的子 agent。
/// 返回值只有子 agent 的结构化摘要,不带回子 agent 的完整消息历史(控制主上下文体积的关键)。
pub struct SpawnSubagent;

#[async_trait::async_trait]
impl Tool for SpawnSubagent {
    fn name(&self) -> &str {
        "spawn_subagent"
    }
    fn description(&self) -> &str {
        "把一个独立、边界清晰的多步骤子任务派发给拥有独立上下文的子 agent 执行,只返回结构化摘要。仅当子任务的执行长度/探索成本明显高于污染主上下文的代价时使用;琐碎单步操作不要派生。"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["task", "allowed_tools"],
            "properties": {
                "task": {"type": "string", "description": "子任务描述,需自包含(子 agent 看不到主对话)"},
                "allowed_tools": {"type": "array", "items": {"type": "string"}, "description": "允许子 agent 使用的工具名列表"},
                "context": {"type": "string", "description": "子任务需要的必要上下文片段(可选)"},
                "context_budget": {"type": "integer", "description": "子 agent 上下文 token 上限(可选,缺省继承父级)"},
                "allow_nested": {"type": "boolean", "description": "是否允许子 agent 再派生下一层(默认 false,硬上限共 2 层)"}
            }
        })
    }
    fn read_only(&self) -> bool {
        false
    }
    fn parallel_safe(&self) -> bool {
        true
    }
    async fn execute(&self, input: Value, ctx: &ToolContext) -> Result<String> {
        let spawner = ctx.spawner.as_ref().ok_or_else(|| {
            LexError::Tool("当前环境不允许派生子 agent(已达派生深度上限或未启用)".into())
        })?;
        let task = crate::tools::file_read::require_str(&input, "task")?;
        let allowed_tools: Vec<String> = input
            .get("allowed_tools")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        if allowed_tools.is_empty() {
            return Err(LexError::Tool("allowed_tools 不能为空:请列出子 agent 可用的工具名".into()));
        }
        let context = input.get("context").and_then(Value::as_str).map(str::to_string);
        let context_budget = input.get("context_budget").and_then(Value::as_u64).map(|v| u32::try_from(v).unwrap_or(u32::MAX));
        let allow_nested = input.get("allow_nested").and_then(Value::as_bool).unwrap_or(false);
        spawner
            .spawn(SubagentRequest { task, allowed_tools, context, context_budget, allow_nested })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Recorder {
        requests: Mutex<Vec<SubagentRequest>>,
        reply: String,
        err: Option<String>,
    }
    #[async_trait::async_trait]
    impl SubagentSpawner for Recorder {
        async fn spawn(&self, req: SubagentRequest) -> Result<String> {
            self.requests.lock().unwrap_or_else(|p| p.into_inner()).push(req);
            if let Some(e) = &self.err {
                return Err(LexError::Tool(e.clone()));
            }
            Ok(self.reply.clone())
        }
    }

    fn ctx_with(rec: Arc<Recorder>) -> ToolContext {
        ToolContext { cwd: std::path::PathBuf::from("."), shell: None, todos: Default::default(), spawner: Some(rec) }
    }

    #[tokio::test]
    async fn parses_input_and_returns_summary() {
        let rec = Arc::new(Recorder { reply: "## 子任务摘要\n- **做了什么**:x".into(), ..Default::default() });
        let out = SpawnSubagent.execute(
            json!({"task":"排查失败","allowed_tools":["file_read"],"context":"模块在 src/","context_budget":16000,"allow_nested":true}),
            &ctx_with(rec.clone()),
        ).await.unwrap();
        assert!(out.starts_with("## 子任务摘要"));
        let reqs = rec.requests.lock().unwrap_or_else(|p| p.into_inner());
        assert_eq!(reqs[0].task, "排查失败");
        assert_eq!(reqs[0].allowed_tools, vec!["file_read".to_string()]);
        assert_eq!(reqs[0].context.as_deref(), Some("模块在 src/"));
        assert_eq!(reqs[0].context_budget, Some(16_000));
        assert!(reqs[0].allow_nested);
    }

    #[tokio::test]
    async fn missing_spawner_is_error() {
        let ctx = ToolContext { cwd: std::path::PathBuf::from("."), shell: None, todos: Default::default(), spawner: None };
        let err = SpawnSubagent.execute(json!({"task":"t","allowed_tools":["file_read"]}), &ctx).await.unwrap_err();
        assert!(err.to_string().contains("不允许派生"), "实际: {err}");
    }

    #[tokio::test]
    async fn empty_allowed_tools_is_error() {
        let rec = Arc::new(Recorder::default());
        let err = SpawnSubagent.execute(json!({"task":"t","allowed_tools":[]}), &ctx_with(rec)).await.unwrap_err();
        assert!(err.to_string().contains("allowed_tools"), "实际: {err}");
    }

    #[test]
    fn tool_metadata() {
        assert_eq!(SpawnSubagent.name(), "spawn_subagent");
        assert!(!SpawnSubagent.read_only());
        assert!(SpawnSubagent.parallel_safe());
    }
}
```

注:`require_str(&input, "task")` 若现有签名不同(以 `lex-core/src/tools/file_read.rs` 实际为准),按实际签名调用;返回类型为 `Result<String>`。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p lex-core spawn_subagent`
Expected: FAIL,模块不存在。

- [ ] **Step 3: 实现(上面 Step 1 的代码即最终实现)**

`lex-core/src/tools/mod.rs` 顶部加 `pub mod spawn_subagent;`。`lex-core/src/security/mod.rs` 的 `describe_call` 补分支:

```rust
        "spawn_subagent" => {
            let t = input.get("task").and_then(Value::as_str).unwrap_or("<未知任务>");
            format!("派生子 agent 执行子任务: {t}")
        }
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --workspace`
Expected: 全部 PASS。

- [ ] **Step 5: Commit**

```bash
git add lex-core
git commit -m "feat(tools): spawn_subagent 工具(参数解析/无派生器降级为错误 ToolResult/并发安全标记)"
```

---

### Task 5: ThrottledProvider(Provider 适配层并发节流)

**Files:**
- Create: `lex-core/src/provider/throttle.rs`
- Modify: `lex-core/src/provider/mod.rs`(加 `pub mod throttle;`)
- Test: `lex-core/src/provider/throttle.rs`(`mod tests`)

**Interfaces:**
- Consumes: `Provider` trait、`StreamResult`(现有)。
- Produces:
  - `pub struct ThrottledProvider { .. }`,`Clone`,`pub fn new(inner: std::sync::Arc<dyn Provider>, max_concurrent: usize) -> Self`,实现 `Provider`。
  - `pub const MAX_CONCURRENT_STREAMS: usize = 3;`
- 语义:许可从流创建起持有到流结束(或流被提前 drop,Ctrl+C 打断即释放);多个实例共享同一 `Arc<Semaphore>` 即可全局节流。消费方:Task 6(子 agent provider)、Task 8(main.rs)。

- [ ] **Step 1: 写失败测试**

```rust
use crate::error::Result;
use crate::provider::{Provider, RequestContext, StreamResult};
use futures::StreamExt;
use std::sync::Arc;
use tokio::sync::Semaphore;

/// 并发请求数节流:同一时刻最多 max_concurrent 条在途 SSE 流,
/// 防止并发子 agent 瞬间打满 API 速率限制。许可持有到流结束(或流被 drop)。
#[derive(Clone)]
pub struct ThrottledProvider {
    inner: Arc<dyn Provider>,
    permits: Arc<tokio::sync::Semaphore>,
}

pub const MAX_CONCURRENT_STREAMS: usize = 3;

impl ThrottledProvider {
    pub fn new(inner: Arc<dyn Provider>, max_concurrent: usize) -> Self {
        ThrottledProvider { inner, permits: Arc::new(tokio::sync::Semaphore::new(max_concurrent)) }
    }
}

#[async_trait::async_trait]
impl Provider for ThrottledProvider {
    async fn send(&self, ctx: RequestContext) -> Result<StreamResult> {
        let permit = self
            .permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| crate::error::LexError::Provider("并发节流信号量已关闭".into()))?;
        let stream = self.inner.send(ctx).await?;
        Ok(async_stream::stream! {
            let _permit = permit; // 持有至流结束;调用方提前 drop 流时随之释放
            let mut inner = stream;
            while let Some(item) = inner.next().await {
                yield item;
            }
        }
        .boxed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Usage;
    use crate::provider::ProviderEvent;
    use futures::StreamExt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct SlowProbe {
        active: AtomicUsize,
        peak: AtomicUsize,
    }
    #[async_trait::async_trait]
    impl Provider for SlowProbe {
        async fn send(&self, _ctx: RequestContext) -> Result<StreamResult> {
            let now = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(futures::stream::iter(vec![Ok(ProviderEvent::Completed { usage: Usage::default() })]).boxed())
        }
    }

    #[tokio::test]
    async fn limits_concurrent_in_flight_streams() {
        let probe = Arc::new(SlowProbe { active: AtomicUsize::new(0), peak: AtomicUsize::new(0) });
        let throttled = ThrottledProvider::new(probe.clone(), 2);
        let mut tasks = Vec::new();
        for _ in 0..5 {
            let t = throttled.clone();
            tasks.push(tokio::spawn(async move { t.send(RequestContext { system: String::new(), tools: vec![], messages: vec![] }).await.unwrap().next().await }));
        }
        for t in tasks {
            t.await.unwrap().unwrap().unwrap();
        }
        assert_eq!(probe.peak.load(Ordering::SeqCst), 2, "同时在途的流不得超过许可数");
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p lex-core throttle`
Expected: FAIL,模块不存在。

- [ ] **Step 3: 实现**

按 Step 1 代码落盘(测试与实现同文件);`lex-core/src/provider/mod.rs` 加 `pub mod throttle;`。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p lex-core throttle`
Expected: PASS(并发峰值 = 2)。

- [ ] **Step 5: Commit**

```bash
git add lex-core/src/provider
git commit -m "feat(provider): ThrottledProvider 并发在途流节流(默认 3,许可持有至流结束)"
```

---

### Task 6: SubagentRuntime(agent 层实现派生器 + 缓存友好 system 组装)

**Files:**
- Create: `lex-core/src/agent/subagent.rs`
- Modify: `lex-core/src/agent/mod.rs`(加 `pub mod subagent;`)
- Modify: `lex-core/src/security/rules.rs`(`SecurityRules` 加 `#[derive(Clone)]`)
- Test: `lex-core/src/agent/subagent.rs`(`mod tests`,可访问私有字段构造指定 depth 的 runtime)

**Interfaces:**
- Consumes: `AgentLoop`(现有全部 pub 字段)、`ThrottledProvider`(Clone,Task 5)、`SubagentSpawner/SubagentRequest`(Task 3)、`SpawnSubagent`(Task 4)、`ToolRegistry::subset/names/Clone`(Task 1)、`SecurityRules::clone`(本任务加)。
- Produces:
  - `pub const MAX_SPAWN_DEPTH: u32 = 2;`
  - `pub struct SubagentRuntime { .. }`,`pub fn new(provider: ThrottledProvider, base_system: String, handler: std::sync::Arc<dyn crate::security::PermissionHandler>, rules: SecurityRules, cwd: std::path::PathBuf, shell: Option<crate::tools::ShellCommand>, todos: Arc<std::sync::Mutex<Vec<crate::tools::Todo>>>, base_registry: std::sync::Arc<ToolRegistry>, max_turns: u32, context_limit: Option<u32>, on_tool_result: Option<crate::agent::ToolResultHook>) -> Self`(depth 固定 0,即主循环层级)
  - `#[async_trait] impl SubagentSpawner for SubagentRuntime`
  - `pub fn subagent_system_suffix(task: &str, allowed_tools: &[String]) -> String`
  - `pub fn compose_subagent_system(base: &str, suffix: &str) -> String`
  - `fn child_task_text(req: &SubagentRequest) -> String`(私有)
  - `fn nested_spawner_allowed(depth: u32, allow_nested: bool) -> bool`(私有)

- [ ] **Step 1: 写失败测试**

`lex-core/src/agent/subagent.rs` 底部 `mod tests`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{Provider, ProviderEvent, RequestContext, StreamResult};
    use crate::tools::{SubagentRequest as Req, ToolRegistry};
    use std::collections::VecDeque;
    use std::sync::Mutex;

    #[test]
    fn composed_system_keeps_base_as_byte_prefix() {
        // 缓存关键:子 agent system = 主 prompt 原文前缀,任务限定只追加在末尾
        let base = "# 身份\n你是编程 agent。\n\n# 核心原则\n安全第一。";
        let suffix = subagent_system_suffix("排查测试", &["file_read".into()]);
        let composed = compose_subagent_system(base, &suffix);
        assert!(composed.starts_with(base), "任务限定内容必须追加在固定前缀之后");
        assert!(composed.contains("file_read"));
        assert!(composed.contains("子任务摘要"));
        // 同一任务两次生成应逐字节一致(前缀缓存稳定性)
        assert_eq!(subagent_system_suffix("t", &["a".into()]), subagent_system_suffix("t", &["a".into()]));
    }

    #[test]
    fn nested_depth_boundary() {
        // 语义:depth 层 runtime 派生子 agent 时,子 agent 能否获得派生能力。
        // 共 2 层派生:主(0)→子(1)可授;子(1)→孙(2)不授(孙彻底没有派生工具,结构性封顶)。
        assert!(nested_spawner_allowed(0, true));
        assert!(!nested_spawner_allowed(1, true), "孙 agent 不得再获得派生能力(2 层硬上限)");
        assert!(!nested_spawner_allowed(2, true));
        assert!(!nested_spawner_allowed(0, false), "未显式允许不得嵌套");
    }

    #[test]
    fn child_task_text_merges_context_fragment() {
        let req = SubagentRequest {
            task: "排查失败".into(),
            allowed_tools: vec!["file_read".into()],
            context: Some("模块在 src/foo".into()),
            context_budget: None,
            allow_nested: false,
        };
        let text = child_task_text(&req);
        assert!(text.contains("[父级上下文]"));
        assert!(text.contains("模块在 src/foo"));
        assert!(text.contains("排查失败"));
        let mut req2 = req.clone();
        req2.context = None;
        assert_eq!(child_task_text(&req2), "排查失败");
    }

    // —— 以下为 spawn 行为测试:脚本化 Provider,验证权限/深度/过滤不被任务描述绕过 ——
    use crate::provider::{Provider, ProviderEvent, RequestContext, StreamResult};
    use crate::tools::{SubagentRequest as Req, ToolRegistry};
    use std::collections::VecDeque;
    use std::sync::Mutex;

    #[derive(Clone, Default)]
    struct ScriptedProvider {
        scripts: Arc<Mutex<VecDeque<Vec<ProviderEvent>>>>,
    }
    impl ScriptedProvider {
        fn push(&self, events: Vec<ProviderEvent>) {
            self.scripts.lock().unwrap_or_else(|p| p.into_inner()).push_back(events);
        }
    }
    #[async_trait::async_trait]
    impl Provider for ScriptedProvider {
        async fn send(&self, _ctx: RequestContext) -> Result<StreamResult> {
            let script = self
                .scripts
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .pop_front()
                .unwrap_or_default();
            Ok(futures::stream::iter(script.into_iter().map(Ok)).boxed())
        }
    }

    fn completed() -> ProviderEvent {
        ProviderEvent::Completed { usage: crate::message::Usage::default() }
    }
    fn text_reply(s: &str) -> Vec<ProviderEvent> {
        vec![ProviderEvent::TextDelta(s.to_string()), completed()]
    }

    fn runtime_at_depth(
        provider: &ScriptedProvider,
        depth: u32,
        registry: std::sync::Arc<ToolRegistry>,
    ) -> SubagentRuntime {
        let throttled = crate::provider::throttle::ThrottledProvider::new(Arc::new(provider.clone()), 3);
        SubagentRuntime::new_for_test(
            throttled,
            "主提示词前缀".into(),
            Arc::new(AllowHandler),
            crate::security::SecurityRules::defaults(),
            std::path::PathBuf::from("."),
            None,
            Default::default(),
            registry,
            10,
            Some(64_000),
            None,
            depth,
        )
    }

    struct AllowHandler;
    #[async_trait::async_trait]
    impl crate::security::PermissionHandler for AllowHandler {
        async fn confirm(&self, _: &crate::security::PendingAction) -> crate::error::Result<bool> {
            Ok(true)
        }
    }

    #[tokio::test]
    async fn spawn_returns_child_summary_only() {
        let provider = ScriptedProvider::default();
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(crate::tools::file_read::FileRead));
        registry.register(Box::new(crate::tools::spawn_subagent::SpawnSubagent));
        let registry = Arc::new(registry);
        // 子 agent 一轮:直接产出结构化摘要
        provider.push(text_reply("## 子任务摘要\n- **做了什么**:读了文件\n- **关键结论**:原因 A\n- **修改的文件**:无"));
        let rt = runtime_at_depth(&provider, 0, registry.clone());
        let summary = rt
            .spawn(Req {
                task: "排查".into(),
                allowed_tools: vec!["file_read".into()],
                context: None,
                context_budget: None,
                allow_nested: false,
            })
            .await
            .unwrap();
        assert!(summary.contains("子任务摘要"));
        // 父级拿到的只有摘要,不带子历史
        assert!(!summary.contains("TextDelta"));
    }

    #[tokio::test]
    async fn unknown_allowed_tool_is_rejected() {
        let provider = ScriptedProvider::default();
        let registry = Arc::new(ToolRegistry::new());
        let rt = runtime_at_depth(&provider, 0, registry);
        let err = rt
            .spawn(Req { task: "t".into(), allowed_tools: vec!["ghost".into()], context: None, context_budget: None, allow_nested: false })
            .await
            .unwrap_err();
        assert!(err.to_string().contains("未知工具"), "实际: {err}");
    }

    #[tokio::test]
    async fn nested_chain_reaches_two_layers_and_grandchild_cannot_spawn() {
        let provider = ScriptedProvider::default();
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(crate::tools::file_read::FileRead));
        registry.register(Box::new(crate::tools::spawn_subagent::SpawnSubagent));
        let registry = Arc::new(registry);
        let rt = runtime_at_depth(&provider, 0, registry);
        // 子脚本:子 agent 再派生孙 agent(allow_nested=true,depth 1 → 2 合法)
        provider.push(vec![
            ProviderEvent::ToolUseComplete {
                id: "c1".into(),
                name: "spawn_subagent".into(),
                input: serde_json::json!({"task":"孙任务","allowed_tools":["file_read"],"allow_nested":true}),
            },
            completed(),
        ]);
        // 孙脚本:孙 agent(depth 2)只能文本作答——其注册表里没有 spawn_subagent 可调
        provider.push(text_reply("## 子任务摘要\n- **做了什么**:孙任务完成"));
        // 子收尾:汇总
        provider.push(text_reply("## 子任务摘要\n- **做了什么**:委派孙任务完成"));
        let summary = rt
            .spawn(Req { task: "子任务".into(), allowed_tools: vec!["file_read".into()], context: None, context_budget: None, allow_nested: true })
            .await
            .unwrap();
        assert!(summary.contains("子任务摘要"));
    }

    #[tokio::test]
    async fn depth_cap_enforced_defensively() {
        // 结构上 depth-2 runtime 不会持有派生器;防御性守卫兜底(即便被错误构造也拒绝)
        let provider = ScriptedProvider::default();
        let registry = Arc::new(ToolRegistry::new());
        let rt = runtime_at_depth(&provider, 2, registry);
        let err = rt
            .spawn(Req { task: "t".into(), allowed_tools: vec!["file_read".into()], context: None, context_budget: None, allow_nested: true })
            .await
            .unwrap_err();
        assert!(err.to_string().contains("上限"), "实际: {err}");
        // 结构性证明:子(1)派生孙(2)时不再授予派生能力
        assert!(!nested_spawner_allowed(1, true));
        assert!(nested_spawner_allowed(0, true));
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p lex-core subagent`
Expected: FAIL,模块/类型不存在。

- [ ] **Step 3: 最小实现**

`lex-core/src/security/rules.rs`:`pub struct SecurityRules` 上加 `#[derive(Clone)]`。

`lex-core/src/agent/mod.rs` 顶部加 `pub mod subagent;`。

`lex-core/src/agent/subagent.rs`:

```rust
use crate::error::{LexError, Result};
use crate::provider::throttle::ThrottledProvider;
use crate::security::{PermissionHandler, SecurityGuard, SecurityRules};
use crate::tools::{ShellCommand, SubagentRequest, SubagentSpawner, Todo, ToolContext, ToolRegistry};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// 派生层数硬上限:主循环为第 0 层,最多派生两层(子 = 1,孙 = 2)。
/// 不做成配置项,防止失控递归。
pub const MAX_SPAWN_DEPTH: u32 = 2;

/// depth 层 runtime 派生子 agent 时,子 agent 能否获得派生能力:
/// 共 2 层派生(子=1、孙=2),孙的孙(3)结构性不可达。
fn nested_spawner_allowed(depth: u32, allow_nested: bool) -> bool {
    allow_nested && depth + 2 <= MAX_SPAWN_DEPTH
}

/// 子 agent system prompt:主 prompt 原文作为逐字节前缀(保住隐式前缀缓存),
/// 任务范围限定只追加在末尾,绝不插进固定前缀中间。
pub fn compose_subagent_system(base: &str, suffix: &str) -> String {
    format!("{base}\n\n{suffix}")
}

pub fn subagent_system_suffix(task: &str, allowed_tools: &[String]) -> String {
    format!(
        "# 子任务模式\n你是被主 agent 派生出来的子 agent,只负责下面这一项独立子任务,完成后即结束:\n\n{task}\n\n## 任务范围限定\n- 你只能使用以下工具:{};不要尝试调用列表之外的任何工具。\n- 你的执行过程不会回到主对话,主 agent 只会收到你的最终回复,请让最终回复自包含。\n- 完成任务后,最终回复必须严格使用以下结构化摘要格式:\n\n## 子任务摘要\n- **做了什么**:<步骤概述>\n- **关键结论**:<发现/结果>\n- **修改的文件**:<文件路径列表;无修改写「无」>",
        allowed_tools.join(", ")
    )
}

/// 子 agent 首条任务消息 = 父级上下文片段 + 任务描述
fn child_task_text(req: &SubagentRequest) -> String {
    match req.context.as_deref() {
        Some(c) if !c.trim().is_empty() => format!("[父级上下文]\n{c}\n\n[子任务]\n{}", req.task),
        _ => req.task.clone(),
    }
}

/// 子 agent 运行时:持有共享的节流 provider / 主 prompt 前缀 / 父级权限配置 / 基础注册表。
pub struct SubagentRuntime {
    provider: ThrottledProvider,
    base_system: String,
    handler: Arc<dyn PermissionHandler>,
    rules: SecurityRules,
    cwd: PathBuf,
    shell: Option<ShellCommand>,
    todos: Arc<Mutex<Vec<Todo>>>,
    base_registry: Arc<ToolRegistry>,
    max_turns: u32,
    context_limit: Option<u32>,
    on_tool_result: Option<crate::agent::ToolResultHook>,
    depth: u32,
}

impl SubagentRuntime {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provider: ThrottledProvider,
        base_system: String,
        handler: Arc<dyn PermissionHandler>,
        rules: SecurityRules,
        cwd: PathBuf,
        shell: Option<ShellCommand>,
        todos: Arc<Mutex<Vec<Todo>>>,
        base_registry: Arc<ToolRegistry>,
        max_turns: u32,
        context_limit: Option<u32>,
        on_tool_result: Option<crate::agent::ToolResultHook>,
    ) -> Self {
        Self::new_for_test(provider, base_system, handler, rules, cwd, shell, todos, base_registry, max_turns, context_limit, on_tool_result, 0)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_for_test(
        provider: ThrottledProvider,
        base_system: String,
        handler: Arc<dyn PermissionHandler>,
        rules: SecurityRules,
        cwd: PathBuf,
        shell: Option<ShellCommand>,
        todos: Arc<Mutex<Vec<Todo>>>,
        base_registry: Arc<ToolRegistry>,
        max_turns: u32,
        context_limit: Option<u32>,
        on_tool_result: Option<crate::agent::ToolResultHook>,
        depth: u32,
    ) -> Self {
        SubagentRuntime { provider, base_system, handler, rules, cwd, shell, todos, base_registry, max_turns, context_limit, on_tool_result, depth }
    }
}

#[async_trait::async_trait]
impl SubagentSpawner for SubagentRuntime {
    async fn spawn(&self, req: SubagentRequest) -> Result<String> {
        // 1. allowed_tools 校验与过滤:spawn_subagent 永不随 allowed_tools 传入(嵌套只由 allow_nested 控制)
        let allowed: Vec<String> = req.allowed_tools.iter().filter(|n| n.as_str() != "spawn_subagent").cloned().collect();
        if allowed.is_empty() {
            return Err(LexError::Tool("allowed_tools 过滤后为空:至少提供一个工具".into()));
        }
        let unknown: Vec<String> = allowed.iter().filter(|n| self.base_registry.get(n).is_none()).cloned().collect();
        if !unknown.is_empty() {
            return Err(LexError::Tool(format!(
                "allowed_tools 含未知工具: {}(可用: {})",
                unknown.join(", "),
                self.base_registry.names().join(", ")
            )));
        }

        // 2. 深度防御 + 嵌套判定:显式 allow_nested + 深度硬上限;
        //    不授予时子 agent 完全拿不到 spawn 工具与派生器(结构性不可绕过)
        if self.depth + 1 > MAX_SPAWN_DEPTH {
            return Err(LexError::Tool(format!("已达派生深度硬上限 {} 层,禁止继续派生", MAX_SPAWN_DEPTH)));
        }
        let nested_spawner: Option<Arc<dyn SubagentSpawner>> = if nested_spawner_allowed(self.depth, req.allow_nested) {
            Some(Arc::new(SubagentRuntime::new_for_test(
                self.provider.clone(),
                self.base_system.clone(),
                self.handler.clone(),
                self.rules.clone(),
                self.cwd.clone(),
                self.shell.clone(),
                Arc::new(Mutex::new(Vec::new())),
                self.base_registry.clone(),
                self.max_turns,
                self.context_limit,
                self.on_tool_result.clone(),
                self.depth + 1,
            )))
        } else {
            None
        };

        let mut child_registry = self.base_registry.subset(&allowed);
        if nested_spawner.is_some() {
            child_registry.register(Box::new(crate::tools::spawn_subagent::SpawnSubagent));
        }

        // 3. 独立 AgentLoop:独立历史、独立 todos、独立 SecurityGuard(规则表与父级相同 = 权限不高于父级)
        let mut child = crate::agent::AgentLoop {
            provider: Box::new(self.provider.clone()),
            registry: child_registry,
            handler: Box::new(self.handler.clone()),
            tool_ctx: ToolContext { cwd: self.cwd.clone(), shell: self.shell.clone(), todos: Arc::new(Mutex::new(Vec::new())), spawner: nested_spawner },
            security: SecurityGuard::new(self.rules.clone()),
            system: compose_subagent_system(&self.base_system, &subagent_system_suffix(&req.task, &allowed)),
            history: vec![],
            max_turns: self.max_turns,
            cache_strategy: None,
            context_limit: req.context_budget.or(self.context_limit),
            pending_summary: None,
            compress_attempted: false,
            on_tool_result: self.on_tool_result.clone(),
        };

        // 4. 运行子任务,只把最终摘要(结构化文本)交回父级
        let final_text = child.run_turn(&child_task_text(&req), &mut |_e| {}).await?;
        Ok(final_text)
    }
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --workspace`
Expected: 全部 PASS(含既有 agent_loop/context_flow 回归)。

- [ ] **Step 5: Commit**

```bash
git add lex-core
git commit -m "feat(agent): SubagentRuntime 派生器(独立上下文/权限继承/深度硬上限/缓存友好前缀组装)"
```

---

### Task 7: main.rs 装配(ThrottledProvider + SubagentRuntime 注入 ToolContext)

**Files:**
- Modify: `lex-cli/src/main.rs`(`build_loop` 与 `run`)
- Test: 现有编译即回归(`cargo build --workspace`);行为冒烟在 Task 8。

**Interfaces:**
- Consumes: Task 5/6 全部产物。
- Produces: 生产装配——主循环与子 agent 共享同一个并发节流信号量;`ToolContext.spawner` 指向 depth=0 的 `SubagentRuntime`;`todos` 的 `Arc` 提升到 `run()` 层(TUI 任务将复用)。

- [ ] **Step 1: 改造 `build_loop`**

`build_loop` 签名改为(handler/todos 由外部注入,便于后续 TUI 复用):

```rust
fn build_loop(
    cfg: &Config,
    cwd: PathBuf,
    handler: std::sync::Arc<dyn lex_core::security::PermissionHandler>,
    todos: std::sync::Arc<std::sync::Mutex<Vec<lex_core::tools::Todo>>>,
    on_tool_result: Option<ToolResultHook>,
) -> Result<AgentLoop> {
```

函数体内:

```rust
    let api_key = resolve_api_key(&cfg.provider)?;
    let inner: std::sync::Arc<dyn Provider> = match cfg.provider.as_str() {
        "openai" => std::sync::Arc::new(OpenAiCompatProvider::with_defaults(
            cfg.openai.base_url.clone(),
            cfg.openai.model.clone(),
            OpenAiParams {
                max_tokens: cfg.openai.max_tokens,
                thinking: cfg.openai.thinking.clone(),
                reasoning_effort: cfg.openai.reasoning_effort.clone(),
                user_id: cfg.openai.user_id.clone(),
            },
            api_key,
        )?),
        _ => std::sync::Arc::new(AnthropicProvider::with_defaults(
            cfg.anthropic.base_url.clone(),
            cfg.anthropic.model.clone(),
            cfg.anthropic.max_tokens,
            api_key,
        )?),
    };
    // 主循环与所有子 agent 共享同一并发节流(默认 3 条在途流)
    let throttled = lex_core::provider::throttle::ThrottledProvider::new(inner, lex_core::provider::throttle::MAX_CONCURRENT_STREAMS);
```

registry 部分(注册 5 内置工具 + SpawnSubagent,再 Clone 给 AgentLoop):

```rust
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(FileRead));
    registry.register(Box::new(FileEdit));
    registry.register(Box::new(BashExec));
    registry.register(Box::new(GrepSearch));
    registry.register(Box::new(TodoWrite));
    registry.register(Box::new(lex_core::tools::spawn_subagent::SpawnSubagent));
    let registry = std::sync::Arc::new(registry);
```

shell/system 段不变;`SecurityRules` 只构建一次并 Clone:

```rust
    let rules = lex_core::security::SecurityRules::build(&cfg.security)?;
    let spawner: std::sync::Arc<dyn lex_core::tools::SubagentSpawner> = std::sync::Arc::new(
        lex_core::agent::subagent::SubagentRuntime::new(
            throttled.clone(),
            system.clone(),
            handler.clone(),
            rules.clone(),
            cwd.clone(),
            shell.clone(),
            todos.clone(),
            registry.clone(),
            cfg.max_turns,
            cfg.context.enabled.then_some(cfg.context.limit),
            on_tool_result.clone(),
        ),
    );

    Ok(AgentLoop {
        provider: Box::new(throttled),
        registry: (*registry).clone(),
        handler: Box::new(handler),
        tool_ctx: ToolContext { cwd, shell, todos, spawner: Some(spawner) },
        security: SecurityGuard::new(rules),
        system,
        history: vec![],
        max_turns: cfg.max_turns,
        cache_strategy: Some(cache_strategy),
        context_limit: cfg.context.enabled.then_some(cfg.context.limit),
        pending_summary: None,
        compress_attempted: false,
        on_tool_result,
    })
```

- [ ] **Step 2: 改造 `run()`**

```rust
    let input = std::sync::Arc::new(confirm::CliInput::new());
    let handler: std::sync::Arc<dyn lex_core::security::PermissionHandler> = input.clone();
    let todos: std::sync::Arc<std::sync::Mutex<Vec<lex_core::tools::Todo>>> = Default::default();
    let renderer = Arc::new(Mutex::new(ui::events::Renderer::new()));
    let mut agent = build_loop(&cfg, cwd.clone(), handler, todos, Some(make_result_hook(&renderer)))?;
```

(`make_result_hook` 不变;单任务分支与交互分支签名随之适配。)

- [ ] **Step 3: 编译与全量测试**

Run: `cargo build --workspace && cargo test --workspace`
Expected: 编译零警告级错误,测试全部 PASS。

- [ ] **Step 4: Commit**

```bash
git add lex-cli/src/main.rs
git commit -m "feat(cli): 装配 SubagentRuntime 与并发节流 provider,spawn_subagent 进入工具注册表"
```

---

### Task 8: 端到端回归 + 文档 + 真实冒烟步骤

**Files:**
- Create: `lex-core/tests/subagent_e2e.rs`(跨层装配级回归)
- Modify: `README.md`(安全模型小节补"子 agent 权限继承/深度上限")、`AGENTS.md`(路线图与工具清单)、`examples/smoke/README.md`(第 10 节)、`assets/coding-agent-system-prompt.md`(spawn 使用守则)

**Interfaces:**
- Consumes: Task 1–7 全部产物。

- [ ] **Step 1: 写 e2e 回归测试**

`lex-core/tests/subagent_e2e.rs`:

```rust
//! 端到端:主循环 → spawn_subagent → 子 agent → 摘要回填主循环历史。
//! 全程脚本化 Provider,不触网。
use lex_core::agent::AgentLoop;
use lex_core::message::Message;
use lex_core::provider::throttle::ThrottledProvider;
use lex_core::provider::{Provider, ProviderEvent, RequestContext, StreamResult};
use lex_core::security::{PermissionHandler, PendingAction, SecurityGuard, SecurityRules};
use lex_core::tools::spawn_subagent::SpawnSubagent;
use lex_core::tools::file_read::FileRead;
use lex_core::tools::{ToolContext, ToolRegistry};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct ScriptedProvider {
    scripts: Arc<Mutex<VecDeque<Vec<ProviderEvent>>>>,
}
#[async_trait::async_trait]
impl Provider for ScriptedProvider {
    async fn send(&self, _ctx: RequestContext) -> Result<StreamResult> {
        let script = self.scripts.lock().unwrap_or_else(|p| p.into_inner()).pop_front().unwrap_or_default();
        Ok(futures::stream::iter(script.into_iter().map(Ok)).boxed())
    }
}

struct YesHandler;
#[async_trait::async_trait]
impl PermissionHandler for YesHandler {
    async fn confirm(&self, _: &PendingAction) -> lex_core::error::Result<bool> {
        Ok(true)
    }
}

fn completed() -> ProviderEvent {
    ProviderEvent::Completed { usage: lex_core::message::Usage::default() }
}
fn text_reply(s: &str) -> Vec<ProviderEvent> {
    vec![ProviderEvent::TextDelta(s.to_string()), completed()]
}

#[tokio::test]
async fn main_loop_delegates_and_keeps_only_summary() {
    let provider = ScriptedProvider::default();
    // 第 1 轮(主):模型调用 spawn_subagent
    provider.scripts.lock().unwrap_or_else(|p| p.into_inner()).push_back(vec![
        ProviderEvent::ToolUseComplete {
            id: "t1".into(),
            name: "spawn_subagent".into(),
            input: serde_json::json!({"task":"排查测试失败原因","allowed_tools":["file_read"],"context":"测试目录 tests/"}),
        },
        completed(),
    ]);
    // 第 1 轮(子):子 agent 直接给出结构化摘要
    provider.scripts.lock().unwrap_or_else(|p| p.into_inner()).push_back(text_reply(
        "## 子任务摘要\n- **做了什么**:通读了 tests 目录\n- **关键结论**:失败原因为断言过期\n- **修改的文件**:无",
    ));
    // 第 2 轮(主):模型基于摘要作答
    provider.scripts.lock().unwrap_or_else(|p| p.into_inner()).push_back(text_reply("子任务已完成,结论:断言过期。"));

    let throttled = ThrottledProvider::new(Arc::new(provider.clone()), 3);
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(FileRead));
    registry.register(Box::new(SpawnSubagent));
    let todos: Arc<Mutex<Vec<lex_core::tools::Todo>>> = Default::default();
    let spawner: Arc<dyn lex_core::tools::SubagentSpawner> = Arc::new(lex_core::agent::subagent::SubagentRuntime::new(
        throttled.clone(),
        "主提示词".into(),
        Arc::new(YesHandler),
        SecurityRules::defaults(),
        std::path::PathBuf::from("."),
        None,
        todos.clone(),
        Arc::new(registry),
        10,
        Some(64_000),
        None,
    ));

    let mut loop_registry = ToolRegistry::new();
    loop_registry.register(Box::new(FileRead));
    loop_registry.register(Box::new(SpawnSubagent));
    let mut agent = AgentLoop {
        provider: Box::new(throttled),
        registry: loop_registry,
        handler: Box::new(YesHandler),
        tool_ctx: ToolContext { cwd: std::path::PathBuf::from("."), shell: None, todos, spawner: Some(spawner) },
        security: SecurityGuard::new(SecurityRules::defaults()),
        system: "主提示词".into(),
        history: vec![],
        max_turns: 10,
        cache_strategy: None,
        context_limit: None,
        pending_summary: None,
        compress_attempted: false,
        on_tool_result: None,
    };

    let final_text = agent.run_turn("排查测试失败原因", &mut |_| {}).await.unwrap();
    assert!(final_text.contains("断言过期"));
    // 主上下文里:tool_result 内容 = 子 agent 摘要(不含子过程)
    let summary_block = agent.history.iter().find_map(|m: &Message| {
        m.content.iter().find_map(|b| match b {
            lex_core::message::Block::ToolResult { content, .. } if content.contains("子任务摘要") => Some(content.clone()),
            _ => None,
        })
    });
    assert!(summary_block.is_some(), "主历史应包含子任务摘要 ToolResult");
}
```

Run: `cargo test -p lex-core --test subagent_e2e`
Expected: PASS。

- [ ] **Step 2: assets 提示词补 spawn 守则**

`assets/coding-agent-system-prompt.md` 工具说明区追加一小节(措辞即"给模型的指令"):

```markdown
## spawn_subagent 使用守则
- 只在子任务的执行长度、探索成本明显高于"污染主上下文"的代价时派生(如:批量排查、大范围检索、独立试错);琐碎单步操作直接自己做。
- 派生时把任务写自包含,并在 `context` 里只带必要片段;用 `allowed_tools` 做最小授权。
- 你只会收到子 agent 的结构化摘要,子过程不会回到本对话。
```

- [ ] **Step 3: 文档更新**

- `examples/smoke/README.md` 追加"第 10 节:Phase 6 子 agent 真实冒烟":步骤 = 启动交互模式 → 输入"请用子 agent 排查 lex-core 下所有 TODO 并汇总成清单,allowed_tools 只给 file_read/grep_search" → 预期:终端出现 `spawn_subagent(派生子 agent 执行子任务: …)` 确认项(y/N)、子任务 ⎿ 结果行、最终回复含"做了什么/关键结论/修改的文件"结构;记录缓存遥测(子 agent 首轮 prefix 前缀命中情况)。
- `README.md` 安全模型小节补两行:子 agent 权限等级继承父级(同一规则表 + 同一确认处理器,不允许配置降级);派生深度硬上限 2 层,`allow_nested` 无法越过。
- `AGENTS.md`:tools 清单加 spawn_subagent;架构边界注明"`tools/` 定义 `SubagentSpawner` trait,`agent/subagent.rs` 实现,方向不变"。

- [ ] **Step 4: 全量回归 + Commit**

Run: `cargo test --workspace`
Expected: 全部 PASS(≥115 个测试)。

```bash
git add -A
git commit -m "feat: Phase6 子 agent 端到端回归与文档/冒烟/提示词守则"
```

---

## 验收对照(任务书)

| 任务书要求 | 落点 |
|---|---|
| `spawn_subagent(task, allowed_tools, context_budget)` 遵循 Tool trait | Task 4 |
| 子 agent 复用 AgentLoop 状态机,独立历史 + 任务限定 system | Task 6 |
| 权限不高于父级、不可被任务描述绕过 | Task 6(同规则表 + 同 handler + 结构性无工具);Task 6 测试 |
| 嵌套默认禁止,`allow_nested` + 硬编码深度上限 2 | Task 6(`nested_spawner_allowed` + 注册表排除) |
| 并发派生 + Provider 层节流 | Task 2(parallel_safe)+ Task 5(ThrottledProvider) |
| 只回结构化摘要,不回完整历史 | Task 6(spawn 返回 final_text)+ Task 8 e2e |
| system 前缀复用保缓存,限定内容放末尾 | Task 6(`compose_subagent_system` 前缀测试) |
| 派生时机守则(不污染提示词代码) | Task 8(assets 提示词小节) |
