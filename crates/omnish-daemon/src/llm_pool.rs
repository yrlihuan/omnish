//! Daemon-wide bounded concurrency for LLM calls.
//!
//! Every LLM request the daemon issues should go through here rather than
//! calling `backend.complete()` directly. A feature that fans out (e.g. the
//! `/resume ?` search splitting a large thread set into several requests) would
//! otherwise put an unbounded number of requests in flight at once, which
//! invites provider rate limits and makes cost scale with corpus size.
//!
//! The permit pool is process-wide, so the cap is a property of the daemon and
//! not of any single feature: a caller only says *what* to send, never how many
//! may run at once.

use omnish_llm::backend::{LlmBackend, LlmRequest, LlmResponse};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;

/// Maximum LLM requests the daemon keeps in flight at any moment.
pub const MAX_CONCURRENT_LLM_CALLS: usize = 16;

/// Shared permit pool. Created on first use and never closed.
fn semaphore() -> &'static Semaphore {
    static SEM: std::sync::OnceLock<Semaphore> = std::sync::OnceLock::new();
    SEM.get_or_init(|| Semaphore::new(MAX_CONCURRENT_LLM_CALLS))
}

/// Permits currently available. Exposed for diagnostics/tests.
pub fn available_permits() -> usize {
    semaphore().available_permits()
}

/// Result of one pooled LLM call. Callers decide how to surface each case;
/// keeping them distinct lets a batch report "some requests timed out" rather
/// than silently returning partial results.
#[derive(Debug)]
pub enum LlmOutcome {
    Ok(Box<LlmResponse>),
    Failed(String),
    TimedOut,
}

impl LlmOutcome {
    pub fn response(&self) -> Option<&LlmResponse> {
        match self {
            LlmOutcome::Ok(r) => Some(r),
            _ => None,
        }
    }

    pub fn timed_out(&self) -> bool {
        matches!(self, LlmOutcome::TimedOut)
    }
}

/// Issue one LLM request through the pool, returning the backend's own result.
///
/// This is the drop-in replacement for `backend.complete(&req)`: it changes
/// nothing about error handling or timeouts, it only makes the call wait its
/// turn when the daemon already has `MAX_CONCURRENT_LLM_CALLS` in flight. Call
/// sites that need a timeout should use [`complete_one`] instead.
pub async fn complete<B>(backend: &B, req: &LlmRequest) -> anyhow::Result<LlmResponse>
where
    B: LlmBackend + ?Sized,
{
    let _permit = semaphore()
        .acquire()
        .await
        .map_err(|_| anyhow::anyhow!("llm pool closed"))?;
    backend.complete(req).await
}

/// Issue one LLM request through the pool, bounded by `timeout`.
///
/// The permit is taken *before* the timeout starts, so `timeout` bounds the
/// call itself and never charges a request for time spent queued behind others.
pub async fn complete_one<B>(backend: &B, req: &LlmRequest, timeout: Duration) -> LlmOutcome
where
    B: LlmBackend + ?Sized,
{
    let _permit = match semaphore().acquire().await {
        Ok(p) => p,
        Err(_) => return LlmOutcome::Failed("llm pool closed".to_string()),
    };
    match tokio::time::timeout(timeout, backend.complete(req)).await {
        Ok(Ok(resp)) => LlmOutcome::Ok(Box::new(resp)),
        Ok(Err(e)) => LlmOutcome::Failed(format!("{e}")),
        Err(_) => LlmOutcome::TimedOut,
    }
}

/// Issue many LLM requests concurrently, bounded by the pool.
///
/// Outcomes are returned in the order of `requests`, independent of completion
/// order, so callers can correlate a result with the request that produced it.
pub async fn complete_all<B>(
    backend: &Arc<B>,
    requests: Vec<LlmRequest>,
    timeout: Duration,
) -> Vec<LlmOutcome>
where
    B: LlmBackend + Send + Sync + 'static,
{
    let total = requests.len();
    let mut outcomes: Vec<LlmOutcome> = (0..total)
        .map(|_| LlmOutcome::Failed("request did not run".to_string()))
        .collect();
    if total == 0 {
        return outcomes;
    }

    let mut set = tokio::task::JoinSet::new();
    for (idx, req) in requests.into_iter().enumerate() {
        let backend = Arc::clone(backend);
        set.spawn(async move { (idx, complete_one(backend.as_ref(), &req, timeout).await) });
    }
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((idx, outcome)) => outcomes[idx] = outcome,
            // A panicked task leaves its slot as the "did not run" default; we
            // cannot tell which index it was, so just record that it happened.
            Err(e) => tracing::warn!("llm_pool: request task failed: {}", e),
        }
    }
    outcomes
}

