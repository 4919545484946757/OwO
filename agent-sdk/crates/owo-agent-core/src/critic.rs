//! Critic：只读评审原语（writer-critic 模式，多 Agent P0）。
//!
//! 设计来源：《多Agent并行体系-生产级设计与跨机扩展-2026-08-16.md》§3 best-of-n / writer-critic。
//! - 权限门禁 [`ReadOnlyGate`]：评审器必须运行在只读策略下，写/执行/注入一律拒绝。
//! - [`review_loop`]：评审意见回流原作者，最多 `max_rounds` 轮；通过或轮次耗尽即返回。
//! - [`ConsistencyReport`]：评审结果与人工结论一致率评估（P0 退出标准）。
//!
//! 本模块不触碰 agent 执行路径，只提供可测试的评审原语；
//! `goal.rs` worker 层经 [`CriticConfig`] 可选接入。

use crate::permissions::{Decision, PermissionRequest, Policy};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::future::Future;
use std::sync::{Arc, Mutex};

/// 评审结论。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CriticVerdict {
    pub approved: bool,
    /// 0..=100 评分；>= `CriticConfig::min_score` 视为通过。
    pub score: u8,
    pub comments: Vec<String>,
}

impl CriticVerdict {
    pub fn approve(score: u8) -> Self {
        Self {
            approved: true,
            score,
            comments: Vec::new(),
        }
    }

    pub fn reject(score: u8, comments: Vec<String>) -> Self {
        Self {
            approved: false,
            score,
            comments,
        }
    }
}

/// 评审器抽象（只读；调用链上不执行任何写/注入动作）。
#[async_trait]
pub trait Critic: Send + Sync {
    async fn review(
        &self,
        draft: &str,
        context: &serde_json::Value,
    ) -> Result<CriticVerdict, String>;
}

/// 只读门禁：包一层 [`Policy`]，保证评审路径只读。
///
/// - [`ReadOnlyGate::ensure_read_only`]：策略非只读时拒绝启动评审（防误配置）。
/// - [`ReadOnlyGate::decide`]：对任意请求按只读策略裁决（写/执行/注入 → Deny）。
#[derive(Clone)]
pub struct ReadOnlyGate {
    policy: Arc<Policy>,
}

impl ReadOnlyGate {
    pub fn new(policy: Policy) -> Self {
        Self {
            policy: Arc::new(policy),
        }
    }

    /// 标准只读门禁（评审器默认）。
    pub fn read_only() -> Self {
        Self::new(Policy::read_only("."))
    }

    pub fn policy(&self) -> &Policy {
        &self.policy
    }

    /// 门禁检查：策略必须处于只读态，否则评审拒绝启动。
    pub fn ensure_read_only(&self) -> Result<(), String> {
        if self.policy.is_read_only() {
            Ok(())
        } else {
            Err("critic 门禁要求只读策略（当前策略可写，拒绝评审）".to_string())
        }
    }

    /// 对单个请求按只读策略裁决。
    pub fn decide(&self, request: &PermissionRequest) -> Decision {
        self.policy.decision(request)
    }
}

/// 一轮评审记录（历史回溯）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewRound {
    pub draft: String,
    pub verdict: CriticVerdict,
}

/// 评审循环结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewOutcome {
    pub final_draft: String,
    /// 评审轮数（≥1）。
    pub rounds: u32,
    /// 原作者修订次数（意见回流次数）。
    pub revisions: u32,
    pub approved: bool,
    pub history: Vec<ReviewRound>,
}

/// 评审配置（`goal.rs` worker 层可选接入）。
#[derive(Clone)]
pub struct CriticConfig {
    /// 最多评审轮数。
    pub max_rounds: u32,
    /// 通过评分阈值。
    pub min_score: u8,
    pub critic: Arc<dyn Critic>,
    pub gate: ReadOnlyGate,
}

impl CriticConfig {
    pub fn new(critic: Arc<dyn Critic>, gate: ReadOnlyGate) -> Self {
        Self {
            max_rounds: 2,
            min_score: 80,
            critic,
            gate,
        }
    }

