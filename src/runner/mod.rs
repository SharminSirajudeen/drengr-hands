//! `drengr test`: load a suite, run each task through the OODA loop, and
//! leave what the device saw beside the result: pass or fail, how the loop
//! stopped, and an evidence folder per task. What a failure *means* is the
//! caller's question; the raw facts are here, in the JSON and on disk.

mod format;

pub use format::{escape_xml, format_human, format_json, format_junit};

use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::evidence;
use crate::expect_network::{evaluate, ExpectNetwork, ExpectOutcome};
use crate::network::sink::{NetworkSink, NetworkSource};
use crate::ooda::{self, LlmClient, OodaConfig};
use crate::run_outcome::RunOutcomeKind;
use crate::transport::DeviceTransport;

/// A test suite loaded from drengr-tests.yml.
#[derive(Debug, Deserialize)]
pub struct TestSuite {
    pub app: String,
    pub tasks: Vec<TestTask>,
}

/// A single test task in the suite. Keys this runner does not read, such as
/// a wrapper's `paths:`, are kept out of its way rather than rejected.
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
    /// The device the suite ran on and the model that drove it, as the run
    /// reported them, so a page built from this file can say so.
    pub device: String,
    pub model: String,
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
    /// How the loop stopped: `judge_pass`, `self_reported`, `crash`,
    /// `step_cap`, `progress_stuck`, `duplicate_screen`, `timeout`, `error`.
    pub outcome: String,
    /// The loop's error, when `outcome` is `error`. Kept apart from
    /// `reasoning` so a caller never parses it back out of a sentence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Folder under the evidence directory holding this task's trail,
    /// screenshots, calls and, on failure, its device log and process verdict.
    pub evidence: String,
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

/// How to run, and where to leave what the run saw.
pub struct RunOptions<'a> {
    pub evidence_dir: &'a Path,
    /// The tasks to run, from `select_only`, so the choice is made once.
    pub tasks: &'a [&'a TestTask],
}

/// Load a test suite from a YAML file.
pub fn load_suite(path: &Path) -> Result<TestSuite> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read test file: {}", path.display()))?;
    serde_yaml::from_str(&content)
        .with_context(|| format!("Failed to parse test file: {}", path.display()))
}

/// The suite file as JSON, every key included, so a wrapper can read what it
/// adds to a task (such as `paths:`) without parsing YAML itself. The file
/// must still be a suite this runner would load.
pub fn suite_json(path: &Path) -> Result<String> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read test file: {}", path.display()))?;
    let value: serde_yaml::Value = serde_yaml::from_str(&content)
        .with_context(|| format!("Failed to parse test file: {}", path.display()))?;
    let _: TestSuite = serde_yaml::from_value(value.clone())
        .with_context(|| format!("Failed to parse test file: {}", path.display()))?;
    let json = serde_json::to_value(&value)
        .context("the suite holds a key JSON cannot carry (keys must be strings)")?;
    Ok(serde_json::to_string_pretty(&json)?)
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

/// The tasks to run: all of them, or the named ones in suite order. A name
/// the suite does not have is a mistake in the command, not an empty run.
pub fn select_only<'a>(suite: &'a TestSuite, only: Option<&[String]>) -> Result<Vec<&'a TestTask>> {
    let Some(only) = only else {
        return Ok(suite.tasks.iter().collect());
    };
    if let Some(missing) = only
        .iter()
        .find(|n| !suite.tasks.iter().any(|t| &t.name == *n))
    {
        anyhow::bail!("no task named `{missing}` in the suite");
    }
    Ok(suite
        .tasks
        .iter()
        .filter(|t| only.contains(&t.name))
        .collect())
}

struct SuiteCtx<'a> {
    suite: &'a TestSuite,
    transport: &'a dyn DeviceTransport,
    llm: &'a LlmClient,
    device_id: &'a str,
    network: &'a NetworkSink,
    evidence_dir: &'a Path,
}

