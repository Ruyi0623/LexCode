use crate::message::Usage;
use crate::provider::RequestContext;
use std::sync::Mutex;

/// 缓存策略介入点(设计文档 4.4):
/// - `prepare`:请求组装点介入;此处仅做基线记录与前缀校验,不改写请求内容
/// - `observe`:从响应 Usage 采集命中率
/// - `invalidate`:压缩后"缓存链断开"信号(重置基线,不做恢复)
///
/// 实现内部状态用 &self + 内部可变性,便于跨 await 共享。
pub trait CacheStrategy: Send + Sync {
    fn prepare(&self, ctx: &RequestContext);
    fn observe(&self, usage: &Usage);
    fn invalidate(&self);
    /// 遥测:前缀不一致次数(默认 0,供上层观测)
    fn violations(&self) -> u64 {
        0
    }
    /// 遥测:缓存链断开次数(默认 0,供上层观测)
    fn invalidations(&self) -> u64 {
        0
    }
}

#[derive(Default)]
struct Baseline {
    system: Option<Vec<u8>>,
    tools: Option<Vec<u8>>,
    /// 逐条消息序列化的字节串(整体 JSON 数组序列化会因结尾 `]` 破坏前缀性质)
    messages: Option<Vec<Vec<u8>>>,
    violations: u64,
    invalidations: u64,
}

/// DeepSeek 隐式前缀缓存策略:不插任何缓存标记,靠 KV Cache 的前缀命中特性。
/// prepare 时将 system / tools / messages 段分别序列化为字节串并与上次请求比对:
/// - system / tools 必须逐字节一致(变化即报不一致)
/// - messages 必须是上次的严格前缀追加(改写即报不一致)
/// 不一致只记 warning 与遥测计数,不阻断请求。序列化基于 serde_json 确定性输出
/// (字段顺序 = derive 声明顺序,动态 JSON 用 Value 保序),同一请求重复序列化字节稳定。
pub struct ImplicitPrefixCacheStrategy {
    baseline: Mutex<Baseline>,
}

impl ImplicitPrefixCacheStrategy {
    pub fn new() -> Self {
        ImplicitPrefixCacheStrategy { baseline: Mutex::new(Baseline::default()) }
    }

    /// 遥测计数:前缀不一致次数(供测试与上层观测)
    pub fn violations(&self) -> u64 {
        self.baseline.lock().unwrap_or_else(|p| p.into_inner()).violations
    }

    /// 遥测计数:缓存链断开(invalidate)次数
    pub fn invalidations(&self) -> u64 {
        self.baseline.lock().unwrap_or_else(|p| p.into_inner()).invalidations
    }
}

impl Default for ImplicitPrefixCacheStrategy {
    fn default() -> Self {
        Self::new()
    }
}

fn serialize<T: serde::Serialize>(value: &T) -> Option<Vec<u8>> {
    serde_json::to_vec(value).ok()
}

/// 列表级严格前缀:new 的前 old.len() 个元素与 old 逐字节一致(允许纯追加)
fn is_list_prefix(old: &[Vec<u8>], new: &[Vec<u8>]) -> bool {
    new.len() >= old.len() && new[..old.len()] == old[..]
}

impl CacheStrategy for ImplicitPrefixCacheStrategy {
    fn prepare(&self, ctx: &RequestContext) {
        let system = ctx.system.as_bytes().to_vec();
        let tools = serialize(&ctx.tools);
        let messages: Option<Vec<Vec<u8>>> = ctx.messages.iter().map(serialize).collect();
        let mut base = self.baseline.lock().unwrap_or_else(|p| p.into_inner());

        if base.system.is_some() {
            let mut problems: Vec<&'static str> = Vec::new();
            if base.system.as_deref() != Some(system.as_slice()) {
                problems.push("system 段变化(破坏前缀缓存)");
            }
            if base.tools != tools {
                problems.push("tools 段变化(破坏前缀缓存)");
            }
            match (&base.messages, &messages) {
                (Some(old), Some(new)) if is_list_prefix(old, new) => {}
                (Some(_), Some(_)) => problems.push("messages 非严格前缀追加(历史被改写)"),
                (Some(_), None) => problems.push("messages 序列化失败"),
                _ => {}
            }
            if !problems.is_empty() {
                base.violations += 1;
                tracing::warn!(violations = base.violations, reasons = ?problems, "前缀缓存一致性校验未通过(不阻断请求)");
            }
        }

        base.system = Some(system);
        base.tools = tools;
        base.messages = messages;
    }

