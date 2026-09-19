# 子 agent 成本护栏 + 结构化子事件 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 给子 agent 派生加每轮成本上限(可配,默认 4),并把子 agent 的工具活动以带归属的结构化事件转发到终端。

**Architecture:** 护栏用一棵派生树共享的轮次级计数器(`SpawnState`),由 `SubagentSpawner` 新增的 `begin_turn()` 生命周期钩子驱动重置,且**仅根 runtime(depth 0)**清零。子事件走 `lex-core` 定义的中立 `ChildEvent` 类型 + `ChildEventHook` 回调;子 agent 的工具结果**不再经共享的 `on_tool_result`**,改由该通道承载——既给结果行加了归属,又顺带修掉"子级 `⎿` 减父级 `pending_tools` 计数却无 `●` 来加"的失衡。

**Tech Stack:** Rust(tokio / async-trait / futures / serde / toml),均为现有依赖,不新增 crate。

**Spec:** `docs/superpowers/specs/2026-09-19-subagent-budget-and-child-events-design.md`

## Global Constraints

- 非测试代码禁止裸 `unwrap()`/`expect()`(`unwrap_or` / `unwrap_or_else(|p| p.into_inner())` / `map_or` 等显式处理允许;测试代码可用 `unwrap`)。
- 进入请求 payload 的 serde 结构体字段顺序 = derive 声明顺序;动态 JSON 用 `Value`/`Vec`,禁止 `HashMap`。
- 架构边界:`lex-cli → lex-core` 单向;`lex-core` 内 `agent → provider/tools/security/context`,**反向禁止**——`lex-core/src/tools/` 不得引用 `agent/` 下任何类型。
- 路径一律 `PathBuf`。
- **颜色/ANSI 只从 `lex-cli/src/ui/theme.rs` 取**,其他文件禁止裸 `\x1b[`。
- 提示词/交互输出与代码注释用中文。
- Forbidden 级安全规则不可被绕过;有副作用的工具执行必须经 `execute_tool_call` 内部的权限检查。
- 构建测试:WSL 下 cargo 不在 PATH,必须用绝对路径 `/mnt/c/Users/Administrator/.cargo/bin/cargo.exe`;**绝不用管道退出码判成败**(管道返回末命令状态,且过滤到 0 个测试也退出 0)——读 `test result:` 行。不要改 `.cargo/config.toml`。
- 基线:**163 个测试全绿、零警告**。每个任务结束时不得回归。
- 每次 commit 前跑相关测试;commit 信息用中文 conventional commits;**用显式 `git add <文件>`,不用 `git add -A`**。

---

### Task 1: 配置 `[agent]` 段

**Files:**
- Modify: `lex-core/src/config.rs`(新增 `AgentConfig`、挂进 `Config`、加进 `Config::default`)
- Modify: `README.md`(配置参考表补一行)
- Test: `lex-core/src/config.rs`(`mod tests`)

**Interfaces:**
- Produces: `pub struct AgentConfig { pub max_children_per_turn: u32 }`,默认 `4`;`Config` 新增字段 `pub agent: AgentConfig`。
- 消费方:Task 2(`main.rs` 读 `cfg.agent.max_children_per_turn` 传给 `SpawnLimits`)、Task 5(schema 描述)。

- [ ] **Step 1: 写失败测试**

在 `lex-core/src/config.rs` 的 `mod tests` 末尾追加:

```rust
    #[test]
    fn agent_section_defaults_to_four_and_parses_zero() {
        // 缺省 → 4
        let d = temp_dir("agent-default");
        fs::write(d.join("lex-code.toml"), "[anthropic]\nbase_url = \"http://127.0.0.1:9\"\nmodel = \"m\"\n").unwrap();
        assert_eq!(Config::load(&d).unwrap().agent.max_children_per_turn, 4);

        // 显式 0(禁止派生)必须能与「缺省」区分开 —— 二者语义不同
        let d0 = temp_dir("agent-zero");
        fs::write(
            d0.join("lex-code.toml"),
            "[anthropic]\nbase_url = \"http://127.0.0.1:9\"\nmodel = \"m\"\n[agent]\nmax_children_per_turn = 0\n",
        )
        .unwrap();
        assert_eq!(Config::load(&d0).unwrap().agent.max_children_per_turn, 0);

        // 显式值
        let d12 = temp_dir("agent-explicit");
        fs::write(
            d12.join("lex-code.toml"),
            "[anthropic]\nbase_url = \"http://127.0.0.1:9\"\nmodel = \"m\"\n[agent]\nmax_children_per_turn = 12\n",
        )
        .unwrap();
        assert_eq!(Config::load(&d12).unwrap().agent.max_children_per_turn, 12);
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `/mnt/c/Users/Administrator/.cargo/bin/cargo.exe test -p lex-core agent_section`
Expected: FAIL — 编译错 `no field 'agent' on type 'Config'`(E0609)。

- [ ] **Step 3: 最小实现**

`lex-core/src/config.rs`,`ContextConfig` 之后新增:

```rust
/// `[agent]` 子 agent 派生控制。
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    /// 每轮最多派生多少个子 agent;0 = 禁止派生。
    /// 成本护栏:ThrottledProvider 只限**并发**在途流,不限总量,故需要一个总量上界。
    pub max_children_per_turn: u32,
}

impl Default for AgentConfig {
    fn default() -> Self {
        AgentConfig { max_children_per_turn: 4 }
    }
}
```

`Config` 结构体加字段(放在 `context` 之后、`max_turns` 之前):

```rust
    pub context: ContextConfig,
    pub agent: AgentConfig,
    pub max_turns: u32,
```

`Config::default()` 里对应加:

```rust
            context: ContextConfig::default(),
            agent: AgentConfig::default(),
            max_turns: 50,
