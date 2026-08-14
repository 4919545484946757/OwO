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
    /// 执行后必须存在的文件（相对路径 → 内容须包含的子串）。
    /// 让"写文件"类用例断言真实落盘，而非模型口头汇报。
    #[serde(default)]
    pub expected_files: Vec<(String, String)>,
    /// 执行后必须不存在的文件（相对路径）——验证删除/重命名。
    #[serde(default)]
    pub expected_missing: Vec<String>,
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
            let text_ok = case
                .expected
                .iter()
                .all(|expected| output.contains(expected));
            let files_ok = case.expected_files.iter().all(|(path, contains)| {
                let Ok(content) = std::fs::read_to_string(workspace.join(path)) else {
                    return false;
                };
                content.contains(contains)
            });
            let missing_ok = case
                .expected_missing
                .iter()
                .all(|path| !workspace.join(path).exists());
            let passed = text_ok && files_ok && missing_ok;
            let mut error = None;
            if !files_ok {
                error = Some(format!(
                    "期望文件未就绪：{:?}",
                    case.expected_files
                        .iter()
                        .filter(|(path, contains)| {
                            std::fs::read_to_string(workspace.join(path))
                                .map(|content| !content.contains(contains))
                                .unwrap_or(true)
                        })
                        .map(|(path, _)| path.clone())
                        .collect::<Vec<_>>()
                ));
            } else if !missing_ok {
                error = Some(format!(
                    "应被删除的文件仍存在：{:?}",
                    case.expected_missing
                        .iter()
                        .filter(|path| workspace.join(path).exists())
                        .collect::<Vec<_>>()
                ));
            }
            CaseResult {
                name: case.name.clone(),
                passed,
                output,
                steps: outcome.steps,
                duration_ms: 0,
                error,
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
                expected_files: Vec::new(),
                expected_missing: Vec::new(),
            },
            EvalCase {
                name: "write_file".to_string(),
                prompt: "创建 output.txt，内容为 eval-ok".to_string(),
                expected: vec!["eval-ok".to_string()],
                setup_files: Vec::new(),
                expected_files: Vec::new(),
                expected_missing: Vec::new(),
            },
            EvalCase {
                name: "list_dir".to_string(),
                prompt: "列出当前目录，找出 src.txt".to_string(),
                expected: vec!["src.txt".to_string()],
                setup_files: vec![("src.txt".to_string(), "x".to_string())],
                expected_files: Vec::new(),
                expected_missing: Vec::new(),
            },
            EvalCase {
                name: "search_files".to_string(),
                prompt: "搜索文件名包含 todo 的文件并汇报".to_string(),
                expected: vec!["todo.md".to_string()],
                setup_files: vec![("todo.md".to_string(), "任务".to_string())],
                expected_files: Vec::new(),
                expected_missing: Vec::new(),
            },
            EvalCase {
                name: "explore_subagent".to_string(),
                prompt: "调用 explore 调查 NOTES.txt 的内容并汇报".to_string(),
                expected: vec!["subagent-notes".to_string()],
                setup_files: vec![("NOTES.txt".to_string(), "subagent-notes".to_string())],
                expected_files: Vec::new(),
                expected_missing: Vec::new(),
            },
            EvalCase {
                name: "read_file_second".to_string(),
                prompt: "读取 two.txt 并汇报内容".to_string(),
                expected: vec!["second-file".to_string()],
                setup_files: vec![("two.txt".to_string(), "second-file".to_string())],
                expected_files: Vec::new(),
                expected_missing: Vec::new(),
            },
            EvalCase {
                name: "write_file_nested".to_string(),
                prompt: "创建 nested/out.txt，内容为 nested-ok".to_string(),
                expected: vec!["nested-ok".to_string()],
                setup_files: Vec::new(),
                expected_files: Vec::new(),
                expected_missing: Vec::new(),
            },
            EvalCase {
                name: "list_dir_nested".to_string(),
                prompt: "列出目录，找到 nested 子目录".to_string(),
                expected: vec!["nested".to_string()],
                setup_files: vec![("nested/.keep".to_string(), "x".to_string())],
                expected_files: Vec::new(),
                expected_missing: Vec::new(),
            },
            EvalCase {
                name: "search_notes".to_string(),
                prompt: "搜索文件名包含 notes 的文件并汇报".to_string(),
                expected: vec!["notes.md".to_string()],
                setup_files: vec![("notes.md".to_string(), "n".to_string())],
                expected_files: Vec::new(),
                expected_missing: Vec::new(),
            },
            EvalCase {
                name: "explore_readme".to_string(),
                prompt: "调用 explore 调查 README2.txt 的内容并汇报".to_string(),
                expected: vec!["readme-two".to_string()],
                setup_files: vec![("README2.txt".to_string(), "readme-two".to_string())],
                expected_files: Vec::new(),
                expected_missing: Vec::new(),
            },
            EvalCase {
                name: "read_file_uppercase".to_string(),
                prompt: "读取 DATA.TXT 并汇报内容".to_string(),
                expected: vec!["upper".to_string()],
                setup_files: vec![("DATA.TXT".to_string(), "upper".to_string())],
                expected_files: Vec::new(),
                expected_missing: Vec::new(),
            },
            EvalCase {
                name: "write_report".to_string(),
                prompt: "创建 report.md，内容为 report-ok".to_string(),
                expected: vec!["report-ok".to_string()],
                setup_files: Vec::new(),
                expected_files: Vec::new(),
                expected_missing: Vec::new(),
            },
            EvalCase {
                name: "list_config".to_string(),
                prompt: "列出目录，找到 config.json".to_string(),
                expected: vec!["config.json".to_string()],
                setup_files: vec![("config.json".to_string(), "{}".to_string())],
                expected_files: Vec::new(),
                expected_missing: Vec::new(),
            },
            EvalCase {
                name: "search_config".to_string(),
                prompt: "搜索文件名包含 config 的文件并汇报".to_string(),
                expected: vec!["config.json".to_string()],
                setup_files: vec![("config.json".to_string(), "{}".to_string())],
                expected_files: Vec::new(),
                expected_missing: Vec::new(),
            },
            EvalCase {
                name: "explore_config".to_string(),
                prompt: "调用 explore 调查 config.json 的内容并汇报".to_string(),
                expected: vec!["cfg-value".to_string()],
                setup_files: vec![("config.json".to_string(), "cfg-value".to_string())],
                expected_files: Vec::new(),
                expected_missing: Vec::new(),
            },
            EvalCase {
                name: "read_multiword".to_string(),
                prompt: "读取 multiword-name.txt 并汇报内容".to_string(),
                expected: vec!["multi".to_string()],
                setup_files: vec![("multiword-name.txt".to_string(), "multi".to_string())],
                expected_files: Vec::new(),
                expected_missing: Vec::new(),
            },
            EvalCase {
                name: "write_chinese".to_string(),
                prompt: "创建 中文文件.txt，内容为 中文ok".to_string(),
                expected: vec!["中文ok".to_string()],
                setup_files: Vec::new(),
                expected_files: Vec::new(),
                expected_missing: Vec::new(),
            },
            EvalCase {
                name: "list_chinese".to_string(),
                prompt: "列出目录，找到 中文文件.txt".to_string(),
                expected: vec!["中文文件.txt".to_string()],
                setup_files: vec![("中文文件.txt".to_string(), "c".to_string())],
                expected_files: Vec::new(),
                expected_missing: Vec::new(),
            },
            EvalCase {
                name: "search_chinese".to_string(),
                prompt: "搜索文件名包含 中文 的文件并汇报".to_string(),
                expected: vec!["中文文件.txt".to_string()],
                setup_files: vec![("中文文件.txt".to_string(), "c".to_string())],
                expected_files: Vec::new(),
                expected_missing: Vec::new(),
            },
            EvalCase {
                name: "explore_multiword".to_string(),
                prompt: "调用 explore 调查 multiword-name.txt 的内容并汇报".to_string(),
                expected: vec!["multi".to_string()],
                setup_files: vec![("multiword-name.txt".to_string(), "multi".to_string())],
                expected_files: Vec::new(),
                expected_missing: Vec::new(),
            },
            EvalCase {
                name: "run_command_echo".to_string(),
                prompt: "运行命令 echo eval-shell-ok 并汇报输出".to_string(),
                expected: vec!["eval-shell-ok".to_string()],
                setup_files: Vec::new(),
                expected_files: Vec::new(),
                expected_missing: Vec::new(),
            },
            EvalCase {
                name: "write_file_expected".to_string(),
                prompt: "创建 verify.txt，内容为 verified-content".to_string(),
                expected: vec!["verified-content".to_string()],
                setup_files: Vec::new(),
                expected_files: vec![("verify.txt".to_string(), "verified-content".to_string())],
                expected_missing: Vec::new(),
            },
            EvalCase {
                name: "delete_file".to_string(),
                prompt: "删除 old.txt".to_string(),
                expected: vec!["已删除".to_string()],
                setup_files: vec![("old.txt".to_string(), "delete-me".to_string())],
                expected_files: Vec::new(),
                expected_missing: vec!["old.txt".to_string()],
            },
            EvalCase {
                name: "rename_file".to_string(),
                prompt: "把 a.txt 重命名为 b.txt".to_string(),
                expected: vec!["b.txt".to_string()],
                setup_files: vec![("a.txt".to_string(), "rename-me".to_string())],
                expected_files: vec![("b.txt".to_string(), "rename-me".to_string())],
                expected_missing: vec!["a.txt".to_string()],
            },
            EvalCase {
                name: "deep_write_expected".to_string(),
                prompt: "创建 a/b/c/d.txt，内容为 deep-content".to_string(),
                expected: vec!["deep-content".to_string()],
                setup_files: Vec::new(),
                expected_files: vec![("a/b/c/d.txt".to_string(), "deep-content".to_string())],
                expected_missing: Vec::new(),
            },
            EvalCase {
                name: "read_two_summarize".to_string(),
                prompt: "分别读取 notes1.txt 和 notes2.txt，汇报两个文件的值".to_string(),
                expected: vec!["first-value".to_string(), "second-value".to_string()],
                setup_files: vec![
                    ("notes1.txt".to_string(), "first-value".to_string()),
                    ("notes2.txt".to_string(), "second-value".to_string()),
                ],
                expected_files: Vec::new(),
                expected_missing: Vec::new(),
            },
            EvalCase {
                name: "search_nested".to_string(),
                prompt: "搜索文件名包含 todo 的文件并汇报".to_string(),
                expected: vec!["todo.md".to_string()],
                setup_files: vec![("src/deep/todo.md".to_string(), "t".to_string())],
                expected_files: Vec::new(),
                expected_missing: Vec::new(),
            },
            EvalCase {
                name: "chinese_nested_write".to_string(),
                prompt: "创建 目录甲/文件乙.txt，内容为 深层中文ok".to_string(),
                expected: vec!["深层中文ok".to_string()],
                setup_files: Vec::new(),
                expected_files: vec![("目录甲/文件乙.txt".to_string(), "深层中文ok".to_string())],
                expected_missing: Vec::new(),
            },
            EvalCase {
                name: "multi_step_pipeline".to_string(),
                prompt: "先创建 plan.txt 内容为 step-one，再读取它并汇报".to_string(),
                expected: vec!["step-one".to_string()],
                setup_files: Vec::new(),
                expected_files: vec![("plan.txt".to_string(), "step-one".to_string())],
                expected_missing: Vec::new(),
            },
            EvalCase {
                name: "explore_and_write".to_string(),
                prompt:
                    "调用 explore 调查 spec.txt 的内容，然后创建 summary.txt 并写入调查到的内容"
                        .to_string(),
                expected: vec!["spec-summary".to_string()],
                setup_files: vec![("spec.txt".to_string(), "spec-summary 说明".to_string())],
                expected_files: vec![("summary.txt".to_string(), "spec-summary".to_string())],
                expected_missing: Vec::new(),
            },
        ],
    }
}

pub fn eval_suite_path(path: &PathBuf) -> Option<EvalSuite> {
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}
