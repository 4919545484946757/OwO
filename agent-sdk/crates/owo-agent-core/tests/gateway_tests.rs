//! R9 模型网关韧性主链路冒烟：模拟失败→重试→熔断→恢复 + failover + 成本硬停。
//! 简化策略：只保留主链路，不重复 core lib 内已有解析/预算单测。

use owo_agent_core::gateway::{
    BreakerState, ChatMessage, CircuitBreaker, ModelOutput, ModelProvider, ResilientProvider,
    RetryPolicy,
};
use owo_agent_core::tools::ToolSpec;
use owo_agent_core::TokenUsage;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// 可编程 Provider：`fails_then_ok(n)` 前 n 次调用失败（可重试 500），其后成功；
/// `dead()` 永远失败（网络错误）。
struct MockProvider {
    id: &'static str,
    remaining_failures: AtomicUsize,
    always_fail: bool,
    calls: AtomicUsize,
}

impl MockProvider {
    fn fails_then_ok(id: &'static str, failures: usize) -> Self {
        Self {
            id,
            remaining_failures: AtomicUsize::new(failures),
            always_fail: false,
            calls: AtomicUsize::new(0),
        }
    }

    fn dead(id: &'static str) -> Self {
        Self {
            id,
            remaining_failures: AtomicUsize::new(0),
            always_fail: true,
            calls: AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }
}

#[async_trait::async_trait]
impl ModelProvider for MockProvider {
    async fn complete(
        &self,
        _messages: &[ChatMessage],
        _tools: &[ToolSpec],
    ) -> Result<ModelOutput, String> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if self.always_fail {
            return Err(format!("{}: 模型请求失败（连接被拒）", self.id));
        }
        let mut remaining = self.remaining_failures.load(Ordering::Relaxed);
        loop {
            if remaining == 0 {
                return Ok(ModelOutput::Text(format!("{}:ok", self.id)));
            }
            match self.remaining_failures.compare_exchange(
                remaining,
                remaining - 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return Err(format!("{}: 模型返回 500 内部错误", self.id)),
                Err(current) => remaining = current,
            }
        }
    }

    fn usage_snapshot(&self) -> TokenUsage {
        TokenUsage {
            prompt_tokens: 10,
            completion_tokens: 2,
            total_tokens: 12,
        }
    }
}

fn retry_policy(max_retries: usize) -> RetryPolicy {
    RetryPolicy {
        max_retries,
        base_delay_ms: 0,
        max_delay_ms: 1,
        retry_429: true,
        retry_network: true,
    }
}

/// 主链路 1：失败 → 指数退避重试 → 成功。
#[tokio::test]
async fn retry_recovers_after_transient_failures() {
    let primary = Arc::new(MockProvider::fails_then_ok("primary", 2));
    let resilient = ResilientProvider::new(
        primary.clone(),
        vec![],
        CircuitBreaker::new(5, std::time::Duration::from_millis(50)),
        retry_policy(3),
    );
    let output = resilient.complete(&[], &[]).await.expect("重试后应成功");
    assert_eq!(output, ModelOutput::Text("primary:ok".to_string()));
    assert_eq!(primary.calls(), 3, "1 首次 + 2 重试");
    assert_eq!(resilient.breaker().state(), BreakerState::Closed);
    assert_eq!(resilient.breaker().consecutive_failures(), 0);
}