```

`README.md` 的配置参考表中,`[context].enabled` 那一行之后插入:

```markdown
| `[agent].max_children_per_turn` | `4` | 每轮最多派生多少个子 agent;`0` 表示禁止派生 |
```

- [ ] **Step 4: 跑测试确认通过**

Run: `/mnt/c/Users/Administrator/.cargo/bin/cargo.exe test -p lex-core config`
Expected: PASS(config 模块全部测试通过,含新测试)。

- [ ] **Step 5: 回归 + Commit**

Run: `/mnt/c/Users/Administrator/.cargo/bin/cargo.exe test --workspace`
Expected: **164 passed, 0 failed**(163 + 1),零警告。

```bash
git add lex-core/src/config.rs README.md
git commit -m "feat(config): 新增 [agent] 段 max_children_per_turn(默认 4,0=禁止派生)"
```

---

### Task 2: 成本护栏(计数 + 上限判定 + 每轮重置)

**Files:**
- Modify: `lex-core/src/tools/mod.rs`(`SubagentSpawner` 加 `begin_turn` 默认方法)
- Modify: `lex-core/src/agent/mod.rs`(`run_turn` 调用 `begin_turn`)
- Modify: `lex-core/src/agent/subagent.rs`(`SpawnState` / `SpawnLimits` / 判定 / 重置)
- Modify: `lex-cli/src/main.rs`(装配)
- Test: `lex-core/src/agent/subagent.rs`(`mod tests`)、`lex-core/tests/agent_loop.rs`

**Interfaces:**
- Consumes: Task 1 的 `cfg.agent.max_children_per_turn`。
- Produces:
  - `SubagentSpawner` 新增 `fn begin_turn(&self) {}`(默认无操作)。
  - `pub struct SpawnLimits { pub max_turns: u32, pub context_limit: Option<u32>, pub max_children_per_turn: u32 }`(`Copy`)。
  - `pub fn SubagentRuntime::new(provider, base_system, handler, rules, cwd, shell, base_registry, limits: SpawnLimits, on_tool_result) -> Self`(9 参,`with_depth` 额外接 `spawn_state: SpawnState` 与 `depth`)。
  - `pub(crate) struct SpawnState`(Clone,整棵树共享)。
- 消费方:Task 3 给 `SpawnState` 加子级 id 分配;Task 5 读 `limits.max_children_per_turn` 写 schema。

**注意**:本任务**不**引入 `SubagentHooks`——它属于 Task 3(那里才有第二个字段)。

- [ ] **Step 1: 写失败测试**

在 `lex-core/src/agent/subagent.rs` 的 `mod tests` 末尾追加(并把它 `use` 进 `super::*` 已覆盖的 `SpawnState`):

```rust
    #[test]
    fn try_admit_enforces_limit_without_leaking_slots() {
        let st = SpawnState::default();
        assert!(st.try_admit(2));
        assert!(st.try_admit(2));
        assert!(!st.try_admit(2), "第三次应被拒");
        assert_eq!(st.spawned(), 2, "被拒的派生不得占位(必须回滚)");
        st.reset();
        assert_eq!(st.spawned(), 0);
        assert!(st.try_admit(2), "重置后应重新可派生");
    }

    #[test]
    fn zero_limit_forbids_every_spawn() {
        let st = SpawnState::default();
        assert!(!st.try_admit(0));
        assert_eq!(st.spawned(), 0, "被拒的派生不占位");
    }

    #[test]
    fn clones_share_one_counter() {
        let a = SpawnState::default();
        let b = a.clone();
        assert!(a.try_admit(1));
        assert!(!b.try_admit(1), "clone 必须共享同一计数器,而非各持一份——否则每层各有一个上限");
    }

    #[tokio::test]
    async fn spawn_is_rejected_past_per_turn_limit() {
        let provider = ScriptedProvider::default();
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(crate::tools::file_read::FileRead));
        let registry = Arc::new(registry);
        provider.push(text_reply("## 子任务摘要\n- **做了什么**:一"));
        provider.push(text_reply("## 子任务摘要\n- **做了什么**:二"));

        let state = SpawnState::default();
        let rt = runtime_with(&provider, 0, registry, state.clone(), 1);
        let req = || Req {
            task: "t".into(),
            allowed_tools: vec!["file_read".into()],
            context: None,
            context_budget: None,
            allow_nested: false,
        };
        assert!(rt.spawn(req()).await.is_ok(), "第 1 次应放行");
        let err = rt.spawn(req()).await.unwrap_err();
        assert!(err.to_string().contains("已达上限"), "实际: {err}");
        assert_eq!(state.spawned(), 1, "被拒的那次必须回滚,不占配额");
    }

    #[tokio::test]
    async fn child_begin_turn_does_not_clear_parent_counter() {
        // 设计中最易写错处:子 runtime 与父级共享同一个 SpawnState。
        // 若 begin_turn 不按 depth 设闸,子 agent 每跑一轮都会清空父级的当轮计数,
        // 护栏表面还在、实际已被架空。
        let provider = ScriptedProvider::default();
        let registry = Arc::new(ToolRegistry::new());
        let state = SpawnState::default();

        let parent = runtime_with(&provider, 0, registry.clone(), state.clone(), 4);
        let child = runtime_with(&provider, 1, registry, state.clone(), 4);

        assert!(state.try_admit(4));
        assert_eq!(state.spawned(), 1);

        child.begin_turn(); // 子 agent 开始一轮 → 不得触碰父级计数
        assert_eq!(state.spawned(), 1, "子 runtime 的 begin_turn 必须 no-op");

        parent.begin_turn(); // 根开始新一轮 → 清零
        assert_eq!(state.spawned(), 0, "只有 depth==0 才清零");
    }
```

同时把既有的 `runtime_at_depth` 改为委托给新的 `runtime_with`(保持既有测试的调用方式不变):

```rust
    fn runtime_with(
        provider: &ScriptedProvider,
        depth: u32,
        registry: std::sync::Arc<ToolRegistry>,
        state: SpawnState,
        max_children_per_turn: u32,
    ) -> SubagentRuntime {
        let throttled = crate::provider::throttle::ThrottledProvider::new(Arc::new(provider.clone()), 3);
        SubagentRuntime::with_depth(
            throttled,
            "主提示词前缀".into(),
            Arc::new(AllowHandler),
            crate::security::SecurityRules::defaults(),
            std::path::PathBuf::from("."),
            None,
            registry,
            SpawnLimits { max_turns: 10, context_limit: Some(64_000), max_children_per_turn },
            None,
            state,
            depth,
        )
    }

    fn runtime_at_depth(
        provider: &ScriptedProvider,
        depth: u32,
        registry: std::sync::Arc<ToolRegistry>,
    ) -> SubagentRuntime {
        runtime_with(provider, depth, registry, SpawnState::default(), 4)
    }
