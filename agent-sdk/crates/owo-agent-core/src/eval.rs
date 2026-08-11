//! Evals：用固定任务集回归评测 Agent（成功率/耗时/输出断言）。

use crate::agent::{Agent, AgentConfig};
use crate::gateway::ModelProvider;
use crate::permissions::{AutoApprover, Policy};
use crate::session::Session;
use crate::tools::ToolRegistry;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalCase {
    pub name: String,
    pub prompt: String,
    /// 最终文本必须包含的全部子串。
    #[serde(default)]
    pub expected: Vec<String>,
    /// 用例启动前写入临时工作区的文件（相对路径 → 内容）。
    #[serde(default)]
    pub setup_files: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalSuite {
    pub name: String,
    pub cases: Vec<EvalCase>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaseResult {
    pub name: String,
    pub passed: bool,
    pub output: String,
    pub steps: usize,
    pub duration_ms: u64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalReport {
    pub suite: String,
    pub total: usize,
    pub passed: usize,
    pub pass_rate: f64,
    pub total_duration_ms: u64,
    pub cases: Vec<CaseResult>,
}

pub async fn run_suite(
    provider: Arc<dyn ModelProvider>,
    model: &str,
    suite: &EvalSuite,
) -> EvalReport {
    let started = Instant::now();
    let mut results = Vec::new();
    for case in &suite.cases {
        let workspace = std::env::temp_dir().join(format!(
            "owo-eval-{}-{}",
            case.name.replace(|c: char| !c.is_alphanumeric(), "_"),
            uuid::Uuid::new_v4()
        ));
        let case_started = Instant::now();
        let mut case_result = match std::fs::create_dir_all(&workspace) {
            Ok(()) => {
                let mut setup_ok = true;
                for (path, content) in &case.setup_files {
                    let target = workspace.join(path);
                    if let Some(parent) = target.parent() {
                        if std::fs::create_dir_all(parent).is_err() {
                            setup_ok = false;
                            break;
                        }
                    }
                    if std::fs::write(&target, content).is_err() {
                        setup_ok = false;
                        break;
                    }
                }
                if !setup_ok {
                    CaseResult {
                        name: case.name.clone(),
                        passed: false,
                        output: String::new(),
                        steps: 0,
                        duration_ms: 0,
                        error: Some("用例 setup 失败".to_string()),
                    }
                } else {
                    run_case(Arc::clone(&provider), model, &workspace, case, case_started).await
                }
            }
            Err(error) => CaseResult {
                name: case.name.clone(),
                passed: false,
                output: String::new(),
                steps: 0,
                duration_ms: 0,
                error: Some(format!("临时工作区创建失败：{error}")),
            },
        };
        case_result.duration_ms = case_started.elapsed().as_millis() as u64;
        let _ = std::fs::remove_dir_all(&workspace);
        results.push(case_result);
    }
    let passed = results.iter().filter(|result| result.passed).count();
    let total = results.len();
    EvalReport {
        suite: suite.name.clone(),
        total,
        passed,
        pass_rate: if total == 0 {
            0.0
        } else {
            passed as f64 / total as f64
        },
        total_duration_ms: started.elapsed().as_millis() as u64,
        cases: results,
    }
}

async fn run_case(
    provider: Arc<dyn ModelProvider>,
    model: &str,
    workspace: &std::path::Path,
    case: &EvalCase,
    _started: Instant,
) -> CaseResult {
    let policy = Policy::new(workspace.to_path_buf());
    let registry = ToolRegistry::new();
    let agent = Agent::new(provider, registry, policy, AgentConfig::default());
    let mut session = Session::new(workspace, model, None);
    let abort = AtomicBool::new(false);
    let approver = AutoApprover { allow: true };
    let outcome = agent
        .run_turn(&mut session, &case.prompt, &approver, &abort, &mut |_| {})
        .await;
    match outcome {
        Ok(outcome) => {
            let output = outcome.final_text.unwrap_or_default();
            let passed = case
                .expected
                .iter()
                .all(|expected| output.contains(expected));
            CaseResult {
                name: case.name.clone(),
                passed,
                output,
                steps: outcome.steps,
                duration_ms: 0,
                error: None,
            }
        }
        Err(error) => CaseResult {
            name: case.name.clone(),
            passed: false,
            output: String::new(),
            steps: 0,
            duration_ms: 0,
            error: Some(error.to_string()),
        },
    }
}

/// 内置演示套件：覆盖读/写/列表/搜索/子代理。
pub fn builtin_suite() -> EvalSuite {
    EvalSuite {
        name: "builtin-demo".to_string(),
        cases: vec![
            EvalCase {
                name: "read_file".to_string(),
                prompt: "读取 data.txt 并汇报其内容".to_string(),
                expected: vec!["hello-eval".to_string()],
                setup_files: vec![("data.txt".to_string(), "hello-eval".to_string())],
            },
            EvalCase {
                name: "write_file".to_string(),
                prompt: "创建 output.txt，内容为 eval-ok".to_string(),
                expected: vec!["eval-ok".to_string()],
                setup_files: Vec::new(),
            },
            EvalCase {
                name: "list_dir".to_string(),
                prompt: "列出当前目录，找出 src.txt".to_string(),
                expected: vec!["src.txt".to_string()],
                setup_files: vec![("src.txt".to_string(), "x".to_string())],
            },
            EvalCase {
                name: "search_files".to_string(),
                prompt: "搜索文件名包含 todo 的文件并汇报".to_string(),
                expected: vec!["todo.md".to_string()],
                setup_files: vec![("todo.md".to_string(), "任务".to_string())],
            },
            EvalCase {
                name: "explore_subagent".to_string(),
                prompt: "调用 explore 调查 NOTES.txt 的内容并汇报".to_string(),
                expected: vec!["subagent-notes".to_string()],
                setup_files: vec![("NOTES.txt".to_string(), "subagent-notes".to_string())],
            },
        ],
    }
}

pub fn eval_suite_path(path: &PathBuf) -> Option<EvalSuite> {
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}
