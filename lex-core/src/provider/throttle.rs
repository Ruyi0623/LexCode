use crate::error::Result;
use crate::provider::{Provider, RequestContext, StreamResult};
use futures::StreamExt;
use std::sync::Arc;

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