```

在 `lex-core/tests/agent_loop.rs` 末尾追加(验证调用点确实被接到 `run_turn` 上)。该文件顶部 `use std::sync::Mutex;` 需改为 `use std::sync::{Arc, Mutex};`:

```rust
struct BeginTurnProbe {
    calls: Arc<std::sync::atomic::AtomicUsize>,
}
#[async_trait]
impl lex_core::tools::SubagentSpawner for BeginTurnProbe {
    async fn spawn(&self, _req: lex_core::tools::SubagentRequest) -> Result<String> {
        unreachable!("本测试不派生")
    }
    fn begin_turn(&self) {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[tokio::test]
async fn run_turn_signals_spawner_begin_turn() {
    let mock = MockProvider::new(vec![vec![ProviderEvent::TextDelta("好".into())]]);
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut loop_ = AgentLoop {
        provider: Box::new(mock),
        registry: ToolRegistry::new(),
        handler: Box::new(AllowAll),
        tool_ctx: ToolContext {
            cwd: PathBuf::from("."),
            shell: None,
            todos: Default::default(),
            spawner: Some(Arc::new(BeginTurnProbe { calls: calls.clone() })),
        },
        security: SecurityGuard::new(SecurityRules::defaults()),
        system: "sys".into(),
        history: vec![],
        max_turns: 5,
        cache_strategy: None,
        context_limit: None,
        pending_summary: None,
        compress_attempted: false,
        on_tool_result: None,
    };
    loop_.run_turn("任务", &mut |_| {}).await.unwrap();
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "run_turn 必须调用 begin_turn —— 否则轮次级状态永不重置,每轮上限会退化成会话上限"
    );
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `/mnt/c/Users/Administrator/.cargo/bin/cargo.exe test -p lex-core subagent`
Expected: FAIL — 编译错:`SpawnState` / `SpawnLimits` / `runtime_with` 不存在,`with_depth` 参数数不符。

- [ ] **Step 3: 最小实现**

**3a. `lex-core/src/tools/mod.rs`** — `SubagentSpawner` trait 加默认方法:

```rust
#[async_trait::async_trait]
pub trait SubagentSpawner: Send + Sync {
    /// 执行子任务,返回子 agent 的结构化摘要(绝不返回子 agent 的完整消息历史)。
    async fn spawn(&self, req: SubagentRequest) -> crate::error::Result<String>;

    /// 新一轮开始的信号。实现方按需重置轮次级状态;默认无操作。
    fn begin_turn(&self) {}
}
```

**3b. `lex-core/src/agent/mod.rs`** — 在 `run_turn` 的 `self.security.reset_turn();` 之后插入:

```rust
        // 通知派生器新一轮开始(重置其轮次级状态,如当轮子 agent 计数)。
        // 子 agent 的 run_turn 也会走到这里,但其 runtime 的 depth > 0 → no-op,
        // 否则子 agent 会中途清空父级的当轮计数,护栏被静默架空。
        if let Some(spawner) = &self.tool_ctx.spawner {
            spawner.begin_turn();
        }
```

**3c. `lex-core/src/agent/subagent.rs`** — 顶部 `use` 补 `std::sync::atomic::{AtomicU32, Ordering}`,并新增:

```rust
/// 子 agent 的运行限额(集中传递,避免构造函数参数爆炸)
#[derive(Debug, Clone, Copy)]
pub struct SpawnLimits {
    pub max_turns: u32,
    pub context_limit: Option<u32>,
    /// 每轮最多派生多少个子 agent;0 = 禁止派生
    pub max_children_per_turn: u32,
}

/// 整棵派生树共享的轮次级状态。仅 depth == 0 的 runtime 在 `begin_turn` 时重置它,
/// 故「每轮上限」的口径是**整棵树在根的一轮内**的派生总数,而非每个父级各自计数。
#[derive(Clone, Default)]
pub(crate) struct SpawnState {
    spawned_this_turn: Arc<AtomicU32>,
}

impl SpawnState {
    /// 本轮已派生的子 agent 数
    fn spawned(&self) -> u32 {
        self.spawned_this_turn.load(Ordering::SeqCst)
    }

    /// 清零(新一轮开始;仅根 runtime 调用)
    fn reset(&self) {
        self.spawned_this_turn.store(0, Ordering::SeqCst);
    }

    /// 尝试占用一个派生名额。超上限时不占位(回滚)并返回 false。
    /// 用「先加后判、越限回滚」而非 CAS 循环:并发的两次调用各自的 new 都已包含对方,
    /// 故不会双双越限;回滚保证被拒的派生不消耗配额。
    fn try_admit(&self, max_children_per_turn: u32) -> bool {
        let admitted = self.spawned_this_turn.fetch_add(1, Ordering::SeqCst) + 1;
        if admitted > max_children_per_turn {
            self.spawned_this_turn.fetch_sub(1, Ordering::SeqCst);
            return false;
        }
        true
    }
}
```

`SubagentRuntime` 结构体:把 `max_turns` / `context_limit` 换成 `limits: SpawnLimits`,并加 `spawn_state`:

```rust
pub struct SubagentRuntime {
    provider: ThrottledProvider,
    base_system: String,
    handler: Arc<dyn PermissionHandler>,
    rules: SecurityRules,
    cwd: PathBuf,
    shell: Option<ShellCommand>,
    base_registry: Arc<ToolRegistry>,
    limits: SpawnLimits,
    on_tool_result: Option<crate::agent::ToolResultHook>,
    depth: u32,
    /// 整棵派生树共享;新树由 `new` 创建,嵌套时 clone 下去
    spawn_state: SpawnState,
}
```

构造函数(去掉 `#[allow(clippy::too_many_arguments)]`,已降到 9 参):

```rust
    pub fn new(
        provider: ThrottledProvider,
        base_system: String,
        handler: Arc<dyn PermissionHandler>,
        rules: SecurityRules,
        cwd: PathBuf,
        shell: Option<ShellCommand>,
        base_registry: Arc<ToolRegistry>,
        limits: SpawnLimits,
        on_tool_result: Option<crate::agent::ToolResultHook>,
    ) -> Self {
        Self::with_depth(provider, base_system, handler, rules, cwd, shell, base_registry, limits, on_tool_result, SpawnState::default(), 0)
    }

    /// 带派生深度与共享轮次状态的构造:主循环走 `new`(depth 0),嵌套派生器由此构造(depth + 1)。
    pub(crate) fn with_depth(
        provider: ThrottledProvider,
        base_system: String,
        handler: Arc<dyn PermissionHandler>,
        rules: SecurityRules,
        cwd: PathBuf,
        shell: Option<ShellCommand>,
        base_registry: Arc<ToolRegistry>,
        limits: SpawnLimits,
        on_tool_result: Option<crate::agent::ToolResultHook>,
        spawn_state: SpawnState,
        depth: u32,
    ) -> Self {
        SubagentRuntime { provider, base_system, handler, rules, cwd, shell, base_registry, limits, on_tool_result, depth, spawn_state }
    }
```

`spawn` 中:深度守卫之后、`effective_limit` 之前插入护栏判定:

```rust
        // 3. 成本护栏:整棵派生树在根的一轮内共享计数,防止单轮扇出失控。
        if !self.spawn_state.try_admit(self.limits.max_children_per_turn) {
            return Err(LexError::Tool(format!(
                "本轮派生子 agent 已达上限 {}(可在 lex-code.toml 的 [agent] max_children_per_turn 调整)",
                self.limits.max_children_per_turn
            )));
        }
```

`spawn` 中其余引用 `self.max_turns` / `self.context_limit` 处改为 `self.limits.max_turns` / `self.limits.context_limit`;
`effective_context_limit(req.context_budget, self.limits.context_limit)`;
子 AgentLoop 的 `max_turns: self.limits.max_turns`;嵌套 `with_depth` 调用多传 `self.spawn_state.clone()`。

`impl SubagentSpawner for SubagentRuntime` 中新增:

```rust
    /// 新一轮开始:仅根 runtime(depth 0)清零当轮计数。
    /// 子 runtime 与父级共享同一个计数器,不设闸的话子 agent 每跑一轮都会清空
    /// 父级的当轮计数,护栏就被静默架空了 —— 这是本设计最易写错处。
    fn begin_turn(&self) {
        if self.depth == 0 {
            self.spawn_state.reset();
        }
    }
```

**3d. `lex-cli/src/main.rs`** — `SubagentRuntime::new(...)` 的 `cfg.max_turns, context_limit,` 两个实参替换为一个:

```rust
            lex_core::agent::subagent::SpawnLimits {
                max_turns: cfg.max_turns,
                context_limit,
                max_children_per_turn: cfg.agent.max_children_per_turn,
            },
```

- [ ] **Step 4: 跑测试确认通过**

Run: `/mnt/c/Users/Administrator/.cargo/bin/cargo.exe test -p lex-core subagent`
Expected: PASS。

- [ ] **Step 5: 全量回归 + Commit**

Run: `/mnt/c/Users/Administrator/.cargo/bin/cargo.exe test --workspace`
Expected: **170 passed, 0 failed**(164 + 6 新增:subagent.rs 5 条 + agent_loop.rs 1 条),零警告。

```bash
git add lex-core/src/tools/mod.rs lex-core/src/agent/mod.rs lex-core/src/agent/subagent.rs lex-core/tests/agent_loop.rs lex-cli/src/main.rs
git commit -m "feat(agent): 子 agent 每轮派生上限(树内共享计数,仅根 runtime 重置)"
```

---

### Task 3: 结构化子事件(类型 + 发射点 + 子级结果改道)

**Files:**
- Modify: `lex-core/src/agent/mod.rs`(新增 `ChildEvent` / `ChildEventKind` / `ChildEventHook`)
- Modify: `lex-core/src/agent/subagent.rs`(`SpawnState` 加 id 分配、`SubagentHooks`、四个发射点)
- Modify: `lex-cli/src/main.rs`(构造函数换用 `SubagentHooks`,暂传 `on_child_event: None`——Task 4 接线)
- Test: `lex-core/src/agent/subagent.rs`(`mod tests`)

**Interfaces:**
- Consumes: Task 2 的 `SpawnLimits` / `SpawnState` / `with_depth`。
- Produces:
  - `pub struct ChildEvent { pub child_id: u32, pub depth: u32, pub kind: ChildEventKind }`(`Debug + Clone`)
  - `pub enum ChildEventKind { Started { task: String }, ToolCall { name: String, input: Value }, ToolResult { name: String, first_line: String, is_error: bool }, Finished { summary_first_line: String } }`(`Debug + Clone`)
  - `pub type ChildEventHook = Arc<dyn Fn(&ChildEvent) + Send + Sync>;`
  - `pub struct SubagentHooks { pub on_child_event: Option<ChildEventHook> }`
  - `SubagentRuntime::new` 末参由 `on_tool_result` 改为 `hooks: SubagentHooks`。
- 消费方:Task 4(`Renderer::child_event` + `main.rs` 接线)。

**关键语义**:本任务起,子 agent 的工具结果**不再经共享的 `on_tool_result`**,改由 `ChildEventKind::ToolResult` 承载。故 `SubagentRuntime` 不再需要 `on_tool_result` 字段。

- [ ] **Step 1: 写失败测试**

`lex-core/src/agent/subagent.rs` 的 `mod tests` 追加:

```rust
    /// 收集子事件用(测试夹具)
    #[derive(Default)]
    struct EventLog {
        events: Mutex<Vec<ChildEvent>>,
    }
    impl EventLog {
        fn hook(self: &Arc<Self>) -> crate::agent::ChildEventHook {
            let me = Arc::clone(self);
            Arc::new(move |ev: &ChildEvent| {
                me.events.lock().unwrap_or_else(|p| p.into_inner()).push(ev.clone());
            })
        }
        fn kinds(&self) -> Vec<String> {
            self.events
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .iter()
                .map(|e| match &e.kind {
                    ChildEventKind::Started { .. } => "started".to_string(),
                    ChildEventKind::ToolCall { .. } => "tool_call".to_string(),
                    ChildEventKind::ToolResult { .. } => "tool_result".to_string(),
                    ChildEventKind::Finished { .. } => "finished".to_string(),
                })
                .collect()
        }
    }

    fn runtime_with_hooks(
        provider: &ScriptedProvider,
        state: SpawnState,
        hooks: crate::agent::SubagentHooks,
    ) -> SubagentRuntime {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(crate::tools::file_read::FileRead));
        let throttled = crate::provider::throttle::ThrottledProvider::new(Arc::new(provider.clone()), 3);
        SubagentRuntime::with_depth(
            throttled,
            "主提示词前缀".into(),
            Arc::new(AllowHandler),
            crate::security::SecurityRules::defaults(),
            std::path::PathBuf::from("."),
            None,
            Arc::new(registry),
            SpawnLimits { max_turns: 10, context_limit: Some(64_000), max_children_per_turn: 4 },
            hooks,
            state,
            0,
        )
    }

    #[tokio::test]
    async fn emits_started_tool_call_tool_result_and_finished() {
        let provider = ScriptedProvider::default();
        // 子 agent:先调一次 file_read,再文本作答
        provider.push(vec![
            ProviderEvent::ToolUseComplete {
                id: "c1".into(),
                name: "file_read".into(),
                input: serde_json::json!({"path": "Cargo.toml"}),
            },
            completed(),
        ]);
        provider.push(text_reply("## 子任务摘要\n- **做了什么**:读了文件"));

        let log = Arc::new(EventLog::default());
        let rt = runtime_with_hooks(&provider, SpawnState::default(), crate::agent::SubagentHooks { on_child_event: Some(log.hook()) });
        rt.spawn(Req { task: "排查".into(), allowed_tools: vec!["file_read".into()], context: None, context_budget: None, allow_nested: false })
            .await
            .unwrap();

        // Started → ToolCall → ToolResult → Finished,顺序完整
        assert_eq!(log.kinds(), vec!["started", "tool_call", "tool_result", "finished"]);

        let events = log.events.lock().unwrap_or_else(|p| p.into_inner());
        assert!(events.iter().all(|e| e.child_id == 1 && e.depth == 1), "归属信息应一致(child_id=1, depth=1)");
        match &events[1].kind {
            ChildEventKind::ToolCall { name, input } => {
                assert_eq!(name, "file_read");
                assert_eq!(input.get("path").and_then(Value::as_str), Some("Cargo.toml"));
            }
            other => panic!("第 2 个事件应是 ToolCall,实际: {other:?}"),
        }
    }

    #[tokio::test]
    async fn child_text_and_usage_are_not_forwarded() {
        // 子 agent 的正文/思考/用量是它的内部过程,不得污染主输出。
        // 脚本里塞入 TextDelta 与 Completed(带非零用量),断言钩子只收到工具类事件。
        let provider = ScriptedProvider::default();
        provider.push(vec![
            ProviderEvent::ThinkingDelta("子 agent 的思考".into()),
            ProviderEvent::TextDelta("子 agent 的正文".into()),
            ProviderEvent::Completed { usage: crate::message::Usage { input_tokens: 999, output_tokens: 888, ..Default::default() } },
        ]);
        let log = Arc::new(EventLog::default());
        let rt = runtime_with_hooks(&provider, SpawnState::default(), crate::agent::SubagentHooks { on_child_event: Some(log.hook()) });
        rt.spawn(Req { task: "t".into(), allowed_tools: vec!["file_read".into()], context: None, context_budget: None, allow_nested: false })
            .await
            .unwrap();

        assert_eq!(log.kinds(), vec!["started", "finished"], "只应有首尾两条,正文/思考/用量一律不转发");
    }

    #[tokio::test]
    async fn concurrent_children_get_distinct_ids() {
        let provider = ScriptedProvider::default();
        provider.push(text_reply("## 子任务摘要\n- **做了什么**:一"));
        provider.push(text_reply("## 子任务摘要\n- **做了什么**:二"));

        let log = Arc::new(EventLog::default());
        let rt = Arc::new(runtime_with_hooks(&provider, SpawnState::default(), crate::agent::SubagentHooks { on_child_event: Some(log.hook()) }));
        let mk = || Req { task: "t".into(), allowed_tools: vec!["file_read".into()], context: None, context_budget: None, allow_nested: false };
        let (a, b) = tokio::join!(rt.spawn(mk()), rt.spawn(mk()));
        assert!(a.is_ok() && b.is_ok());

        let ids: std::collections::BTreeSet<u32> =
            log.events.lock().unwrap_or_else(|p| p.into_inner()).iter().map(|e| e.child_id).collect();
        assert_eq!(ids.len(), 2, "同一轮内两个子 agent 的 child_id 必须不同,否则并发交错时无法归属");
    }

    #[test]
    fn begin_turn_also_resets_child_ids() {
        let state = SpawnState::default();
        assert_eq!(state.next_child_id(), 1);
        assert_eq!(state.next_child_id(), 2);
        state.reset();
        assert_eq!(state.next_child_id(), 1, "每轮从 1 开始");
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `/mnt/c/Users/Administrator/.cargo/bin/cargo.exe test -p lex-core subagent`
Expected: FAIL — `ChildEvent` / `ChildEventKind` / `SubagentHooks` / `next_child_id` 不存在。

- [ ] **Step 3: 最小实现**

**3a. `lex-core/src/agent/mod.rs`** — 紧接 `ToolResultHook` 之后新增:

```rust
/// 子 agent 活动事件:承载归属信息 + 该子 agent 的关键动作。
/// 只携带渲染所需的最小信息,**绝不携带子 agent 的完整消息历史**
/// (正文/思考/用量都不在转发之列——它们是子 agent 的内部过程)。
#[derive(Debug, Clone)]
pub struct ChildEvent {
    /// 树内自增标识,与轮次计数同批重置 → 每轮从 1 开始
    pub child_id: u32,
    /// 该子 agent 所处深度(子 = 1,孙 = 2)
    pub depth: u32,
    pub kind: ChildEventKind,
}

#[derive(Debug, Clone)]
pub enum ChildEventKind {
    /// 子 agent 开始执行
    Started { task: String },
    /// 子 agent 发起一次工具调用(在其独立上下文内)
    ToolCall { name: String, input: serde_json::Value },
    /// 子 agent 的工具调用返回
    ToolResult { name: String, first_line: String, is_error: bool },
    /// 子 agent 结束(只带摘要首行)
    Finished { summary_first_line: String },
}

pub type ChildEventHook = Arc<dyn Fn(&ChildEvent) + Send + Sync>;

/// 子 agent 相关的回调集合。
///
/// 这里**没有** `on_tool_result`:父级自己的工具结果钩子直接交给 `AgentLoop`,
/// 不经 `SubagentRuntime` 转手;子级的工具结果改走 `on_child_event`(见设计 6.3)。
pub struct SubagentHooks {
    pub on_child_event: Option<ChildEventHook>,
}
```

**3b. `lex-core/src/agent/subagent.rs`** — `SpawnState` 加 id 分配:

```rust
#[derive(Clone, Default)]
pub(crate) struct SpawnState {
    spawned_this_turn: Arc<AtomicU32>,
    next_child_id: Arc<AtomicU32>,
}

impl SpawnState {
    /// 分配一个子 agent 标识(树内自增;reset 后从 1 重新开始)
    fn next_child_id(&self) -> u32 {
        self.next_child_id.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn reset(&self) {
        self.spawned_this_turn.store(0, Ordering::SeqCst);
        self.next_child_id.store(0, Ordering::SeqCst);
    }
    // spawned / try_admit 同 Task 2
}
```

`SubagentRuntime`:删掉 `on_tool_result` 字段,换成 `hooks: SubagentHooks`;`with_depth` 的对应形参也改成 `hooks: SubagentHooks`。

`spawn` 中,在构建子 loop 之前准备发射器并把 `Started` 发出去:

```rust
        // 子事件发射器:只捕获 Send + Sync + Copy 的捕获物,便于 clone 进两个回调
        let child_id = self.spawn_state.next_child_id();
        let child_depth = self.depth + 1;
        let hook = self.hooks.on_child_event.clone();
        let emit = move |kind: ChildEventKind| {
            if let Some(h) = &hook {
                h(&ChildEvent { child_id, depth: child_depth, kind });
            }
        };
        emit(ChildEventKind::Started { task: req.task.clone() });
```

子 `AgentLoop` 的两个回调改为:

```rust
            // 子 agent 的工具结果改走子事件通道,不再经父级的 on_tool_result ——
            // 既给结果行加上归属,又避免子级的 ⎿ 去减父级的 pending_tools 计数
            on_tool_result: self.hooks.on_child_event.as_ref().map(|_| {
                let emit = emit.clone();
                Arc::new(move |info: &crate::agent::ToolResultInfo| {
                    emit(ChildEventKind::ToolResult {
                        name: info.tool_name.clone(),
                        first_line: info.first_line.clone(),
                        is_error: info.is_error,
                    });
                }) as crate::agent::ToolResultHook
            }),
```

`run_turn` 的调用改为过滤闭包 + 收尾发 `Finished`:

```rust
        // 只转发工具调用事件;子 agent 的正文/思考/用量一概不转(见 ChildEvent 文档)
        let mut on_event = {
            let emit = emit.clone();
            move |e: &crate::provider::ProviderEvent| {
                if let crate::provider::ProviderEvent::ToolUseComplete { name, input, .. } = e {
                    emit(ChildEventKind::ToolCall { name: name.clone(), input: input.clone() });
                }
            }
        };
        let final_text = child.run_turn(&child_task_text(&req), &mut on_event).await?;
        emit(ChildEventKind::Finished { summary_first_line: crate::agent::first_line_of(&final_text) });
        Ok(final_text)
```

顶部 `use` 补 `crate::agent::{ChildEvent, ChildEventKind, SubagentHooks}`。

**3c. `lex-cli/src/main.rs`** — 构造调用末参改为(Task 4 接线前先传 `None`):

```rust
            lex_core::agent::SubagentHooks { on_child_event: None },
```

同时 `build_loop` 里原先为子 agent 传 `on_tool_result.clone()` 的那一处删除。

- [ ] **Step 4: 跑测试确认通过**

Run: `/mnt/c/Users/Administrator/.cargo/bin/cargo.exe test --workspace`
Expected: **174 passed, 0 failed**(170 + 4),零警告。

- [ ] **Step 5: Commit**

```bash
git add lex-core/src/agent/mod.rs lex-core/src/agent/subagent.rs lex-cli/src/main.rs
git commit -m "feat(agent): 结构化子事件 ChildEvent(归属 + 工具活动),子级结果改走独立通道"
```

---

### Task 4: CLI 子事件渲染与接线

**Files:**
- Modify: `lex-cli/src/ui/events.rs`(`Renderer::child_event`)
- Modify: `lex-cli/src/main.rs`(接线 `on_child_event`)
- Test: `lex-cli/src/ui/events.rs`(`mod tests`,新建)

**Interfaces:**
- Consumes: Task 3 的 `ChildEvent` / `ChildEventKind` / `ChildEventHook`。
- Produces: `pub fn Renderer::child_event(&mut self, ev: &ChildEvent)`。

- [ ] **Step 1: 写失败测试**

`lex-cli/src/ui/events.rs` 末尾新建测试模块(`summarize` 与 `Renderer` 的私有字段在同文件测试中可直接访问):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn ev(id: u32, kind: ChildEventKind) -> ChildEvent {
        ChildEvent { child_id: id, depth: 1, kind }
    }

    #[test]
    fn summarize_covers_spawn_subagent_task() {
        // 既有的 spawn_subagent 分支不得回归
        assert_eq!(summarize("spawn_subagent", &serde_json::json!({"task": "排查"})), "排查");
        assert_eq!(summarize("spawn_subagent", &serde_json::json!({})), "<未知任务>");
    }

    #[test]
    fn rendered_child_events_carry_attribution_and_finished_is_silent() {
        // 归属标记 [子N] 必须出现在每一条会输出的事件里;
        // Finished 不输出(其结局已由父级 ⎿ 行体现)
        let started = render_child_event(&ev(2, ChildEventKind::Started { task: "排查".into() }));
        assert!(started.as_deref().unwrap_or_default().contains("[子2]"), "Started: {started:?}");
        assert!(started.as_deref().unwrap_or_default().contains("排查"));

        let call = render_child_event(&ev(2, ChildEventKind::ToolCall {
            name: "file_read".into(),
            input: serde_json::json!({"path": "a.rs"}),
        }));
        assert!(call.as_deref().unwrap_or_default().contains("[子2]"), "ToolCall: {call:?}");
        assert!(call.as_deref().unwrap_or_default().contains("a.rs"), "应复用 summarize 的参数摘要: {call:?}");

        let ok = render_child_event(&ev(2, ChildEventKind::ToolResult {
            name: "file_read".into(), first_line: "读到了".into(), is_error: false,
        }));
        assert!(ok.as_deref().unwrap_or_default().contains("[子2]"), "ToolResult: {ok:?}");

        assert!(render_child_event(&ev(2, ChildEventKind::Finished { summary_first_line: "done".into() })).is_none(),
            "Finished 不应输出");
    }

    #[test]
    fn child_events_do_not_disturb_parent_pending_tools() {
        // 计数平衡:子 agent 的活动不得触碰父级的 pending_tools。
        // 旧实现下子级的 ⎿ 会减该计数却无 ● 来加,使父级 token 尾注提前打印。
        let mut r = Renderer::new();
        r.pending_tools = 2;
        r.child_event(&ev(1, ChildEventKind::Started { task: "排查".into() }));
        r.child_event(&ev(1, ChildEventKind::ToolCall { name: "file_read".into(), input: serde_json::json!({"path": "a"}) }));
        r.child_event(&ev(1, ChildEventKind::ToolResult { name: "file_read".into(), first_line: "ok".into(), is_error: false }));
        r.child_event(&ev(1, ChildEventKind::Finished { summary_first_line: "done".into() }));
        assert_eq!(r.pending_tools, 2, "子事件不得改动父级计数");
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `/mnt/c/Users/Administrator/.cargo/bin/cargo.exe test -p lex-cli child_event`
Expected: FAIL — `render_child_event` 不存在(E0425)。

- [ ] **Step 3: 最小实现**

`lex-cli/src/ui/events.rs`:顶部 `use lex_core::agent::{ChildEvent, ChildEventKind, ToolResultInfo};`,并在 `summarize` 之前新增纯函数:

```rust
/// 子 agent 事件的单行渲染。返回 None 表示该事件不输出。
/// 颜色只引用 theme.rs 常量(项目硬约束);归属由 [子N] 标签承载,
/// 整行用 DIM 以区别于父级的 ACCENT ●。
fn render_child_event(ev: &ChildEvent) -> Option<String> {
    match &ev.kind {
        ChildEventKind::Started { task } => {
            let first: String = task.lines().next().unwrap_or_default().chars().take(80).collect();
            Some(format!("  {}⤷ [子{}] 派生: {}{}", theme::DIM, ev.child_id, first, theme::RESET))
        }
        ChildEventKind::ToolCall { name, input } => Some(format!(
            "  {}● [子{}] {}({}){}",
            theme::DIM,
            ev.child_id,
            name,
            summarize(name, input),
            theme::RESET
        )),
        ChildEventKind::ToolResult { first_line, is_error, .. } => {
            let color = if *is_error { theme::ERROR } else { theme::DIM };
            Some(format!("  {}⎿ [子{}] {}{}", color, ev.child_id, first_line, theme::RESET))
        }
        // 子 agent 的结局已由父级那条 ⎿ 行(摘要首行)体现,再打一行是冗余噪声
        ChildEventKind::Finished { .. } => None,
    }
}
```

`impl Renderer` 中新增:

```rust
    /// 子 agent 活动行(由 ChildEventHook 回调驱动)
    pub fn child_event(&mut self, ev: &ChildEvent) {
        let Some(line) = render_child_event(ev) else { return };
        self.break_thinking();
        self.flush_markdown();
        anstream::println!("{line}");
    }
```

`lex-cli/src/main.rs` 的 `build_loop` 中,把 Task 3 留的 `on_child_event: None` 换成真实接线(在 `SubagentRuntime::new(...)` 之前):

```rust
    // 子 agent 活动行转发到共享渲染器(带 [子N] 归属)
    let on_child_event: Option<lex_core::agent::ChildEventHook> = {
        let r = std::sync::Arc::clone(renderer);
        Some(std::sync::Arc::new(move |ev: &lex_core::agent::ChildEvent| {
            r.lock().unwrap_or_else(|p| p.into_inner()).child_event(ev);
        }))
    };
```

并把构造实参改为 `lex_core::agent::SubagentHooks { on_child_event }`。

- [ ] **Step 4: 跑测试确认通过**

Run: `/mnt/c/Users/Administrator/.cargo/bin/cargo.exe test --workspace`
Expected: **177 passed, 0 failed**(174 + 3),零警告。

- [ ] **Step 5: Commit**

```bash
git add lex-cli/src/ui/events.rs lex-cli/src/main.rs
git commit -m "feat(cli): 子 agent 活动行渲染(⤷ 派生 / ● 调用 / ⎿ 结果,带 [子N] 归属)"
```

---

### Task 5: 告知模型 + 文档同步

**Files:**
- Modify: `lex-core/src/tools/spawn_subagent.rs`(结构体带上限,schema 描述写明)
- Modify: `lex-core/src/agent/subagent.rs`(嵌套注册处)
- Modify: `lex-cli/src/main.rs`(注册处)
- Modify: `assets/coding-agent-system-prompt.md`(第十节补一句)
- Modify: `README.md`(安全模型小节补两句)
- Modify: `AGENTS.md` + `CLAUDE.md`(**两份同源副本必须同步**)
- Test: `lex-core/src/tools/spawn_subagent.rs`、既有 e2e 回归

**Interfaces:**
- Consumes: Task 1 的 `cfg.agent.max_children_per_turn`、Task 2 的 `SpawnLimits`。
- Produces: `pub fn SpawnSubagent::new(max_children_per_turn: u32) -> Self`。

- [ ] **Step 1: 写失败测试**

`lex-core/src/tools/spawn_subagent.rs` 的 `mod tests` 追加:

```rust
    #[test]
    fn description_states_the_configured_per_turn_cap() {
        // 模型需要知道上限,否则会反复撞墙而不自知。
        // 写进工具 description —— 模型挑工具时读的就是它。
        let d7 = SpawnSubagent::new(7).description().to_string();
        assert!(d7.contains('7'), "描述应含配置的上限值: {d7}");
        assert!(d7.contains("每轮"), "应说明是按轮的: {d7}");

        // 上限为 0(禁止派生)时也要如实告知
        let d0 = SpawnSubagent::new(0).description().to_string();
        assert!(d0.contains('0'), "描述应如实反映 0: {d0}");
    }

    #[test]
    fn tool_metadata_unchanged_by_construction() {
        let t = SpawnSubagent::new(4);
        assert_eq!(t.name(), "spawn_subagent");
        assert!(!t.read_only());
        assert!(t.parallel_safe());
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `/mnt/c/Users/Administrator/.cargo/bin/cargo.exe test -p lex-core description_states_the_configured`
Expected: FAIL — `SpawnSubagent::new` 不存在(当前是单元结构体)。

- [ ] **Step 3: 最小实现**

**3a. `lex-core/src/tools/spawn_subagent.rs`**:结构体由单元结构改为带字段。

上限写进**工具 description**(模型挑工具时读的就是它),因此在 `new` 里预先拼好存起来——
`Tool::description(&self) -> &str` 返回借用,不能每次现拼 `String`:

```rust
/// spawn_subagent:把独立、边界清晰的子任务派发给独立上下文的子 agent。
/// 返回值只有子 agent 的结构化摘要,不带回子 agent 的完整消息历史(控制主上下文体积的关键)。
///
/// 每轮派生数量有上限(来自 `[agent] max_children_per_turn`,0 表示禁止派生),
/// 该上限写进 description 供模型自知,避免反复撞墙。
pub struct SpawnSubagent {
    description: String,
}

impl SpawnSubagent {
    pub fn new(max_children_per_turn: u32) -> Self {
        SpawnSubagent {
            description: format!(
                "把一个独立、边界清晰的多步骤子任务派发给拥有独立上下文的子 agent 执行,只返回结构化摘要。\
                 同一轮内最多派生 {max_children_per_turn} 个子 agent,超出会被拒绝——请优先把相近的排查合并成一个子任务。\
                 仅当子任务的执行长度/探索成本明显高于污染主上下文的代价时使用;琐碎单步操作不要派生。"
            ),
        }
    }
}
```

`impl Tool` 中把原 `description()` 的实现体替换为:

```rust
    fn description(&self) -> &str {
        &self.description
    }
```

`schema()` 与 `read_only()` / `parallel_safe()` / `name()` / `execute()` 均**不变**。
`execute()` 中的 `allowed_tools` 空校验、`spawner` 缺失降级等逻辑一律保持原样。

**3b. 全仓的 `SpawnSubagent` 构造点改为 `SpawnSubagent::new(N)`**:

```
lex-cli/src/main.rs                        注册主注册表处 → SpawnSubagent::new(cfg.agent.max_children_per_turn)
lex-core/src/agent/subagent.rs             嵌套注册处 → SpawnSubagent::new(self.limits.max_children_per_turn)
lex-core/src/agent/subagent.rs (tests)     两处注册 → SpawnSubagent::new(4)
lex-core/tests/subagent_e2e.rs             两处注册 → SpawnSubagent::new(4)
```

用 `grep -rn "SpawnSubagent" --include="*.rs" .` 确认无遗留的单元结构用法。

**3c. 文档**:

- `assets/coding-agent-system-prompt.md` 第十节「子 agent 派生守则」追加一条:
  `- 同一轮内派生的子 agent 总数有上限(见工具说明),超出会被拒绝;相近的排查请合并成一个子任务。`
- `README.md` 安全模型小节补:子 agent 权限等级继承父级(同一规则表 + 同一确认处理器);派生深度硬上限 2 层;每轮派生数量上限见 `[agent] max_children_per_turn`。
- `AGENTS.md` 与 `CLAUDE.md`(**两份必须逐字节同步**,`lex-core/tests/docs_consistency.rs` 会强制):tools 清单说明补 `spawn_subagent` 的每轮上限;`[agent]` 段写入配置说明;roadmap 补本次改动要点。

- [ ] **Step 4: 跑测试确认通过**

Run: `/mnt/c/Users/Administrator/.cargo/bin/cargo.exe test --workspace`
Expected: **179 passed, 0 failed**(177 + 2),零警告。

- [ ] **Step 5: Commit**

```bash
git add lex-core/src/tools/spawn_subagent.rs lex-core/src/agent/subagent.rs lex-core/tests/subagent_e2e.rs lex-cli/src/main.rs assets/coding-agent-system-prompt.md README.md AGENTS.md CLAUDE.md
git commit -m "feat(tools): spawn_subagent 告知每轮派生上限;文档与提示词同步"
```

---

## 验收对照(规格章节 → 任务)

| 规格章节 | 落点 |
|---|---|
| §3 配置面(`[agent]` 段,默认 4,0=禁止) | Task 1 |
| §4.1 计数与判定(先加后判、越限回滚) | Task 2(`SpawnState::try_admit`) |
| §4.2 错误与降级 | Task 2(文案含配置项名) |
| §4.3 每轮重置 + depth 设闸 | Task 2(`begin_turn` + `child_begin_turn_does_not_clear_parent_counter`) |
| §4.4 口径(整树在根一轮内) | Task 2(树内共享 `SpawnState`) |
| §5 构造函数收敛 | Task 2(`SpawnLimits`)、Task 3(`SubagentHooks`) |
| §6.1 类型 | Task 3 |
| §6.2 发射点(四个) | Task 3 |
| §6.3 子级结果改走独立通道 | Task 3 |
| §6.4 标识分配(与轮次同批重置) | Task 3(`next_child_id` + `reset`) |
| §7 CLI 渲染 | Task 4 |
| §8 系统提示词 + schema 告知 | Task 5 |
| §9 测试策略 | 各任务的 Step 1 |
| §10 影响文件 | 全部任务 |
| §11 明确不做 | 计划内无对应任务(刻意) |

## 已知代价(规格 §8 已确认)

改动 `spawn_subagent` 的 schema 描述会变更请求 payload 的 tools 段,使 DeepSeek 前缀缓存在升级后**失效一次**(描述是编译期/构造期字面量,此后字节稳定);持续成本约 +20 token/请求。