/// 主链路 2：持续失败 → 熔断打开（快速失败）→ 冷却后半开探测 → 恢复 Closed。
#[tokio::test]
async fn breaker_opens_halfopens_and_recovers() {
    let primary = Arc::new(MockProvider::fails_then_ok("primary", 2));
    let resilient = ResilientProvider::new(
        primary.clone(),
        vec![],
        CircuitBreaker::new(2, std::time::Duration::from_millis(80)),
        retry_policy(0),
    );
    assert!(resilient.complete(&[], &[]).await.is_err(), "失败 1");
    assert!(
        resilient.complete(&[], &[]).await.is_err(),
        "失败 2 → 达阈值"
    );
    assert_eq!(resilient.breaker().state(), BreakerState::Open);

    // Open：快速失败，不再触达 provider。
    let before = primary.calls();
    let error = resilient.complete(&[], &[]).await.unwrap_err();
    assert!(error.contains("熔断器打开"), "熔断打开应快速失败：{error}");
    assert_eq!(primary.calls(), before, "熔断打开后不再调用 provider");

    // 冷却后 → HalfOpen，放行探测；provider 已耗尽失败次数 → 探测成功 → Closed。
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert_eq!(resilient.breaker().state(), BreakerState::HalfOpen);
    assert!(
        resilient.complete(&[], &[]).await.is_ok(),
        "半开探测成功（provider 已恢复）"
    );
    assert_eq!(
        resilient.breaker().state(),
        BreakerState::Closed,
        "探测成功 → Closed"
    );
    assert!(resilient.complete(&[], &[]).await.is_ok(), "恢复后正常放行");
    assert_eq!(resilient.breaker().consecutive_failures(), 0);
}

/// 主链路 3：failover——primary 持续失败 → 降级到 fallback（本地）。
#[tokio::test]
async fn failover_falls_back_to_secondary_when_primary_down() {
    let primary = Arc::new(MockProvider::dead("cloud-primary"));
    let fallback = Arc::new(MockProvider::fails_then_ok("local-fallback", 0));
    let resilient = ResilientProvider::new(
        primary,
        vec![fallback.clone()],
        CircuitBreaker::new(10, std::time::Duration::from_millis(50)),
        retry_policy(1),
    );
    let output = resilient.complete(&[], &[]).await.expect("fallback 应接管");
    assert_eq!(output, ModelOutput::Text("local-fallback:ok".to_string()));
    assert!(fallback.calls() >= 1, "fallback 至少调用一次");
    assert_eq!(
        resilient.breaker().consecutive_failures(),
        0,
        "整体成功应清熔断计数"
    );
}

/// 主链路 4：流式网络失败重试 + 预算类错误不降级不透传。
#[tokio::test]
async fn stream_retries_and_non_retriable_stops() {
    let primary = Arc::new(MockProvider::fails_then_ok("stream-primary", 1));
    let resilient = ResilientProvider::new(
        primary.clone(),
        vec![],
        CircuitBreaker::new(5, std::time::Duration::from_millis(50)),
        retry_policy(2),
    );
    let mut deltas: Vec<String> = Vec::new();
    let mut forward = |delta: String| deltas.push(delta);
    let mut forward_mut: &mut (dyn FnMut(String) + Send) = &mut forward;
    let output = resilient
        .complete_stream(&[], &[], &mut forward_mut)
        .await
        .expect("流式网络失败重试后应成功");
    assert_eq!(output, ModelOutput::Text("stream-primary:ok".to_string()));
    assert_eq!(
        deltas,
        vec!["stream-primary:ok".to_string()],
        "整条成功后才回放增量"
    );
    assert_eq!(primary.calls(), 2, "1 首次 + 1 重试");

    // 预算类错误（不可重试）→ 不重试、不降级。
    struct BudgetProvider;
    #[async_trait::async_trait]
    impl ModelProvider for BudgetProvider {
        async fn complete(
            &self,
            _messages: &[ChatMessage],
            _tools: &[ToolSpec],
        ) -> Result<ModelOutput, String> {
            Err("模型用量预算已超限：累计 5000 tokens ≥ 上限 1000".to_string())
        }
    }
    let resilient2 = ResilientProvider::new(
        Arc::new(BudgetProvider),
        vec![Arc::new(MockProvider::fails_then_ok("must-not-run", 0))],
        CircuitBreaker::new(5, std::time::Duration::from_millis(50)),
        retry_policy(3),
    );
    let error = resilient2.complete(&[], &[]).await.unwrap_err();
    assert!(error.contains("预算已超限"), "预算错误应直接透传：{error}");
    assert!(
        !error.contains("must-not-run"),
        "预算错误不应降级到 fallback"
    );
}
