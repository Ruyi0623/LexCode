# 子 agent 成本护栏 + 结构化子事件 设计文档

> 日期:2026-09-19
> 状态:已与需求方逐节确认(设计在对话中逐节过审)
> 前置:Phase 6 模块一(sub-agent 派生机制)已完成,见 `docs/superpowers/plans/2026-09-13-phase6-subagent.md`
> 来源:Phase 6 模块一最终整分支审查的 Recommendations #5 与 #7

## 1. 背景与目标

Phase 6 模块一让主循环可通过 `spawn_subagent` 把子任务派发给子 agent,并有深度硬上限 `MAX_SPAWN_DEPTH = 2`。但留下两个缺口:

1. **成本无界**:单轮能派生多少子 agent,目前唯一的约束是 Confirm 级确认提示。模型若反复串行派生(每次一个、等完再派下一个),总量无上限——`ThrottledProvider` 只限**并发**在途流(3 条),不限**总量**。
2. **子 agent 活动不可归属**:子 agent 的事件被丢弃(`agent/subagent.rs` 传 `&mut |_e| {}`),故终端只见 `⎿` 结果行、不见 `●` 活动行。且这造成**计数失衡**——子级的 `⎿` 经共享的 `on_tool_result` 减父级 `pending_tools`,却没有对应的 `●` 来加,使父级 token 尾注提前打印。

本设计同时收口 TUI 模块会再次撞上的那个接缝:**子 agent 如何呈现**。

## 2. 已确认的关键决策

| 决策点 | 结论 |
|---|---|
| 护栏口径 | **每轮上限**(非会话总量、非仅并发) |
| 重置机制 | `SubagentSpawner` trait 加 `begin_turn()` 生命周期钩子 |
| 上限来源 | **可配** `[agent] max_children_per_turn`,默认 `4` |
| 转发形态 | **结构化子事件钩子**(非"给现有钩子加标签") |
| 子级工具结果通道 | **改走子事件通道**,不再走共享的 `on_tool_result` |
| 是否告知模型 | **告知**——写入 `spawn_subagent` 的 schema 描述 |

## 3. 配置面

在 `lex-code.toml` 新增 `[agent]` 段(与既有 `[context]` / `[security]` / `[shell]` 同级):

```toml
[agent]
max_children_per_turn = 4   # 每轮最多派生多少个子 agent;0 = 禁止派生
```

- 默认值 `4`。
- `0` 明确表示**禁止派生**,不表示"无限"——不留歧义。
- 不设"无限"取值;需要放宽就写一个足够大的数。
- 与既有 `max_turns` 可配的风格一致:成本容忍度因人/因模型而异,故给旋钮而非硬编码。
- 环境变量覆盖遵循既有优先级(env `LEX_*` > 项目 `lex-code.toml` > 内置默认)。

## 4. 成本护栏

### 4.1 计数与判定

`SubagentRuntime` 持有一个 `Arc<AtomicU32>` 计数器,整棵派生树**共享同一个**(构造子 runtime 时 clone)。

`spawn` 的判定(在 `allowed_tools` 校验与深度守卫**之后**、真正构建子 loop **之前**):

```
1. fetch_add(1) → 得到 new
2. 若 new > max_children_per_turn:fetch_sub(1) 回滚,返回 Err
3. 否则继续
```

用"先加后判、越限回滚"而非 CAS 循环的理由:两个并发派生不会双双越限(各自的 `new` 都已包含对方),而回滚保证被拒的派生不占用配额。语义上等价于原子的"若未满则占位"。

### 4.2 错误与降级

上限命中时返回 `LexError::Tool`,文案须可操作:

```
本轮派生子 agent 已达上限 {N}(可在 lex-code.toml 的 [agent] max_children_per_turn 调整)
```

经 `execute_tool_call` 降级为 `ToolResult{is_error: true}` 回填模型,**不上抛、不中断主循环**——与既有的权限拒绝/工具失败降级路径一致,模型可自行接手该子任务。

### 4.3 每轮重置(设计中最易写错处)

`SubagentSpawner` trait 新增生命周期钩子:

```rust
/// 新一轮开始的信号。实现方按需重置轮次级状态;默认无操作。
fn begin_turn(&self) {}
```

- `SubagentRuntime` 覆写为:**仅当 `self.depth == 0`** 时把计数器清零。
- `AgentLoop::run_turn` 在既有的 `self.security.reset_turn()` 旁调用:
  ```rust
  if let Some(spawner) = &self.tool_ctx.spawner { spawner.begin_turn(); }
  ```

**为什么必须按 depth 设闸**:子 agent 的 `run_turn` 也会调用 `begin_turn`,而子 runtime 共享父级的计数器。若不设闸,子 agent 每跑一轮就会**清空父级的当轮计数**,护栏被静默架空。子 runtime 的 `depth ≥ 1`,故 no-op。

