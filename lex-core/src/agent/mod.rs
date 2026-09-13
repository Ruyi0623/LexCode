use crate::context::compress;
use crate::error::{LexError, Result};
use crate::message::{Block, Message};
use crate::provider::cache::CacheStrategy;
use crate::provider::{Provider, ProviderEvent, RequestContext};
use crate::security::{execute_tool_call, PermissionHandler, SecurityGuard};
use crate::tools::{ToolContext, ToolRegistry};
use futures::StreamExt;
use std::sync::Arc;

/// 状态机状态(带 tracing 日志;Phase 1 无持久化状态)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    AwaitingInput,
    AssemblingRequest,
    AwaitingModel,
    ExecutingTools,
}

pub struct AgentLoop {
    pub provider: Box<dyn Provider>,
    pub registry: ToolRegistry,
    pub handler: Box<dyn PermissionHandler>,
    pub tool_ctx: ToolContext,
    pub security: SecurityGuard,
    pub system: String,
    pub history: Vec<Message>,
    pub max_turns: u32,
    /// 缓存策略(前缀校验 + 命中率遥测);None 则不做缓存观测
    pub cache_strategy: Option<Arc<dyn CacheStrategy>>,
    /// 上下文 token 上限(本地估算);None 则关闭压缩
    pub context_limit: Option<u32>,
    /// 压缩产生的摘要,并入下一条用户消息(避免连续两条 user 破坏角色交替)
    /// 内部状态由 run_turn 维护,构造时置 None/false 即可
    pub pending_summary: Option<String>,
    /// 会话内压缩只触发一次(设计:跨阈值触发一次,不每轮反复)
    pub compress_attempted: bool,
}