    pub fn with_max_rounds(mut self, max_rounds: u32) -> Self {
        self.max_rounds = max_rounds.max(1);
        self
    }

    pub fn with_min_score(mut self, min_score: u8) -> Self {
        self.min_score = min_score.min(100);
        self
    }
}

/// writer-critic 评审循环：评审意见回流原作者，最多 `max_rounds` 轮。
///
/// `author` 闭包：输入（当前草稿, 评审意见列表）→ 修订后的草稿（所有权语义，便于异步实现）。
/// 通过条件：评审 `approved == true` 或 `score >= min_score`；轮数耗尽仍未通过则
/// 返回 `approved == false` 的最终草稿（由调用方决定失败/再仲裁）。
pub async fn review_loop<F, Fut>(
    config: &CriticConfig,
    context: &serde_json::Value,
    initial_draft: String,
    mut author: F,
) -> Result<ReviewOutcome, String>
where
    F: FnMut(String, Vec<String>) -> Fut + Send,
    Fut: Future<Output = Result<String, String>> + Send,
{
    config.gate.ensure_read_only()?;
    let mut draft = initial_draft;
    let mut history: Vec<ReviewRound> = Vec::new();
    let mut rounds = 0u32;
    let mut revisions = 0u32;
    loop {
        let verdict = config.critic.review(&draft, context).await?;
        rounds += 1;
        history.push(ReviewRound {
            draft: draft.clone(),
            verdict: verdict.clone(),
        });
        if verdict.approved || verdict.score >= config.min_score {
            return Ok(ReviewOutcome {
                final_draft: draft,
                rounds,
                revisions,
                approved: true,
                history,
            });
        }
        if rounds >= config.max_rounds {
            return Ok(ReviewOutcome {
                final_draft: draft,
                rounds,
                revisions,
                approved: false,
                history,
            });
        }
        let feedback = verdict.comments.clone();
        draft = author(draft, feedback).await?;
        revisions += 1;
    }
}

/// 一致率样本：批评家结论 vs 人工结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SamplePair {
    pub critic_approved: bool,
    pub human_approved: bool,
}

/// 一致率报告（P0 退出标准：样例集一致率达标）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ConsistencyReport {
    pub total: usize,
    pub agreed: usize,
    /// 一致率 0.0..=1.0。
    pub rate: f64,
}

impl ConsistencyReport {
    pub fn of(samples: &[SamplePair]) -> Self {
        let total = samples.len();
        let agreed = samples
            .iter()
            .filter(|s| s.critic_approved == s.human_approved)
            .count();
        let rate = if total == 0 {
            1.0
        } else {
            agreed as f64 / total as f64
        };
        Self {
            total,
            agreed,
            rate,
        }
    }

    pub fn is_acceptable(&self, threshold: f64) -> bool {
        self.rate >= threshold
    }
}

/// 测试替身：脚本化评审器（顺序出牌，耗尽后默认通过）。
pub struct ScriptedCritic {
    script: Arc<Mutex<VecDeque<CriticVerdict>>>,
}

impl ScriptedCritic {
    pub fn new(verdicts: Vec<CriticVerdict>) -> Self {
        Self {
            script: Arc::new(Mutex::new(verdicts.into_iter().collect())),
        }
    }

    /// 恒定通过（score=100）。
    pub fn approving() -> Self {
        Self::new(vec![CriticVerdict::approve(100)])
    }
}