/// Run the selected tasks sequentially, leaving one evidence folder per task.
pub async fn run_suite(
    suite: &TestSuite,
    transport: &dyn DeviceTransport,
    llm: &LlmClient,
    device_id: &str,
    network: &NetworkSink,
    opts: RunOptions<'_>,
) -> SuiteResult {
    let suite_start = Instant::now();
    let tasks = opts.tasks;
    if let Err(e) = std::fs::create_dir_all(opts.evidence_dir) {
        eprintln!(
            "  evidence folder {}: {} (the run continues without it)",
            opts.evidence_dir.display(),
            e
        );
    }

    let model = llm.describe();
    eprintln!("{}", model);
    eprintln!("Running {} tasks for {}\n", tasks.len(), suite.app);

    let ctx = SuiteCtx {
        suite,
        transport,
        llm,
        device_id,
        network,
        evidence_dir: opts.evidence_dir,
    };
    let mut results = Vec::new();
    for (i, test_task) in tasks.iter().enumerate() {
        eprintln!("─── Task {}/{}: {} ───", i + 1, tasks.len(), test_task.name);
        eprintln!("  \"{}\" (timeout: {})", test_task.task, test_task.timeout);
        let result = run_task(&ctx, suite_index(suite, test_task), test_task).await;
        print_task_outcome(&result);
        results.push(result);
    }

    let passed = results.iter().filter(|r| r.passed).count();
    SuiteResult {
        app: suite.app.clone(),
        device: device_id.to_string(),
        model: model.lines().next().unwrap_or_default().to_string(),
        total: results.len(),
        passed,
        failed: results.len() - passed,
        results,
        duration_ms: suite_start.elapsed().as_millis() as u64,
    }
}

/// A task's folder is numbered by its place in the suite, not in the
/// selection, so `--only checkout` lands in the same folder a full run uses.
fn suite_index(suite: &TestSuite, task: &TestTask) -> usize {
    suite
        .tasks
        .iter()
        .position(|t| std::ptr::eq(t, task))
        .unwrap_or(0)
}

async fn run_task(ctx: &SuiteCtx<'_>, index: usize, test_task: &TestTask) -> TaskResult {
    let timeout_secs = parse_timeout(&test_task.timeout);
    let max_steps = (timeout_secs / 2).max(10) as usize; // ~2s per step estimate
    let folder = evidence::task_folder(index, &test_task.name);
    let dir = ctx.evidence_dir.join(&folder);
    // A reused evidence folder must not show last run's frames or log beside
    // this run's. A folder that cannot be made is reported as no folder, not
    // as a name that leads nowhere.
    let _ = std::fs::remove_dir_all(&dir);
    let recording = match std::fs::create_dir_all(&dir) {
        Ok(()) => true,
        Err(e) => {
            eprintln!(
                "  evidence folder {}: {}; recording nothing",
                dir.display(),
                e
            );
            false
        }
    };

    let config = OodaConfig {
        task: test_task.task.clone(),
        app_package: ctx.suite.app.clone(),
        max_steps,
        device_id: ctx.device_id.to_string(),
        force_vision: false,
        verify_completion: true,
        // Tests declare their target app up front — cross-app navigation
        // in a test scenario is almost certainly prompt-injection.
        allowed_apps: Some(vec![ctx.suite.app.clone()]),
        trail_dir: recording.then(|| dir.clone()),
    };

    // The device log is this task's own from here. The OkHttp capture, the
    // log tail and the process verdict all read the buffer, so without the
    // clear a call or a crash from an earlier task would be charged to this
    // one. The MCP path clears before every action for the same reason.
    let _ = ctx.transport.clear_http_logs().await;
    let task_start = Instant::now();
    let window_start_ms = crate::session::now_ms();
    let ran = tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        ooda::run_ooda(ctx.transport, ctx.llm, &config),
    )
    .await;
    let duration_ms = task_start.elapsed().as_millis() as u64;

    // Same source do_action.rs pumps: the app's own OkHttp log lines. A
    // transport that cannot read them returns empty, which the evaluator
    // reports as unobservable rather than as a failing app.
    let logcat = ctx.transport.capture_http_logs().await.unwrap_or_default();
    ctx.network.extend(NetworkSource::Logcat, logcat);
    let window = ctx.network.since(window_start_ms);

    let (outcome, error, success, steps, reasoning) = match ran {
        Ok(Ok(r)) => (r.outcome, None, r.success, r.steps, r.final_reasoning),
        Ok(Err(e)) => {
            let msg = format!("{e:#}");
            let reasoning = format!("Error: {msg}");
            (RunOutcomeKind::Error, Some(msg), false, 0, reasoning)
        }
        Err(_) => (
            RunOutcomeKind::Timeout,
            None,
            false,
            0,
            format!("Timeout after {timeout_secs}s"),
        ),
    };
    // Expectations are evaluated only for a run the loop returned from. A
    // task that timed out or errored never reached the call it was meant to
    // make, and blaming the app for that would be a false failure.
    let network_expectations = match outcome {
        RunOutcomeKind::Error | RunOutcomeKind::Timeout => Vec::new(),
        _ => evaluate_expectations(test_task, &window),
    };
    let passed = success && !network_expectations.iter().any(ExpectOutcome::fails_task);
    let capture =
        evidence::capture_task(ctx.transport, &ctx.suite.app, &dir, &window, !passed).await;
    // A loop that never returned reports no step count of its own; the trail
    // it left on disk does.
    let steps = if steps == 0 {
        capture.trail.len()
    } else {
        steps
    };

    TaskResult {
        name: test_task.name.clone(),
        task: test_task.task.clone(),
        passed,
        steps,
        reasoning,
        duration_ms,
        outcome: outcome.as_str().to_string(),
        error,
        evidence: if recording { folder } else { String::new() },
        network_expectations,
    }
}

