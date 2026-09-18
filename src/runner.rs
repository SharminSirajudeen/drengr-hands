use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::expect_network::{evaluate, ExpectNetwork, ExpectOutcome};
use crate::network::sink::{NetworkSink, NetworkSource};
use crate::ooda::{self, LlmClient, OodaConfig};
use crate::transport::DeviceTransport;

/// A test suite loaded from drengr-tests.yml.
#[derive(Debug, Deserialize)]
pub struct TestSuite {
    pub app: String,
    pub tasks: Vec<TestTask>,
}

/// A single test task in the suite.
#[derive(Debug, Deserialize)]
pub struct TestTask {
    pub name: String,
    pub task: String,
    #[serde(default = "default_timeout")]
    pub timeout: String,
    /// Assertions about the traffic this task should have produced. An empty
    /// list means the task is judged on the screen alone, as before.
    #[serde(default)]
    pub expect_network: Vec<ExpectNetwork>,
}

fn default_timeout() -> String {
    "60s".to_string()
}

/// Result of running a test suite.
#[derive(Debug, Serialize)]
pub struct SuiteResult {
    pub app: String,
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub results: Vec<TaskResult>,
    pub duration_ms: u64,
}

/// Result of a single task within a suite.
#[derive(Debug, Serialize)]
pub struct TaskResult {
    pub name: String,
    pub task: String,
    pub passed: bool,
    pub steps: usize,
    pub reasoning: String,
    pub duration_ms: u64,
    /// One entry per `expect_network` rule, in declaration order. Empty when
    /// the task declared none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub network_expectations: Vec<ExpectOutcome>,
}

impl SuiteResult {
    /// Exit code per PRD spec: 0 = all passed, 1 = some failed.
    pub fn exit_code(&self) -> i32 {
        if self.failed == 0 {
            0
        } else {
            1
        }
    }
}

/// Load a test suite from a YAML file.
pub fn load_suite(path: &Path) -> Result<TestSuite> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read test file: {}", path.display()))?;
    serde_yaml::from_str(&content)
        .with_context(|| format!("Failed to parse test file: {}", path.display()))
}

/// Parse a timeout string like "60s", "90s", "2m" into seconds.
pub fn parse_timeout(s: &str) -> u64 {
    let s = s.trim();
    if let Some(secs) = s.strip_suffix('s') {
        secs.parse().unwrap_or(60)
    } else if let Some(mins) = s.strip_suffix('m') {
        mins.parse::<u64>().unwrap_or(1) * 60
    } else {
        s.parse().unwrap_or(60)
    }
}