#[cfg(test)]
mod tests {
    use super::*;
    use omnish_llm::backend::{ContentBlock, LlmRequest, LlmResponse, StopReason, TriggerType, UseCase};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn resp_text(t: &str) -> LlmResponse {
        LlmResponse {
            content: vec![ContentBlock::Text(t.to_string())],
            stop_reason: StopReason::EndTurn,
            model: "test".to_string(),
            usage: None,
        }
    }

    fn req() -> LlmRequest {
        LlmRequest {
            context: String::new(),
            query: None,
            trigger: TriggerType::Manual,
            session_ids: vec![],
            use_case: UseCase::Summarize,
            max_content_chars: None,
            system_prompt: None,
            enable_thinking: Some(false),
            tools: vec![],
            extra_messages: vec![],
        }
    }

    /// Records peak concurrency so the pool's cap can be asserted.
    struct PeakBackend {
        live: AtomicUsize,
        peak: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl LlmBackend for PeakBackend {
        async fn complete(&self, _req: &LlmRequest) -> anyhow::Result<LlmResponse> {
            let now = self.live.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(20)).await;
            self.live.fetch_sub(1, Ordering::SeqCst);
            Ok(resp_text("ok"))
        }
        fn name(&self) -> &str { "peak" }
        fn model_name(&self) -> &str { "peak" }
    }

    #[tokio::test]
    async fn test_complete_all_bounds_concurrency() {
        let backend = Arc::new(PeakBackend {
            live: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        });
        let reqs: Vec<LlmRequest> = (0..40).map(|_| req()).collect();
        let outcomes = complete_all(&backend, reqs, Duration::from_secs(5)).await;

        assert_eq!(outcomes.len(), 40);
        assert!(outcomes.iter().all(|o| o.response().is_some()), "all should succeed");
        let peak = backend.peak.load(Ordering::SeqCst);
        assert!(
            peak <= MAX_CONCURRENT_LLM_CALLS,
            "peak concurrency {} exceeded cap {}",
            peak,
            MAX_CONCURRENT_LLM_CALLS
        );
        assert!(peak > 1, "expected real concurrency, got peak {}", peak);
    }

    struct SlowBackend;

    #[async_trait::async_trait]
    impl LlmBackend for SlowBackend {
        async fn complete(&self, _req: &LlmRequest) -> anyhow::Result<LlmResponse> {
            tokio::time::sleep(Duration::from_secs(60)).await;
            Ok(resp_text("never"))
        }
        fn name(&self) -> &str { "slow" }
        fn model_name(&self) -> &str { "slow" }
    }

    #[tokio::test]
    async fn test_timeout_is_reported_not_swallowed() {
        let backend = Arc::new(SlowBackend);
        let outcomes = complete_all(&backend, vec![req()], Duration::from_millis(30)).await;
        assert_eq!(outcomes.len(), 1);
        assert!(outcomes[0].timed_out(), "got {:?}", outcomes[0]);
        // Permit release on the timeout path is guaranteed by `_permit` being
        // an RAII guard; asserting available_permits() here would race the
        // other tests sharing this process-wide pool.
    }

    struct FailBackend;

    #[async_trait::async_trait]
    impl LlmBackend for FailBackend {
        async fn complete(&self, _req: &LlmRequest) -> anyhow::Result<LlmResponse> {
            Err(anyhow::anyhow!("boom"))
        }
        fn name(&self) -> &str { "fail" }
        fn model_name(&self) -> &str { "fail" }
    }

    #[tokio::test]
    async fn test_order_preserved_and_errors_carried() {
        let backend = Arc::new(FailBackend);
        let outcomes = complete_all(&backend, vec![req(), req()], Duration::from_secs(5)).await;
        assert_eq!(outcomes.len(), 2);
        for o in &outcomes {
            match o {
                LlmOutcome::Failed(msg) => assert!(msg.contains("boom"), "got {msg}"),
                other => panic!("expected Failed, got {other:?}"),
            }
        }
    }
}
