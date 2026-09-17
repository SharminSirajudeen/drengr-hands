use super::*;

/// The judge's answer, including the case where there is no answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JudgeVerdict {
    /// The scene shows the task finished.
    Complete,
    /// The judge looked and says it is not finished, with its reason.
    NotComplete(String),
    /// The judge could not be asked. Not a verdict, and never a rejection.
    Unavailable(String),
}

impl JudgeVerdict {
    /// True only when the judge actually answered.
    pub fn answered(&self) -> bool {
        !matches!(self, Self::Unavailable(_))
    }

    pub fn reason(&self) -> &str {
        match self {
            Self::Complete => "complete",
            Self::NotComplete(r) | Self::Unavailable(r) => r,
        }
    }
}

impl LlmClient {
    /// Binary goal-completion check. Returns `(done, reason)`. Never fails —
    /// transport errors surface as `(false, err_message)` so the OODA loop
    /// continues rather than aborting.
    /// Whether the task is finished, or that we could not find out.
    ///
    /// `Unavailable` is not a rejection. Collapsing "the judge could not be
    /// reached" into "the judge said no" means one provider hiccup refuses every
    /// completion and the run burns its whole step budget against a device that
    /// already finished.
    pub async fn check_goal_complete(
        &self,
        task: &str,
        last_action: &str,
        current_scene: &str,
    ) -> JudgeVerdict {
        if self.provider.is_anthropic_api() {
            tracing::warn!(
                "check_goal_complete skipped — Anthropic provider not supported by judge"
            );
            return JudgeVerdict::Unavailable("judge not supported for anthropic path".to_string());
        }

        let prompt = format!(
            "TASK: {}\n\nLAST ACTION: {}\n\nCURRENT SCREEN:\n{}\n\n\
             Has the TASK been fully achieved based ONLY on CURRENT SCREEN?\n\
             - Break TASK into sub-goals (split on 'and', 'then', commas).\n\
             - Every sub-goal needs explicit on-screen evidence. No inference.\n\
             - 'Tapping the right thing' is NOT completion — the screen must show the resulting state.\n\
             - If any sub-goal is unverified, respond done=false.\n\n\
             Respond: {{\"done\": true|false, \"reason\": \"<sub-goal>: <evidence or what's missing>\"}}",
            task, last_action, current_scene
        );

        let body = build_judge_body(&self.provider, &self.model, &prompt);

        let url = format!("{}/chat/completions", self.base_url());
        let response = match crate::http::client()
            .post(&url)
            .timeout(super::request::LLM_TIMEOUT)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    "check_goal_complete: request failed ({:?}): {}",
                    self.provider,
                    e
                );
                return JudgeVerdict::Unavailable(format!("judge request failed: {}", e));
            }
        };

        let text = match response.text().await {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(
                    "check_goal_complete: response read failed ({:?}): {}",
                    self.provider,
                    e
                );
                return JudgeVerdict::Unavailable(format!("judge response read failed: {}", e));
            }
        };

        let parsed: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    "check_goal_complete: non-JSON response ({:?}): {}",
                    self.provider,
                    e
                );
                return JudgeVerdict::Unavailable("judge returned non-JSON".to_string());
            }
        };

        let content = match parsed["choices"][0]["message"]["content"].as_str() {
            Some(c) => c,
            None => {
                tracing::warn!("check_goal_complete: missing content ({:?})", self.provider);
                return JudgeVerdict::Unavailable("judge response missing content".to_string());
            }
        };

        let verdict_str = crate::ooda::prompt::extract_json(content);

        let verdict: Value = match serde_json::from_str(verdict_str) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    "check_goal_complete: verdict JSON invalid ({:?}): {}",
                    self.provider,
                    e
                );
                return JudgeVerdict::Unavailable(format!(
                    "judge JSON invalid: {}",
                    truncate(verdict_str, VERDICT_PREVIEW_LEN)
                ));
            }
        };

        let done = verdict["done"].as_bool().unwrap_or(false);
        let reason = verdict["reason"]
            .as_str()
            .unwrap_or("(no reason given)")
            .to_string();

        tracing::debug!("JUDGE VERDICT: done={} reason={}", done, reason);
        if done {
            JudgeVerdict::Complete
        } else {
            JudgeVerdict::NotComplete(reason)
        }
    }

    /// Three-way progress verdict; mirrors `check_goal_complete` behavior.
    /// Never panics — error paths return `(MakingProgress, "<error>")` so the
    /// OODA loop continues unblocked.
    pub async fn check_progress(
        &self,
        task: &str,
        recent_scenes: &[&str],
    ) -> (ProgressVerdict, String) {
        if self.provider.is_anthropic_api() {
            tracing::warn!("check_progress skipped — Anthropic provider not supported by judge");
            return (
                ProgressVerdict::MakingProgress,
                "judge not supported for anthropic path".to_string(),
            );
        }

        let mut joined = String::new();
        for (i, s) in recent_scenes.iter().enumerate() {
            joined.push_str(&format!("--- scene {} ---\n{}\n", i + 1, s));
        }
        let prompt = format!(
            "TASK: {}\n\nRECENT SCREENS (oldest first):\n{}\n\
             Verdict on the agent's progress toward TASK based ONLY on the screens above:\n\
             - 'goal_visible' = TASK target is on the current screen, ready to be acted on or done.\n\
             - 'progress'     = the screens show meaningful movement toward TASK.\n\
             - 'stuck'        = no useful change; agent is looping or off-track.\n\n\
             Respond: {{\"verdict\": \"progress\"|\"stuck\"|\"goal_visible\", \"reason\": \"<brief>\"}}",
            task, joined
        );

        let mut body = build_judge_body(&self.provider, &self.model, &prompt);
        body["max_tokens"] = json!(150);

        let url = format!("{}/chat/completions", self.base_url());
        let response = match crate::http::client()
            .post(&url)
            .timeout(super::request::LLM_TIMEOUT)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    "check_progress: request failed ({:?}): {}",
                    self.provider,
                    e
                );
                return (
                    ProgressVerdict::MakingProgress,
                    format!("progress request failed: {}", e),
                );
            }
        };

        let text = match response.text().await {
            Ok(t) => t,
            Err(e) => {
                return (
                    ProgressVerdict::MakingProgress,
                    format!("progress response read failed: {}", e),
                );
            }
        };

        let parsed: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => {
                return (
                    ProgressVerdict::MakingProgress,
                    "progress returned non-JSON".to_string(),
                );
            }
        };

        let content = match parsed["choices"][0]["message"]["content"].as_str() {
            Some(c) => c,
            None => {
                return (
                    ProgressVerdict::MakingProgress,
                    "progress response missing content".to_string(),
                );
            }
        };

        let verdict_str = crate::ooda::prompt::extract_json(content);
        let verdict_json: Value = match serde_json::from_str(verdict_str) {
            Ok(v) => v,
            Err(e) => {
                return (
                    ProgressVerdict::MakingProgress,
                    format!(
                        "progress JSON invalid: {}",
                        truncate(&format!("{}", e), VERDICT_PREVIEW_LEN)
                    ),
                );
            }
        };

        let verdict = ProgressVerdict::from_str(verdict_json["verdict"].as_str().unwrap_or(""));
        let reason = verdict_json["reason"]
            .as_str()
            .unwrap_or("(no reason given)")
            .to_string();
        tracing::debug!("PROGRESS VERDICT: {:?} reason={}", verdict, reason);
        (verdict, reason)
    }
}