### 4.4 口径

计的是**整棵派生树在根的一轮内**的派生总数,不是每个父级各自计数。这才是成本上界:根一轮内无论由谁发起,总派生数不超过上限。

## 5. 构造函数收敛

`SubagentRuntime::new` 现有 10 个参数并已挂 `#[allow(clippy::too_many_arguments)]`;加入护栏与子事件钩子后将达 12 个。按"改到哪儿就顺手修好哪儿"收敛为两组参数对象:

```rust
pub struct SpawnLimits {
    pub max_turns: u32,
    pub context_limit: Option<u32>,
    pub max_children_per_turn: u32,
}

pub struct SubagentHooks {
    /// 子 agent 活动事件回调(CLI/TUI 渲染用)。
    ///
    /// 注意这里**没有** `on_tool_result`:父级自己的工具结果钩子直接交给
    /// `AgentLoop`,不经 `SubagentRuntime` 转手;子级的工具结果则改走
    /// `on_child_event`(见 6.3)。故 runtime 无需该字段——放进来会是死字段。
    pub on_child_event: Option<ChildEventHook>,
}

impl SubagentRuntime {
    pub fn new(
        provider: ThrottledProvider,
        base_system: String,
        handler: Arc<dyn PermissionHandler>,
        rules: SecurityRules,
        cwd: PathBuf,
        shell: Option<ShellCommand>,
        base_registry: Arc<ToolRegistry>,
        limits: SpawnLimits,
        hooks: SubagentHooks,
    ) -> Self
}
```

9 个参数,`#[allow(clippy::too_many_arguments)]` 可移除。`with_depth` 仍为 `pub(crate)`,接受额外 `depth` 与共享计数器/钩子。

## 6. 结构化子事件

### 6.1 类型(`lex-core`,中立表示)

```rust
/// 子 agent 活动事件:承载归属信息 + 该子 agent 的关键动作。
/// 只携带渲染所需的最小信息,绝不携带子 agent 的完整消息历史。
#[derive(Debug, Clone)]
pub struct ChildEvent {
    /// 树内自增标识,与护栏计数器同批重置 → 每轮从 1 开始
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
    ToolCall { name: String, input: Value },
    /// 子 agent 的工具调用返回
    ToolResult { name: String, first_line: String, is_error: bool },
    /// 子 agent 结束(只带摘要首行,不带子历史)
    Finished { summary_first_line: String },
}

pub type ChildEventHook = Arc<dyn Fn(&ChildEvent) + Send + Sync>;
```

### 6.2 发射点

| 事件 | 发射位置 |
|---|---|
| `Started` | `SubagentRuntime::spawn` 中,构建子 loop 之后、`run_turn` 之前 |
| `ToolCall` | 传给子 `run_turn` 的**过滤闭包**:只匹配 `ProviderEvent::ToolUseComplete` |
| `ToolResult` | 传给子 `AgentLoop.on_tool_result` 的**派生钩子**(闭包捕获 `child_id`/`depth`) |
| `Finished` | `spawn` 中 `run_turn` 返回之后(只取摘要首行) |

**子 agent 的正文、思考、用量一律不转发**——它们是子 agent 的内部过程,不该污染主输出。端到端 e2e 已断言父级只拿到摘要;本设计延续该边界,只多转发"可归属的活动行"。

### 6.3 子级工具结果改走独立通道(关键取舍)

子 agent 的工具结果**不再走共享的 `on_tool_result`**,改由 `ChildEventKind::ToolResult` 单一通道承载。

理由两条:

1. **归属**:`ToolResultInfo` 不含任何子级标识,复用它会迫使给该 struct 加字段,且父/子语义混在一个通道里。
2. **顺带修掉计数失衡**:`Renderer.pending_tools` 在 `ToolUseComplete` 时 `+1`、在 `tool_result` 时 `-1`。子级目前只减不加(其 `●` 被丢弃),使计数器提前归零、token 尾注在子 agent 仍在跑时就打印。改走独立通道后**父级计数器完全不被触碰**——比"转发 `●` 去配平"更干净。

实现方式:子 `AgentLoop` 的 `on_tool_result` 传一个派生钩子(捕获 `child_id`/`depth`,内部发 `ChildEvent`),**不**传父级的 `on_tool_result`。

### 6.4 标识分配

`Arc<AtomicU32>` 的 id 计数器,与 4.1 的护栏计数器同属"轮次级状态",由 `begin_turn` 同批重置 → 每轮从 `子1` 开始。同一轮内的多个子 agent 得到不同 id,故并发交错时仍可归属。

## 7. CLI 渲染

`lex-cli/src/ui/events.rs` 的 `Renderer` 新增:

```rust
pub fn child_event(&mut self, ev: &ChildEvent)
```

渲染形态(缩进一级以区别于父级的顶层活动行):

