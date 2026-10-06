//! The three ways a suite result leaves the process: JUnit for CI dashboards,
//! JSON for scripts and wrappers, text for a person at a terminal.

use super::SuiteResult;

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
        escape_xml(&result.app),
        result.total,
        result.failed
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
                "      <failure message=\"{}\" type=\"{}\">{}</failure>\n",
                escape_xml(&task.reasoning),
                escape_xml(&task.outcome),
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
            out.push_str(&format!(
                "     Reason: {} ({})\n",
                task.reasoning, task.outcome
            ));
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
    use crate::runner::{done, suite};

    #[test]
    fn test_format_junit() {
        let xml = format_junit(&suite(vec![
            done("login", true, 5, "Done", 5000),
            done("checkout", false, 10, "Stuck at payment", 12000),
        ]));
        assert!(xml.contains("<?xml"));
        assert!(xml.contains("tests=\"2\""));
        assert!(xml.contains("failures=\"1\""));
        assert!(xml.contains("name=\"login\""));
        assert!(xml.contains("<failure message=\"Stuck at payment\" type=\"step_cap\">"));
    }

    #[test]
    fn test_format_json() {
        let json = format_json(&suite(vec![done("test", true, 3, "Done", 2000)]));
        assert!(json.contains("\"passed\": 1"));
        assert!(json.contains("\"test\""));
        assert!(json.contains("\"outcome\": \"judge_pass\""));
        assert!(json.contains("\"evidence\": \"01-test\""));
        assert!(!json.contains("\"error\""), "no error when there was none");
    }

    #[test]
    fn test_format_human() {
        let text = format_human(&suite(vec![done("login", false, 8, "App crashed", 5000)]));
        assert!(text.contains("0 passed"));
        assert!(text.contains("1 failed"));
        assert!(text.contains("App crashed (step_cap)"));
    }

    #[test]
    fn test_escape_xml() {
        assert_eq!(escape_xml("a & b"), "a &amp; b");
        assert_eq!(escape_xml("<tag>"), "&lt;tag&gt;");
        assert_eq!(escape_xml("\"quoted\""), "&quot;quoted&quot;");
    }
}