#[async_trait]
impl Critic for ScriptedCritic {
    async fn review(
        &self,
        _draft: &str,
        _context: &serde_json::Value,
    ) -> Result<CriticVerdict, String> {
        Ok(self
            .script
            .lock()
            .map(|mut s| s.pop_front())
            .unwrap_or(None)
            .unwrap_or_else(|| CriticVerdict::approve(100)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn read_only_gate_allows_reads_denies_writes() {
        let gate = ReadOnlyGate::read_only();
        assert!(gate.ensure_read_only().is_ok());
        let read = gate
            .policy()
            .evaluate("read_file", &json!({ "path": "a.txt" }));
        assert_eq!(gate.decide(&read), Decision::Allow);
        let write = gate
            .policy()
            .evaluate("write_file", &json!({ "path": "a.txt" }));
        assert_eq!(gate.decide(&write), Decision::Deny);
        let inject = gate.policy().evaluate("text.inject", &json!({}));
        assert_eq!(gate.decide(&inject), Decision::Deny);
    }

    #[test]
    fn writable_policy_gate_refuses_critic() {
        let gate = ReadOnlyGate::new(Policy::new("."));
        assert_eq!(
            gate.ensure_read_only().unwrap_err(),
            "critic 门禁要求只读策略（当前策略可写，拒绝评审）"
        );
    }

    #[tokio::test]
    async fn review_loop_approves_first_round() {
        let config = CriticConfig::new(
            Arc::new(ScriptedCritic::approving()),
            ReadOnlyGate::read_only(),
        );
        let outcome = review_loop(
            &config,
            &json!({}),
            "draft-v1".to_string(),
            |draft, _fb| async move { Ok(format!("{draft}-revised")) },
        )
        .await
        .unwrap();
        assert!(outcome.approved);
        assert_eq!(outcome.rounds, 1);
        assert_eq!(outcome.revisions, 0);
        assert_eq!(outcome.final_draft, "draft-v1");
    }

    #[tokio::test]
    async fn review_loop_feedback_flows_back_to_author() {
        let critic = ScriptedCritic::new(vec![
            CriticVerdict::reject(50, vec!["缺少测试".to_string()]),
            CriticVerdict::approve(90),
        ]);
        let config = CriticConfig::new(Arc::new(critic), ReadOnlyGate::read_only());
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_clone = Arc::clone(&seen);
        let outcome = review_loop(
            &config,
            &json!({}),
            "draft-v1".to_string(),
            move |_draft, feedback| {
                let seen = Arc::clone(&seen_clone);
                async move {
                    seen.lock().unwrap().extend(feedback.iter().cloned());
                    Ok("draft-v2".to_string())
                }
            },
        )
        .await
        .unwrap();
        assert!(outcome.approved);
        assert_eq!(outcome.rounds, 2);
        assert_eq!(outcome.revisions, 1);
        assert_eq!(outcome.final_draft, "draft-v2");
        assert_eq!(seen.lock().unwrap().as_slice(), &["缺少测试".to_string()]);
    }

    #[tokio::test]
    async fn review_loop_exhausts_rounds_not_approved() {
        let critic = ScriptedCritic::new(vec![
            CriticVerdict::reject(30, vec![]),
            CriticVerdict::reject(40, vec![]),
            CriticVerdict::reject(40, vec![]),
        ]);
        let config =
            CriticConfig::new(Arc::new(critic), ReadOnlyGate::read_only()).with_max_rounds(2);
        let outcome = review_loop(
            &config,
            &json!({}),
            "draft-v1".to_string(),
            |draft, _fb| async move { Ok(format!("{draft}-r")) },
        )
        .await
        .unwrap();
        assert!(!outcome.approved);
        assert_eq!(outcome.rounds, 2);
        assert_eq!(outcome.revisions, 1);
        assert_eq!(outcome.final_draft, "draft-v1-r");
        assert_eq!(outcome.history.len(), 2);
    }

    #[test]
    fn consistency_report_matches_human_verdicts() {
        let samples = vec![
            SamplePair {
                critic_approved: true,
                human_approved: true,
            },
            SamplePair {
                critic_approved: true,
                human_approved: true,
            },
            SamplePair {
                critic_approved: false,
                human_approved: false,
            },
            SamplePair {
                critic_approved: true,
                human_approved: false,
            },
        ];
        let report = ConsistencyReport::of(&samples);
        assert_eq!(report.total, 4);
        assert_eq!(report.agreed, 3);
        assert_eq!(report.rate, 0.75);
        assert!(report.is_acceptable(0.7));
        assert!(!report.is_acceptable(0.9));
    }
}