| 事件 | 输出 |
|---|---|
| `Started` | `  ⤷ [子1] 派生: <task 首行,截断>` |
| `ToolCall` | `  ● [子1] <name>(<summarize 摘要>)` |
| `ToolResult` | `  ⎿ [子1] <result 首行>`(错误时用错误色) |
| `Finished` | **不输出**——子 agent 的结局已由父级那条 `⎿` 行(摘要首行)体现,再打一行是冗余噪声 |

`Finished` 虽然 CLI 不渲染,仍在类型与发射点中保留:`Started` 若无配对的 `Finished`,事件流就是**不完整生命周期**,任何消费者都被迫从别的信号去推断子 agent 何时结束(TUI 的子 agent 面板尤其需要)。保留它成本是一个 variant,省掉的是下游的推断逻辑。

- 复用既有 `summarize`(含已补的 `spawn_subagent` 分支)。
- **颜色只从 `ui/theme.rs` 取**——项目硬约束,其他文件不得裸 `\x1b[`。**无需新增常量**:父级的 `●` 用 `ACCENT`(蓝),子级整行用 `DIM` 即视觉上"次级",`[子N]` 标签本身已做归属区分;子级错误行用 `ERROR`。已核对现有色板(`ACCENT`/`DIM`/`ERROR`/`RESET`)足够。
- 中断/raw mode 期间换行仍须遵守既有的 `\r\n` 约束(纯文本路径)。

## 8. 系统提示词

在 `assets/coding-agent-system-prompt.md` 的 `spawn_subagent` 使用守则(第十节)中补一句上限说明,并在 `spawn_subagent` 的 **schema 描述**里写明"每轮最多 N 个"——否则模型会反复撞墙而不自知。

**已知代价**:改动 schema 描述会变更请求 payload 的 tools 段,使 DeepSeek 前缀缓存在升级后失效**一次**(描述是编译期字面量,此后字节稳定);持续成本约 `+20 token/请求`。与 Phase 6 模块一后续批次同样的取舍,已确认可接受。

注意 `AGENTS.md` 与 `CLAUDE.md` 是同源副本,须同步修改(已有 `lex-core/tests/docs_consistency.rs` 强制)。

## 9. 测试策略

**护栏**
- 上限内放行;达到上限后拒绝,且文案含上限值与配置项名
- 被拒的派生**回滚计数**(连续被拒不会累积占位)
- 并发派生不越限(多任务同时 spawn,成功数 ≤ 上限)
- **子 agent 的 `run_turn` 不清空父级的当轮计数** —— 设计中最易写错处(4.3),须有专项测试:构造 depth ≥ 1 的 runtime,调 `begin_turn()`,断言计数器未变
- `max_children_per_turn = 0` 时一律拒绝

**子事件**
- `Started` / `ToolCall` / `ToolResult` / `Finished` 四个发射点各一测
- **子 agent 的正文与用量不被转发**(脚本化 provider 发 TextDelta + Completed,断言钩子只收到工具类事件)
- 同一轮内多个子 agent 的 `child_id` 互不相同
- 钩子为 `None` 时零开销且不 panic

**计数平衡**
- 父级 `pending_tools` 在子 agent 活动期间不受影响(子级事件不触碰它)

**配置**
- `[agent]` 段解析、缺省值 `4`、显式 `0`

**回归**
- 既有 163 个测试不得回归;`cargo test --workspace` 零警告

## 10. 影响文件

```
lex-core/src/tools/mod.rs        SubagentSpawner 加 begin_turn 默认方法
lex-core/src/agent/mod.rs        run_turn 调用 begin_turn;导出 ChildEvent 等新类型
lex-core/src/agent/subagent.rs   主体:SpawnLimits/SubagentHooks、护栏、事件发射
lex-core/src/tools/spawn_subagent.rs  schema 描述补上限说明
lex-core/src/config.rs           新增 [agent] 段与默认值
lex-cli/src/main.rs              装配:SpawnLimits/SubagentHooks 注入 + 子事件钩子接线
lex-cli/src/ui/events.rs         Renderer::child_event
                                 (theme.rs 无需改动,复用既有 ACCENT/DIM/ERROR)
README.md                        配置表补 [agent] 段
AGENTS.md + CLAUDE.md            同步(roadmap 与架构小节)
assets/coding-agent-system-prompt.md  守则补上限说明
```

## 11. 明确不做

- **不做**会话总量上限(本设计是每轮上限;若日后需要,可另加一个计数器,与轮次计数器并列)
- **不做**子 agent 的正文/思考/用量转发(只转发可归属的工具活动)
- **不做**按父级各自计数(只计整树在根一轮内的总数)
- **不做**子 agent 活动的折叠/展开等交互(TUI 模块的事)
- **不改** `MAX_SPAWN_DEPTH`(深度是语义约束,非成本约束,维持硬编码)