    fn observe(&self, usage: &Usage) {
        let total = usage.cache_hit_tokens + usage.cache_miss_tokens;
        if total == 0 {
            return; // 端点未回缓存字段,无从观测
        }
        let hit_rate = usage.cache_hit_tokens as f64 / total as f64 * 100.0;
        tracing::info!(
            hit = usage.cache_hit_tokens,
            miss = usage.cache_miss_tokens,
            hit_rate = format!("{hit_rate:.1}%").as_str(),
            "前缀缓存命中率"
        );
    }

    fn invalidate(&self) {
        let mut base = self.baseline.lock().unwrap_or_else(|p| p.into_inner());
        base.invalidations += 1;
        base.system = None;
        base.tools = None;
        base.messages = None;
        tracing::warn!(invalidations = base.invalidations, "缓存链断开(压缩/基线重置),下轮请求重新建立前缀");
    }

    fn violations(&self) -> u64 {
        ImplicitPrefixCacheStrategy::violations(self)
    }

    fn invalidations(&self) -> u64 {
        ImplicitPrefixCacheStrategy::invalidations(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Block, Message};
    use crate::tools::ToolDefinition;

    fn ctx(system: &str, messages: Vec<Message>) -> RequestContext {
        RequestContext { system: system.into(), tools: vec![], messages }
    }

    fn turn1() -> Vec<Message> {
        vec![Message::user_text("第一轮")]
    }

    fn appended() -> Vec<Message> {
        vec![
            Message::user_text("第一轮"),
            Message::assistant(vec![Block::Text { text: "回复".into() }]),
            Message::user_text("第二轮"),
        ]
    }

    #[test]
    fn pure_append_passes_without_violation() {
        let s = ImplicitPrefixCacheStrategy::new();
        s.prepare(&ctx("sys", turn1()));
        s.prepare(&ctx("sys", appended()));
        assert_eq!(s.violations(), 0);
    }

    #[test]
    fn system_change_is_violation() {
        let s = ImplicitPrefixCacheStrategy::new();
        s.prepare(&ctx("sys", turn1()));
        s.prepare(&ctx("sys2", appended()));
        assert_eq!(s.violations(), 1);
    }

    #[test]
    fn tools_change_is_violation() {
        let s = ImplicitPrefixCacheStrategy::new();
        s.prepare(&ctx("sys", turn1()));
        let mut with_tool = ctx("sys", appended());
        with_tool.tools = vec![ToolDefinition {
            name: "file_read".into(),
            description: "d".into(),
            input_schema: serde_json::json!({"type":"object"}),
        }];
        s.prepare(&with_tool);
        assert_eq!(s.violations(), 1);
    }

    #[test]
    fn history_rewrite_is_violation() {
        let s = ImplicitPrefixCacheStrategy::new();
        s.prepare(&ctx("sys", turn1()));
        // 基线内的第一条被改写:前缀不再成立
        let rewritten = vec![
            Message::user_text("第一轮(被改写)"),
            Message::assistant(vec![Block::Text { text: "回复".into() }]),
        ];
        s.prepare(&ctx("sys", rewritten));
        assert_eq!(s.violations(), 1);
    }

    #[test]
    fn shorter_history_is_rewrite_violation() {
        let s = ImplicitPrefixCacheStrategy::new();
        s.prepare(&ctx("sys", appended()));
        s.prepare(&ctx("sys", turn1()));
        assert_eq!(s.violations(), 1);
    }

    #[test]
    fn invalidate_resets_baseline() {
        let s = ImplicitPrefixCacheStrategy::new();
        s.prepare(&ctx("sys", turn1()));
        s.invalidate();
        assert_eq!(s.invalidations(), 1);
        // 基线已重置:重新记录,不算违例
        s.prepare(&ctx("sys", turn1()));
        assert_eq!(s.violations(), 0);
        s.prepare(&ctx("sys", appended()));
        assert_eq!(s.violations(), 0);
    }

    #[test]
    fn first_prepare_never_violates() {
        let s = ImplicitPrefixCacheStrategy::new();
        s.prepare(&ctx("sys", turn1()));
        assert_eq!(s.violations(), 0);
    }

    #[test]
    fn observe_ignores_missing_cache_fields() {
        let s = ImplicitPrefixCacheStrategy::new();
        // total == 0(端点未回缓存字段)不应 panic
        s.observe(&Usage::default());
    }
}