/// Run all tasks in a test suite sequentially.
pub async fn run_suite(
    suite: &TestSuite,
    transport: &dyn DeviceTransport,
    llm: &LlmClient,
    device_id: &str,
    network: &NetworkSink,
) -> SuiteResult {
    let suite_start = Instant::now();
    let mut results = Vec::new();

    eprintln!("{}", llm.describe());
    eprintln!("Running {} tasks for {}\n", suite.tasks.len(), suite.app);

    for (i, test_task) in suite.tasks.iter().enumerate() {
        let timeout_secs = parse_timeout(&test_task.timeout);
        let max_steps = (timeout_secs / 2).max(10) as usize; // ~2s per step estimate

        eprintln!(
            "─── Task {}/{}: {} ───",
            i + 1,
            suite.tasks.len(),
            test_task.name
        );
        eprintln!("  \"{}\" (timeout: {})", test_task.task, test_task.timeout);

        let config = OodaConfig {
            task: test_task.task.clone(),
            app_package: suite.app.clone(),
            max_steps,
            device_id: device_id.to_string(),
            force_vision: false,
            verify_completion: true,
            // Tests declare their target app up front — cross-app navigation
            // in a test scenario is almost certainly prompt-injection.
            allowed_apps: Some(vec![suite.app.clone()]),
        };

        let task_start = Instant::now();
        let window_start_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let ooda_result = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            ooda::run_ooda(transport, llm, &config),
        )
        .await;

        let duration_ms = task_start.elapsed().as_millis() as u64;

        // Same source do_action.rs pumps: the app's own OkHttp log lines. A
        // transport that cannot read them returns empty, which the evaluator
        // reports as unobservable rather than as a failing app.
        let logcat = transport.capture_http_logs().await.unwrap_or_default();
        network.extend(NetworkSource::Logcat, logcat);
        let window = network.since(window_start_ms);

        let task_result = match ooda_result {
            Ok(Ok(result)) => {
                let outcomes: Vec<ExpectOutcome> = test_task
                    .expect_network
                    .iter()
                    .map(|e| evaluate(e, &window))
                    .collect();
                for (rule, outcome) in test_task.expect_network.iter().zip(&outcomes) {
                    match outcome {
                        ExpectOutcome::Met => {}
                        other => eprintln!(
                            "  {} network `{}`: {}",
                            if other.fails_task() { "❌" } else { "⚠️ " },
                            rule.url,
                            other.detail()
                        ),
                    }
                }
                TaskResult {
                    name: test_task.name.clone(),
                    task: test_task.task.clone(),
                    passed: result.success && !outcomes.iter().any(ExpectOutcome::fails_task),
                    steps: result.steps,
                    reasoning: result.final_reasoning,
                    duration_ms,
                    network_expectations: outcomes,
                }
            }
            Ok(Err(e)) => TaskResult {
                name: test_task.name.clone(),
                task: test_task.task.clone(),
                passed: false,
                steps: 0,
                reasoning: format!("Error: {:#}", e),
                duration_ms,
                network_expectations: Vec::new(),
            },
            Err(_) => TaskResult {
                name: test_task.name.clone(),
                task: test_task.task.clone(),
                passed: false,
                steps: 0,
                reasoning: format!("Timeout after {}s", timeout_secs),
                duration_ms,
                network_expectations: Vec::new(),
            },
        };

        let icon = if task_result.passed { "✅" } else { "❌" };
        eprintln!(
            "  {} {} ({} steps, {:.1}s)\n",
            icon,
            if task_result.passed {
                "PASSED"
            } else {
                "FAILED"
            },
            task_result.steps,
            task_result.duration_ms as f64 / 1000.0
        );

        results.push(task_result);
    }

    let passed = results.iter().filter(|r| r.passed).count();
    let failed = results.len() - passed;

    SuiteResult {
        app: suite.app.clone(),
        total: results.len(),
        passed,
        failed,
        results,
        duration_ms: suite_start.elapsed().as_millis() as u64,
    }
}

/// Format results as JUnit XML.
pub fn format_junit(result: &SuiteResult) -> String {
    let mut xml = String::new();
    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    xml.push_str(&format!(
        "<testsuites tests=\"{}\" failures=\"{}\" time=\"{:.3}\">\n",
        result.total,
        result.failed,
        result.duration_ms as f64 / 1000.0
    ));
    xml.push_str(&format!(
        "  <testsuite name=\"{}\" tests=\"{}\" failures=\"{}\">\n",
        result.app, result.total, result.failed
    ));

    for task in &result.results {
        xml.push_str(&format!(
            "    <testcase name=\"{}\" classname=\"{}\" time=\"{:.3}\"",
            escape_xml(&task.name),
            escape_xml(&result.app),
            task.duration_ms as f64 / 1000.0
        ));

        if task.passed {
            xml.push_str(" />\n");
        } else {
            xml.push_str(">\n");
            xml.push_str(&format!(
                "      <failure message=\"{}\">{}</failure>\n",
                escape_xml(&task.reasoning),
                escape_xml(&task.reasoning)
            ));
            xml.push_str("    </testcase>\n");
        }
    }

    xml.push_str("  </testsuite>\n");
    xml.push_str("</testsuites>\n");
    xml
}

/// Format results as JSON.
pub fn format_json(result: &SuiteResult) -> String {
    serde_json::to_string_pretty(result).unwrap_or_else(|_| "{}".to_string())
}

/// Format results as human-readable text.
pub fn format_human(result: &SuiteResult) -> String {
    let mut out = String::new();
    out.push_str(&format!("\n═══ Results: {} ═══\n", result.app));
    out.push_str(&format!(
        "  {} passed, {} failed, {} total ({:.1}s)\n\n",
        result.passed,
        result.failed,
        result.total,
        result.duration_ms as f64 / 1000.0
    ));

    for task in &result.results {
        let icon = if task.passed { "✅" } else { "❌" };
        out.push_str(&format!(
            "  {} {} — {} steps, {:.1}s\n",
            icon,
            task.name,
            task.steps,
            task.duration_ms as f64 / 1000.0
        ));
        if !task.passed {
            out.push_str(&format!("     Reason: {}\n", task.reasoning));
        }
    }
    out
}