impl AgentLoop {
    /// 执行一轮用户输入:流式转发模型事件、执行工具、回填结果,直到产出无 tool_use 的文本回复。
    pub async fn run_turn(
        &mut self,
        user_input: &str,
        on_event: &mut dyn FnMut(&ProviderEvent),
    ) -> Result<String> {
        self.maybe_compress().await;
        // 压缩摘要并入本轮用户消息:保证角色交替不中断(Anthropic 端点要求)
        match self.pending_summary.take() {
            Some(summary) => {
                self.history
                    .push(Message::user_text(format!("[前期对话摘要]\n{summary}\n\n[本轮任务]\n{user_input}")));
            }
            None => self.history.push(Message::user_text(user_input)),
        }
        // 当轮安全状态复位:敏感文件外发启发式以"同一轮"为判定窗口
        self.security.reset_turn();
        let mut turns: u32 = 0;

        loop {
            turns += 1;
            if turns > self.max_turns {
                return Err(LexError::Provider(format!("已达到单轮任务最大循环次数 {}(疑似模型反复调用工具未收敛)", self.max_turns)));
            }

            let state_msg = |s: State| tracing::debug!(?s, "agent 状态");
            state_msg(State::AssemblingRequest);
            let ctx = RequestContext {
                system: self.system.clone(),
                tools: self.registry.definitions(),
                messages: self.history.clone(),
            };
            if let Some(cache) = &self.cache_strategy {
                cache.prepare(&ctx);
            }

            state_msg(State::AwaitingModel);
            let mut stream = self.provider.send(ctx).await?;

            let mut text_parts: Vec<String> = Vec::new();
            let mut thinking_parts: Vec<String> = Vec::new();
            let mut tool_uses: Vec<(String, String, serde_json::Value)> = Vec::new(); // (id, name, input)

            // BoxStream 是 Pin<Box<_>>,自身 Unpin,可直接 next()
            while let Some(item) = stream.next().await {
                match item? {
                    ProviderEvent::TextDelta(t) => {
                        text_parts.push(t.clone());
                        on_event(&ProviderEvent::TextDelta(t));
                    }
                    ProviderEvent::ThinkingDelta(t) => {
                        thinking_parts.push(t.clone());
                        on_event(&ProviderEvent::ThinkingDelta(t));
                    }
                    ProviderEvent::ToolUseStart { id, name } => on_event(&ProviderEvent::ToolUseStart { id, name }),
                    ProviderEvent::ToolUseDelta { id, partial_json } => on_event(&ProviderEvent::ToolUseDelta { id, partial_json }),
                    ProviderEvent::ToolUseComplete { id, name, input } => {
                        on_event(&ProviderEvent::ToolUseComplete { id: id.clone(), name: name.clone(), input: input.clone() });
                        tool_uses.push((id, name, input));
                    }
                    ProviderEvent::Completed { usage } => {
                        if let Some(cache) = &self.cache_strategy {
                            cache.observe(&usage);
                        }
                        on_event(&ProviderEvent::Completed { usage });
                    }
                }
            }

            if !tool_uses.is_empty() {
                // assistant 消息回填:thinking + text + tool_use 都要原样保留,
                // DeepSeek 兼容端点要求 thinking 历史回传(顺序:thinking 在前)
                let mut blocks: Vec<Block> = Vec::new();
                if !thinking_parts.is_empty() {
                    blocks.push(Block::Thinking { reasoning_content: thinking_parts.join("") });
                }
                if !text_parts.is_empty() {
                    blocks.push(Block::Text { text: text_parts.join("") });
                }
                for (id, name, input) in &tool_uses {
                    blocks.push(Block::ToolUse { id: id.clone(), name: name.clone(), input: input.clone() });
                }
                self.history.push(Message::assistant(blocks));

                state_msg(State::ExecutingTools);
                let mut results: Vec<(&str, String, bool)> = Vec::new();
                // 并发规则:全部只读 → join_all 并发;混入有副作用的工具 → 严格串行
                let all_read_only = tool_uses.iter().all(|(_, name, _)| {
                    self.registry.get(name).map(|t| t.read_only()).unwrap_or(false)
                });
                if all_read_only && tool_uses.len() > 1 {
                    let blocks = futures::future::join_all(tool_uses.iter().map(|(id, name, input)| {
                        execute_tool_call(&self.registry, self.handler.as_ref(), &self.tool_ctx, &self.security, id, name, input.clone())
                    }))
                    .await;
                    for ((id, _, _), block) in tool_uses.iter().zip(blocks) {
                        if let Block::ToolResult { content, is_error, .. } = block {
                            results.push((id.as_str(), content, is_error));
                        }
                    }
                } else {
                    for (id, name, input) in &tool_uses {
                        let block = execute_tool_call(&self.registry, self.handler.as_ref(), &self.tool_ctx, &self.security, id, name, input.clone()).await;
                        if let Block::ToolResult { content, is_error, .. } = block {
                            results.push((id.as_str(), content, is_error));
                        }
                    }
                }
                self.history.push(Message::tool_results(results));
                continue; // 回到 AssemblingRequest,自动多轮
            }

            state_msg(State::AwaitingInput);
            let final_text = text_parts.join("");
            let mut final_blocks: Vec<Block> = Vec::new();
            if !thinking_parts.is_empty() {
                final_blocks.push(Block::Thinking { reasoning_content: thinking_parts.join("") });
            }
            final_blocks.push(Block::Text { text: final_text.clone() });
            self.history.push(Message::assistant(final_blocks));
            return Ok(final_text);
        }
    }

    /// 上下文压缩:本地估算(字符÷4)跨过 `limit × 0.8` 时,对早期历史生成一次摘要。
    /// 会话内只触发一次;失败降级为保留原历史并记 warning,不影响本轮任务。
    async fn maybe_compress(&mut self) {
        if self.compress_attempted {
            return;
        }
        let Some(limit) = self.context_limit else { return };
        let estimate = compress::estimate_tokens(&self.history);
        let threshold = limit as u64 * 8 / 10;
        if estimate <= threshold {
            return;
        }
        let Some(cut) = compress::find_cut_index(&self.history) else {
            // 尚无可摘要的完整轮次:不消耗会话内唯一一次压缩机会,留待历史成型后再触发
            tracing::warn!(estimate, threshold, "上下文超阈值但无可安全切分的历史,本轮跳过压缩");
            return;
        };
        let early: Vec<Message> = self.history.drain(..cut).collect();
        match compress::summarize(self.provider.as_ref(), &early).await {
            Ok(summary) => {
                // 成功即消耗:会话内不再触发(设计:不每轮反复压缩)
                self.compress_attempted = true;
                self.pending_summary = Some(summary);
                tracing::info!(
                    summarized = early.len(),
                    kept = self.history.len(),
                    estimate_before = estimate,
                    "历史压缩完成,摘要将并入下一条用户消息"
                );
                // 压缩改写了历史前缀:缓存链断开(仅遥测,无恢复逻辑)
                if let Some(cache) = &self.cache_strategy {
                    cache.invalidate();
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "历史压缩失败,保留原历史继续");
                self.history.splice(..0, early);
            }
        }
    }
}
