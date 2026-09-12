pub mod anthropic_types;
pub mod sse;

use crate::error::Result;
use crate::message::{Message, Usage};
use crate::tools::ToolDefinition;
use futures::stream::BoxStream;

pub type StreamResult = BoxStream<'static, Result<ProviderEvent>>;

#[derive(Debug, Clone)]
pub struct RequestContext {
    pub system: String,
    pub tools: Vec<ToolDefinition>,
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone)]
pub enum ProviderEvent {
    TextDelta(String),
    ThinkingDelta(String),
    ToolUseStart { id: String, name: String },
    ToolUseDelta { id: String, partial_json: String },
    ToolUseComplete { id: String, name: String, input: serde_json::Value },
    Completed { usage: Usage },
}

#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    fn send(&self, ctx: RequestContext) -> Result<StreamResult>;
}

#[cfg(test)]
mod tests {
    // ProviderEvent 必须可 Debug/Clone,占位断言在 anthropic_types 内
}