/// Single-pass XML escaping (one allocation instead of 5 chained .replace() calls).
pub fn escape_xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_timeout_seconds() {
        assert_eq!(parse_timeout("60s"), 60);
        assert_eq!(parse_timeout("90s"), 90);
        assert_eq!(parse_timeout("120s"), 120);
    }

    #[test]
    fn test_parse_timeout_minutes() {
        assert_eq!(parse_timeout("2m"), 120);
        assert_eq!(parse_timeout("1m"), 60);
    }

    #[test]
    fn test_parse_timeout_bare_number() {
        assert_eq!(parse_timeout("30"), 30);
    }

    #[test]
    fn test_parse_timeout_invalid() {
        assert_eq!(parse_timeout("abc"), 60); // Default
    }

    #[test]
    fn test_load_suite_yaml() {
        let yaml = r#"
app: com.example.app
tasks:
  - name: login
    task: "Log in with test credentials"
    timeout: 60s
  - name: checkout
    task: "Complete checkout flow"
    timeout: 120s
"#;
        let suite: TestSuite = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(suite.app, "com.example.app");
        assert_eq!(suite.tasks.len(), 2);
        assert_eq!(suite.tasks[0].name, "login");
        assert_eq!(suite.tasks[1].timeout, "120s");
    }

    #[test]
    fn test_load_suite_default_timeout() {
        let yaml = r#"
app: com.app
tasks:
  - name: test1
    task: "do something"
"#;
        let suite: TestSuite = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(suite.tasks[0].timeout, "60s");
    }

    #[test]
    fn test_suite_result_exit_code() {
        let result = SuiteResult {
            app: "com.app".to_string(),
            total: 2,
            passed: 2,
            failed: 0,
            results: vec![],
            duration_ms: 1000,
        };
        assert_eq!(result.exit_code(), 0);

        let result_fail = SuiteResult {
            app: "com.app".to_string(),
            total: 2,
            passed: 1,
            failed: 1,
            results: vec![],
            duration_ms: 1000,
        };
        assert_eq!(result_fail.exit_code(), 1);
    }

    #[test]
    fn test_format_junit() {
        let result = SuiteResult {
            app: "com.app".to_string(),
            total: 2,
            passed: 1,
            failed: 1,
            results: vec![
                TaskResult {
                    name: "login".to_string(),
                    task: "Log in".to_string(),
                    passed: true,
                    steps: 5,
                    reasoning: "Done".to_string(),
                    duration_ms: 5000,
                    network_expectations: Vec::new(),
                },
                TaskResult {
                    name: "checkout".to_string(),
                    task: "Check out".to_string(),
                    passed: false,
                    steps: 10,
                    reasoning: "Stuck at payment".to_string(),
                    duration_ms: 12000,
                    network_expectations: Vec::new(),
                },
            ],
            duration_ms: 17000,
        };

        let xml = format_junit(&result);
        assert!(xml.contains("<?xml"));
        assert!(xml.contains("tests=\"2\""));
        assert!(xml.contains("failures=\"1\""));
        assert!(xml.contains("name=\"login\""));
        assert!(xml.contains("<failure"));
        assert!(xml.contains("Stuck at payment"));
    }

    #[test]
    fn test_format_json() {
        let result = SuiteResult {
            app: "com.app".to_string(),
            total: 1,
            passed: 1,
            failed: 0,
            results: vec![TaskResult {
                name: "test".to_string(),
                task: "Do thing".to_string(),
                passed: true,
                steps: 3,
                reasoning: "Done".to_string(),
                duration_ms: 2000,
                network_expectations: Vec::new(),
            }],
            duration_ms: 2000,
        };

        let json = format_json(&result);
        assert!(json.contains("\"passed\": 1"));
        assert!(json.contains("\"test\""));
    }

    #[test]
    fn test_format_human() {
        let result = SuiteResult {
            app: "com.app".to_string(),
            total: 1,
            passed: 0,
            failed: 1,
            results: vec![TaskResult {
                name: "login".to_string(),
                task: "Log in".to_string(),
                passed: false,
                steps: 8,
                reasoning: "App crashed".to_string(),
                duration_ms: 5000,
                network_expectations: Vec::new(),
            }],
            duration_ms: 5000,
        };

        let text = format_human(&result);
        assert!(text.contains("0 passed"));
        assert!(text.contains("1 failed"));
        assert!(text.contains("App crashed"));
    }

    #[test]
    fn test_escape_xml() {
        assert_eq!(escape_xml("a & b"), "a &amp; b");
        assert_eq!(escape_xml("<tag>"), "&lt;tag&gt;");
        assert_eq!(escape_xml("\"quoted\""), "&quot;quoted&quot;");
    }
}
