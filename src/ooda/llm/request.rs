use super::*;

/// An inference call is the one request here that legitimately outlives the
/// shared bound: a vision prompt on a loaded provider takes tens of seconds.
/// Before this it had no bound at all, so a provider that accepted the
/// connection and then went quiet hung the OODA loop forever.
pub(super) const LLM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Provider-agnostic rate-limit headroom from the response headers — OpenAI
/// uses `x-ratelimit-remaining-*`, Anthropic `anthropic-ratelimit-*-remaining`.
/// Returns (remaining_tokens, remaining_requests); the basis for backoff.
fn ratelimit_remaining(headers: &reqwest::header::HeaderMap) -> (Option<u32>, Option<u32>) {
    let get = |names: &[&str]| -> Option<u32> {
        names.iter().find_map(|n| {
            headers
                .get(*n)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.trim().parse().ok())
        })
    };
    (
        get(&[
            "x-ratelimit-remaining-tokens",
            "anthropic-ratelimit-tokens-remaining",
        ]),
        get(&[
            "x-ratelimit-remaining-requests",
            "anthropic-ratelimit-requests-remaining",
        ]),
    )
}

impl LlmClient {
    /// OpenAI-compatible chat completions (works for OpenAI, Gemini, Groq, Together, Fireworks, Ollama).
    pub(super) async fn complete_openai_compat(
        &self,
        prompt: &str,
        image_base64: Option<&str>,
        max_element: usize,
        allow_wait: bool,
    ) -> Result<String> {
        let url = format!("{}/chat/completions", self.base_url());

        let content = if let Some(img) = image_base64 {
            json!([
                {"type": "text", "text": prompt},
                {"type": "image_url", "image_url": {"url": format!("data:image/jpeg;base64,{}", img)}}
            ])
        } else {
            json!(prompt)
        };

        let mut body = json!({
            "model": self.model,
            "messages": [{"role": "user", "content": content}],
            "max_tokens": 256,
            "temperature": 0.2,
        });

        let mut schema_injected = false;
        if self.strict_output {
            match self.provider {
                LlmProvider::Ollama => {
                    body["response_format"] = json!({
                        "type": "json_schema",
                        "json_schema": {
                            "name": "ooda_decision",
                            "strict": true,
                            "schema": build_ollama_schema(max_element, allow_wait),
                        }
                    });
                    schema_injected = true;
                }
                LlmProvider::OpenAi
                | LlmProvider::Groq
                | LlmProvider::Together
                | LlmProvider::Fireworks => {
                    body["response_format"] = json!({"type": "json_object"});
                }
                LlmProvider::Gemini | LlmProvider::Anthropic => {}
            }
        }

        let send_request = |payload: &Value| {
            crate::http::client()
                .post(&url)
                .timeout(LLM_TIMEOUT)
                .header("Authorization", format!("Bearer {}", self.api_key))
                .header("Content-Type", "application/json")
                .json(payload)
                .send()
        };

        let response = send_request(&body)
            .await
            .context("LLM API request failed")?;
        let status = response.status();
        let (rl_tokens, rl_requests) = ratelimit_remaining(response.headers());
        let text = response
            .text()
            .await
            .context("Failed to read LLM response")?;

        let (final_status, final_text) =
            if status.as_u16() == 400 && self.provider == LlmProvider::Ollama && schema_injected {
                tracing::warn!(
                    "Ollama rejected schema (400), retrying prompt-only: {}",
                    truncate(&text, ERROR_PREVIEW_LEN)
                );
                if let Some(obj) = body.as_object_mut() {
                    obj.remove("response_format");
                }
                let retry = send_request(&body)
                    .await
                    .context("LLM API retry request failed")?;
                let retry_status = retry.status();
                let retry_text = retry
                    .text()
                    .await
                    .context("Failed to read LLM retry response")?;
                (retry_status, retry_text)
            } else {
                (status, text)
            };

        if !final_status.is_success() {
            anyhow::bail!(
                "LLM API error ({}): {}",
                final_status,
                truncate(&final_text, ERROR_PREVIEW_LEN)
            );
        }

        let parsed: Value =
            serde_json::from_str(&final_text).context("Failed to parse LLM response JSON")?;

        let content = parsed["choices"][0]["message"]["content"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "No content in LLM response: {}",
                    truncate(&final_text, ERROR_PREVIEW_LEN)
                )
            })?;
        self.set_stats(CallStats {
            prompt_tokens: parsed["usage"]["prompt_tokens"].as_u64().map(|n| n as u32),
            completion_tokens: parsed["usage"]["completion_tokens"]
                .as_u64()
                .map(|n| n as u32),
            finish_reason: parsed["choices"][0]["finish_reason"]
                .as_str()
                .map(String::from),
            model: parsed["model"].as_str().map(String::from),
            ratelimit_remaining_tokens: rl_tokens,
            ratelimit_remaining_requests: rl_requests,
        });
        tracing::debug!("RAW LLM RESPONSE: {}", content);
        Ok(content)
    }

    /// Anthropic messages API.
    pub(super) async fn complete_anthropic(
        &self,
        prompt: &str,
        image_base64: Option<&str>,
        _max_element: usize,
    ) -> Result<String> {
        let url = format!("{}/messages", self.base_url());

        let content = if let Some(img) = image_base64 {
            json!([
                {"type": "image", "source": {"type": "base64", "media_type": "image/jpeg", "data": img}},
                {"type": "text", "text": prompt}
            ])
        } else {
            json!([{"type": "text", "text": prompt}])
        };

        let body = json!({
            "model": self.model,
            "max_tokens": 256,
            "messages": [{"role": "user", "content": content}],
        });

        let response = crate::http::client()
            .post(&url)
            .timeout(LLM_TIMEOUT)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .context("Anthropic API request failed")?;

        let status = response.status();
        let (rl_tokens, rl_requests) = ratelimit_remaining(response.headers());
        let text = response
            .text()
            .await
            .context("Failed to read Anthropic response")?;

        if !status.is_success() {
            anyhow::bail!(
                "Anthropic API error ({}): {}",
                status,
                truncate(&text, ERROR_PREVIEW_LEN)
            );
        }

        let parsed: Value =
            serde_json::from_str(&text).context("Failed to parse Anthropic response JSON")?;

        self.set_stats(CallStats {
            prompt_tokens: parsed["usage"]["input_tokens"].as_u64().map(|n| n as u32),
            completion_tokens: parsed["usage"]["output_tokens"].as_u64().map(|n| n as u32),
            finish_reason: parsed["stop_reason"].as_str().map(String::from),
            model: parsed["model"].as_str().map(String::from),
            ratelimit_remaining_tokens: rl_tokens,
            ratelimit_remaining_requests: rl_requests,
        });
        parsed["content"][0]["text"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("No content in Anthropic response"))
    }
}