fn evaluate_expectations(
    test_task: &TestTask,
    window: &[crate::network::sink::SinkEntry],
) -> Vec<ExpectOutcome> {
    let outcomes: Vec<ExpectOutcome> = test_task
        .expect_network
        .iter()
        .map(|e| evaluate(e, window))
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
    outcomes
}

fn print_task_outcome(result: &TaskResult) {
    let verdict = if result.passed {
        "✅ PASSED"
    } else {
        "❌ FAILED"
    };
    eprintln!(
        "  {} ({} steps, {:.1}s, {})\n",
        verdict,
        result.steps,
        result.duration_ms as f64 / 1000.0,
        result.outcome
    );
}

/// A finished task for format tests.
#[cfg(test)]
pub(crate) fn done(name: &str, passed: bool, steps: usize, reasoning: &str, ms: u64) -> TaskResult {
    TaskResult {
        name: name.into(),
        task: format!("do {name}"),
        passed,
        steps,
        reasoning: reasoning.into(),
        duration_ms: ms,
        outcome: if passed { "judge_pass" } else { "step_cap" }.into(),
        error: None,
        evidence: format!("01-{name}"),
        network_expectations: Vec::new(),
    }
}

#[cfg(test)]
pub(crate) fn suite(results: Vec<TaskResult>) -> SuiteResult {
    let passed = results.iter().filter(|r| r.passed).count();
    SuiteResult {
        app: "com.app".into(),
        device: "emulator-5554".into(),
        model: "Using gemini · gemini-2.5-flash (default)".into(),
        total: results.len(),
        passed,
        failed: results.len() - passed,
        duration_ms: results.iter().map(|r| r.duration_ms).sum(),
        results,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two things in run_task no unit test can reach without a device, pinned
    /// by reading the source. Without the log clear, a call or a crash from an
    /// earlier task is charged to this one: the capture reads the whole buffer
    /// and stamps it now. Without the folder reset, a reused evidence dir shows
    /// last run's frames and log beside this run's.
    #[test]
    fn every_task_starts_from_a_cleared_log_and_an_empty_folder() {
        let src = std::fs::read_to_string("src/runner/mod.rs").expect("own source");
        let clear = format!("clear_http_{}(", "logs");
        let reset = format!("remove_dir_{}(", "all");
        let run = format!("run_{}(", "ooda");
        let fn_at = src.find("async fn run_task(").expect("run_task exists");
        let at = |needle: &str, what: &str| {
            src[fn_at..]
                .find(needle)
                .map(|i| fn_at + i)
                .unwrap_or_else(|| panic!("run_task no longer {what}"))
        };
        let run_at = at(&run, "runs the loop");
        assert!(
            at(&clear, "clears the device log") < run_at,
            "the clear must come before the loop starts"
        );
        assert!(
            at(&reset, "resets the task folder") < run_at,
            "the folder must be emptied before the loop writes into it"
        );
    }

    /// The timeout and error arms skip the expectations on purpose; a loop
    /// that returned is the only one whose window is complete.
    #[test]
    fn expectations_are_skipped_exactly_when_the_loop_never_returned() {
        let src = std::fs::read_to_string("src/runner/mod.rs").expect("own source");
        let arm = format!(
            "RunOutcomeKind::Error | RunOutcomeKind::{} => Vec::new(),",
            "Timeout"
        );
        assert_eq!(
            src.matches(&arm).count(),
            1,
            "the skip covers both arms and only them"
        );
    }

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
    paths: ["src/checkout/*"]
"#;
        let suite: TestSuite = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(suite.app, "com.example.app");
        assert_eq!(suite.tasks.len(), 2);
        assert_eq!(suite.tasks[0].name, "login");
        assert_eq!(suite.tasks[1].timeout, "120s");
    }

    #[test]
    fn a_key_this_runner_does_not_read_is_not_an_error() {
        let yaml = "app: com.app\ntasks:\n  - name: t\n    task: do\n    paths: [\"a/*\"]\n    owner: me\n";
        assert!(
            serde_yaml::from_str::<TestSuite>(yaml).is_ok(),
            "a wrapper's keys ride along"
        );
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
    fn suite_json_keeps_every_key_and_refuses_what_the_runner_would_not_load() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("drengr-tests.yml");
        std::fs::write(&p, "app: com.app\ntasks:\n  - name: t\n    task: do\n    paths: [\"a/*\"]\n    owner: me\n").unwrap();
        let json: serde_json::Value = serde_json::from_str(&suite_json(&p).unwrap()).unwrap();
        assert_eq!(json["app"], "com.app");
        assert_eq!(
            json["tasks"][0]["paths"][0], "a/*",
            "a key the runner ignores is still there"
        );
        assert_eq!(json["tasks"][0]["owner"], "me");
        std::fs::write(&p, "tasks:\n  - name: t\n    task: do\n").unwrap();
        let err = suite_json(&p).unwrap_err().to_string();
        assert!(
            err.contains("Failed to parse"),
            "a file without an app is not a suite: {err}"
        );
    }

    #[test]
    fn test_suite_result_exit_code() {
        assert_eq!(suite(vec![done("a", true, 1, "ok", 1)]).exit_code(), 0);
        assert_eq!(
            suite(vec![
                done("a", true, 1, "ok", 1),
                done("b", false, 1, "no", 1)
            ])
            .exit_code(),
            1
        );
    }

    #[test]
    fn a_task_is_numbered_by_its_place_in_the_suite_not_in_the_selection() {
        let s = three();
        let picked = select_only(&s, Some(&["settings".to_string()])).unwrap();
        assert_eq!(suite_index(&s, picked[0]), 2);
        assert_eq!(
            evidence::task_folder(suite_index(&s, picked[0]), &picked[0].name),
            "03-settings"
        );
    }

    fn three() -> TestSuite {
        serde_yaml::from_str("app: com.app\ntasks:\n  - {name: login, task: a}\n  - {name: checkout, task: b}\n  - {name: settings, task: c}\n").unwrap()
    }

    #[test]
    fn only_keeps_suite_order_and_refuses_a_name_the_suite_lacks() {
        let s = three();
        let all = select_only(&s, None).unwrap();
        assert_eq!(all.len(), 3);
        let some = select_only(&s, Some(&["settings".to_string(), "login".to_string()])).unwrap();
        let names: Vec<&str> = some.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["login", "settings"],
            "suite order, not argument order"
        );
        let err = select_only(&s, Some(&["payments".to_string()]))
            .unwrap_err()
            .to_string();
        assert_eq!(err, "no task named `payments` in the suite");
        assert!(
            select_only(&s, Some(&[])).unwrap().is_empty(),
            "an empty list runs nothing, on purpose"
        );
    }
}
