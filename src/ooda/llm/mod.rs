use anyhow::{Context, Result};
use serde_json::{json, Value};

/// Max characters of an upstream LLM response/error to log or surface.
/// Generous enough to keep the start of an error message readable, capped to
/// avoid blowing out logs on multi-KB error bodies.
const ERROR_PREVIEW_LEN: usize = 200;

/// Max characters of an inline judge-verdict snippet quoted into a status
/// string. Smaller than `ERROR_PREVIEW_LEN` because verdicts are short and
/// these strings flow through user-facing reasoning fields.
const VERDICT_PREVIEW_LEN: usize = 80;

/// Verdict from `LlmClient::check_progress` — three-way meta-LLM judgement
/// over recent screen scenes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressVerdict {
    MakingProgress,
    Stuck,
    GoalVisible,
}

impl ProgressVerdict {
    pub(crate) fn from_str(s: &str) -> Self {
        // Unknown verdicts default to Stuck — over-flagging is safer than
        // missing a real stall (the only consequence is an extra prompt hint).
        match s.trim().to_lowercase().as_str() {
            "goal_visible" => ProgressVerdict::GoalVisible,
            "progress" | "making_progress" => ProgressVerdict::MakingProgress,
            "stuck" => ProgressVerdict::Stuck,
            _ => ProgressVerdict::Stuck,
        }
    }
}

/// Supported LLM providers for the OODA agent.
#[derive(Debug, Clone, PartialEq)]
pub enum LlmProvider {
    /// OpenAI (gpt-4o, gpt-4o-mini)
    OpenAi,
    /// Google Gemini (via OpenAI-compatible endpoint)
    Gemini,
    /// Anthropic Claude (via messages API)
    Anthropic,
    /// Groq (via OpenAI-compatible endpoint)
    Groq,
    /// Together AI (via OpenAI-compatible endpoint)
    Together,
    /// Fireworks AI (via OpenAI-compatible endpoint)
    Fireworks,
    /// Local Ollama (via OpenAI-compatible endpoint)
    Ollama,
}

fn normalize_base_url(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_end_matches('/');
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

impl LlmProvider {
    /// Parse from string (env var DRENGR_VISION_PROVIDER).
    pub fn from_str_or_default(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "openai" => Self::OpenAi,
            "gemini" | "google" => Self::Gemini,
            "anthropic" | "claude" => Self::Anthropic,
            "groq" => Self::Groq,
            "together" => Self::Together,
            "fireworks" => Self::Fireworks,
            "ollama" | "local" => Self::Ollama,
            "" => Self::Gemini,
            unknown => {
                tracing::warn!(
                    "Unknown DRENGR_VISION_PROVIDER '{}' — defaulting to Gemini",
                    unknown
                );
                Self::Gemini
            }
        }
    }

    /// Base URL for this provider's API; `DRENGR_BASE_URL` overrides it so any
    /// OpenAI-compatible endpoint works without a new provider variant.
    fn default_base_url(&self) -> String {
        match self {
            Self::OpenAi => "https://api.openai.com/v1".to_string(),
            Self::Gemini => "https://generativelanguage.googleapis.com/v1beta/openai".to_string(),
            Self::Anthropic => "https://api.anthropic.com/v1".to_string(),
            Self::Groq => "https://api.groq.com/openai/v1".to_string(),
            Self::Together => "https://api.together.xyz/v1".to_string(),
            Self::Fireworks => "https://api.fireworks.ai/inference/v1".to_string(),
            Self::Ollama => "http://localhost:11434/v1".to_string(),
        }
    }

    /// Get the default model for this provider. Every default must accept image
    /// input — the OODA loop escalates to vision when elements are unlabeled.
    fn default_model(&self) -> &str {
        match self {
            Self::OpenAi => "gpt-4o-mini",
            Self::Gemini => "gemini-3.1-flash-lite",
            Self::Anthropic => "claude-haiku-4-5",
            Self::Groq => "qwen/qwen3.6-27b",
            Self::Together => "Qwen/Qwen3.5-9B",
            Self::Fireworks => "accounts/fireworks/models/minimax-m3",
            Self::Ollama => "qwen2.5vl:7b",
        }
    }

    /// Whether this provider uses the Anthropic messages API (not OpenAI-compatible).
    fn is_anthropic_api(&self) -> bool {
        matches!(self, Self::Anthropic)
    }

    /// The provider's stable lowercase name — the spelling `DRENGR_VISION_PROVIDER`
    /// accepts and the key store files its API key under.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Gemini => "gemini",
            Self::Anthropic => "anthropic",
            Self::Groq => "groq",
            Self::Together => "together",
            Self::Fireworks => "fireworks",
            Self::Ollama => "ollama",
        }
    }
}

/// Simple LLM client for the OODA agent.
pub struct LlmClient {
    provider: LlmProvider,
    api_key: String,
    /// Resolved once at construction: a `drengr_` key routes to our gateway, an
    /// explicit DRENGR_BASE_URL wins over both, otherwise the provider default.
    base_url: String,
    model: String,
    strict_output: bool,
}

mod judge;
pub use judge::JudgeVerdict;
mod request;

impl LlmClient {
    /// Create from environment variables.
    pub fn from_env() -> Result<Self> {
        let provider_str = std::env::var("DRENGR_VISION_PROVIDER").unwrap_or_default();
        let provider = LlmProvider::from_str_or_default(&provider_str);

        let api_key = resolve_api_key(&provider)?;
        let base_url = resolve_base_url(&provider);
        let model = std::env::var("DRENGR_MODEL")
            .ok()
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| provider.default_model().to_string());

        let strict_output = std::env::var("DRENGR_STRICT_OUTPUT").map_or(true, |v| v != "0");

        Ok(Self {
            provider,
            api_key,
            base_url,
            model,
            strict_output,
        })
    }

    /// Create with explicit config (for testing).
    pub fn new(provider: LlmProvider, api_key: String, model: Option<String>) -> Self {
        let model = model.unwrap_or_else(|| provider.default_model().to_string());
        let base_url = resolve_base_url(&provider);
        Self {
            provider,
            api_key,
            base_url,
            model,
            strict_output: true,
        }
    }

    /// The endpoint every wire call goes to, after the gateway and override rules.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Send a text-only prompt. `allow_wait` drops `wait` from the schema enum
    /// when the agent is looping.
    pub async fn complete(
        &self,
        prompt: &str,
        max_element: usize,
        allow_wait: bool,
    ) -> Result<String> {
        if self.provider.is_anthropic_api() {
            self.complete_anthropic(prompt, None, max_element).await
        } else {
            self.complete_openai_compat(prompt, None, max_element, allow_wait)
                .await
        }
    }

    /// Send a prompt with an image to the LLM. Returns the response text.
    pub async fn complete_with_image(
        &self,
        prompt: &str,
        image_base64: &str,
        max_element: usize,
        allow_wait: bool,
    ) -> Result<String> {
        if self.provider.is_anthropic_api() {
            self.complete_anthropic(prompt, Some(image_base64), max_element)
                .await
        } else {
            self.complete_openai_compat(prompt, Some(image_base64), max_element, allow_wait)
                .await
        }
    }

    pub fn provider(&self) -> &LlmProvider {
        &self.provider
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    /// One line naming what will be called, where that choice came from, and how
    /// to change it — printed before a run so a failure is never a mystery.
    pub fn describe(&self) -> String {
        let chosen = std::env::var("DRENGR_MODEL")
            .ok()
            .is_some_and(|m| !m.trim().is_empty());
        let origin = if chosen {
            "from DRENGR_MODEL".to_string()
        } else {
            "default, accepts images — override with DRENGR_MODEL".to_string()
        };
        let mut line = format!(
            "Using {} · {} ({})",
            self.provider.as_str(),
            self.model,
            origin
        );
        line.push_str(&format!("\n  endpoint: {}", self.base_url));
        line
    }

    /// Pre-load the model into memory to eliminate the first-inference cold-start penalty.
    /// Only does anything for Ollama (local). No-op for cloud providers.
    pub async fn warmup(&self) -> Result<()> {
        if self.provider != LlmProvider::Ollama {
            return Ok(());
        }

        let url = format!("{}/chat/completions", self.base_url);
        let body = json!({
            "model": self.model,
            "messages": [{"role": "user", "content": "ready"}],
            "max_tokens": 1,
            "temperature": 0.0,
        });

        crate::http::client()
            .post(&url)
            .timeout(request::LLM_TIMEOUT)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .context("Ollama warmup request failed")?;

        Ok(())
    }
}

/// Build the chat-completions body for the judge call. Conditionally adds
/// `response_format` so we never serialize a `null` value (Gemini rejects it).
pub(crate) fn build_judge_body(provider: &LlmProvider, model: &str, prompt: &str) -> Value {
    let mut body = json!({
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "max_tokens": 128,
        "temperature": 0.0,
    });

    match provider {
        LlmProvider::Ollama => {
            body["response_format"] = json!({
                "type": "json_schema",
                "json_schema": {
                    "name": "goal_verdict",
                    "strict": true,
                    "schema": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["done", "reason"],
                        "properties": {
                            "done": {"type": "boolean"},
                            "reason": {"type": "string"}
                        }
                    }
                }
            });
        }
        LlmProvider::OpenAi
        | LlmProvider::Groq
        | LlmProvider::Together
        | LlmProvider::Fireworks => {
            body["response_format"] = json!({"type": "json_object"});
        }
        LlmProvider::Gemini | LlmProvider::Anthropic => {}
    }

    body
}

fn build_ollama_schema(max_element: usize, allow_wait: bool) -> Value {
    // One branch per action, each requiring exactly what its executor reads. A
    // single flat object with `required: [action, reasoning, done]` let the model
    // emit `open_app` with no `name`; the executor rejected it, the prompt was
    // unchanged, and the run burned its whole step budget re-proposing it. With a
    // branch per action the grammar cannot express that action at all — the model
    // either supplies the parameter or picks a different action.
    fn branch(action: &str, required: &[&str], props: Value) -> Value {
        let mut properties = json!({
            "action": {"const": action},
            "reasoning": {"type": "string"},
            "done": {"type": "boolean"}
        });
        if let (Some(dst), Some(src)) = (properties.as_object_mut(), props.as_object()) {
            for (k, v) in src {
                dst.insert(k.clone(), v.clone());
            }
        }
        let mut req: Vec<&str> = vec!["action", "reasoning", "done"];
        req.extend_from_slice(required);
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": req,
            "properties": properties
        })
    }

    let element = json!({
        "element": {"type": "integer", "minimum": 1, "maximum": max_element.max(1)}
    });

    let mut branches = vec![];
    // No addressable elements means no element to name, so those actions are not
    // offered rather than offered and guaranteed to fail.
    if max_element > 0 {
        branches.push(branch("tap", &["element"], element.clone()));
        branches.push(branch("long_press", &["element"], element.clone()));
    }
    branches.push(branch(
        "type",
        &["text"],
        json!({"text": {"type": "string"}, "element": element["element"].clone()}),
    ));
    branches.push(branch(
        "scroll",
        &["direction"],
        json!({"direction": {"type": "string", "enum": ["up", "down", "left", "right"]}}),
    ));
    branches.push(branch("press_back", &[], json!({})));
    if allow_wait {
        branches.push(branch("wait", &[], json!({})));
    }
    branches.push(branch(
        "open_app",
        &["name"],
        json!({"name": {"type": "string"}}),
    ));
    branches.push(branch(
        "draw_path",
        &["points"],
        json!({
            "points": {
                "type": "array",
                "minItems": 2,
                "items": {"type": "array", "minItems": 2, "maxItems": 2, "items": {"type": "integer"}}
            },
            "duration_ms": {"type": "integer", "minimum": 0}
        }),
    ));
    branches.push(branch("done", &[], json!({})));

    json!({ "anyOf": branches })
}

/// Resolve the API key from environment variables.
fn resolve_base_url(provider: &LlmProvider) -> String {
    if let Ok(explicit) = std::env::var("DRENGR_BASE_URL") {
        if let Some(u) = normalize_base_url(&explicit) {
            return u;
        }
    }
    provider.default_base_url()
}

fn resolve_api_key(provider: &LlmProvider) -> Result<String> {
    // Try universal key first. A set-but-empty var must fall through, not win —
    // CI runners routinely export empty strings for unset inputs.
    if let Ok(key) = std::env::var("DRENGR_API_KEY") {
        if !key.trim().is_empty() {
            return Ok(key);
        }
    }

    // Try provider-specific keys (obfuscated to hide from `strings` scan)
    let provider_key: String = match provider {
        LlmProvider::OpenAi => "OPENAI_API_KEY".to_string(),
        LlmProvider::Gemini => "GEMINI_API_KEY".to_string(),
        LlmProvider::Anthropic => "ANTHROPIC_API_KEY".to_string(),
        LlmProvider::Groq => "GROQ_API_KEY".to_string(),
        LlmProvider::Together => "TOGETHER_API_KEY".to_string(),
        LlmProvider::Fireworks => "FIREWORKS_API_KEY".to_string(),
        LlmProvider::Ollama => return Ok("ollama".to_string()),
    };

    // Try env var
    if let Ok(key) = std::env::var(&provider_key) {
        if !key.trim().is_empty() {
            return Ok(key);
        }
    }

    // Try stored keys (~/.drengr/llm_keys.json)
    let provider_name = match provider {
        LlmProvider::OpenAi => "openai",
        LlmProvider::Gemini => "gemini",
        LlmProvider::Anthropic => "anthropic",
        LlmProvider::Groq => "groq",
        LlmProvider::Together => "together",
        LlmProvider::Fireworks => "fireworks",
        LlmProvider::Ollama => unreachable!(),
    };
    if let Some(key) = crate::key_store::KeyStore::load().get(provider_name) {
        // SECURITY: never log raw key values
        tracing::debug!("Using stored key for {}", provider_name);
        return Ok(key.to_string());
    }

    Err(anyhow::anyhow!(
        "No API key found for {}. A key is only needed to drive the device autonomously (`drengr run`, `drengr test`).\n\
         Set one with `drengr key set {} YOUR_KEY` (or the {} / {} env var).\n\
         No key? Drive the device yourself — you are the brain, no key needed:\n\
         drengr look · drengr do · drengr query  (or `drengr mcp` for your MCP client).",
        provider_name,
        provider_name,
        "DRENGR_API_KEY",
        provider_key
    ))
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    // Find a valid char boundary at or before `max`
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Action names the branch schema offers, for the tests below.
    fn schema_actions(schema: &Value) -> Vec<String> {
        schema["anyOf"]
            .as_array()
            .expect("anyOf branches")
            .iter()
            .filter_map(|b| {
                b["properties"]["action"]["const"]
                    .as_str()
                    .map(String::from)
            })
            .collect()
    }

    /// The branch for one action, if it is offered.
    fn schema_branch<'a>(schema: &'a Value, action: &str) -> Option<&'a Value> {
        schema["anyOf"]
            .as_array()?
            .iter()
            .find(|b| b["properties"]["action"]["const"] == action)
    }

    #[test]
    fn test_provider_parsing() {
        assert_eq!(
            LlmProvider::from_str_or_default("openai"),
            LlmProvider::OpenAi
        );
        assert_eq!(
            LlmProvider::from_str_or_default("GEMINI"),
            LlmProvider::Gemini
        );
        assert_eq!(
            LlmProvider::from_str_or_default("google"),
            LlmProvider::Gemini
        );
        assert_eq!(
            LlmProvider::from_str_or_default("anthropic"),
            LlmProvider::Anthropic
        );
        assert_eq!(
            LlmProvider::from_str_or_default("claude"),
            LlmProvider::Anthropic
        );
        assert_eq!(LlmProvider::from_str_or_default("groq"), LlmProvider::Groq);
        assert_eq!(
            LlmProvider::from_str_or_default("ollama"),
            LlmProvider::Ollama
        );
        assert_eq!(
            LlmProvider::from_str_or_default("local"),
            LlmProvider::Ollama
        );
        // Default to Gemini for unknown
        assert_eq!(
            LlmProvider::from_str_or_default("unknown"),
            LlmProvider::Gemini
        );
        assert_eq!(LlmProvider::from_str_or_default(""), LlmProvider::Gemini);
    }

    #[test]
    fn describe_reports_the_endpoint_the_client_will_actually_use() {
        let llm = LlmClient {
            provider: LlmProvider::OpenAi,
            api_key: "sk-abc".to_string(),
            base_url: "https://endpoint.invalid/v1".to_string(),
            model: "test-model".to_string(),
            strict_output: false,
        };
        assert!(
            llm.describe().contains(llm.base_url()),
            "describe() must print the resolved endpoint, not a re-read of the override var; got {:?}",
            llm.describe()
        );
    }

    #[test]
    fn every_provider_keeps_its_own_endpoint_unless_overridden() {
        std::env::remove_var("DRENGR_BASE_URL");

        assert!(resolve_base_url(&LlmProvider::OpenAi).contains("openai.com"));
        assert!(resolve_base_url(&LlmProvider::Groq).contains("groq.com"));
        assert!(resolve_base_url(&LlmProvider::Anthropic).contains("anthropic.com"));
        assert!(resolve_base_url(&LlmProvider::Ollama).contains("localhost:11434"));

        std::env::set_var("DRENGR_BASE_URL", "https://openrouter.ai/api/v1");
        for p in [
            LlmProvider::OpenAi,
            LlmProvider::Anthropic,
            LlmProvider::Ollama,
        ] {
            assert_eq!(resolve_base_url(&p), "https://openrouter.ai/api/v1");
        }
        std::env::remove_var("DRENGR_BASE_URL");
    }

    // Every wire call must read the client's resolved endpoint. When two
    // resolutions existed, judge.rs used the gateway and request.rs, the path that
    // does the actual work, went straight to the provider: a paid key silently
    // bypassed billing on the only calls that matter.
    #[test]
    fn no_call_site_resolves_its_own_endpoint() {
        // Built at runtime so this assertion does not match its own source.
        let needle = format!("provider.{}()", "base_url");
        for f in ["mod.rs", "request.rs", "judge.rs"] {
            let src = std::fs::read_to_string(format!("src/ooda/llm/{f}")).unwrap();
            assert!(
                !src.contains(&needle),
                "{f} resolves its own endpoint instead of using the client's"
            );
        }
    }

    #[test]
    fn a_byo_key_keeps_its_own_provider() {
        let c = LlmClient::new(LlmProvider::Anthropic, "sk-ant-abc".into(), None);
        assert_eq!(*c.provider(), LlmProvider::Anthropic);
        assert!(c.base_url().contains("anthropic.com"));
    }

    #[test]
    fn test_normalize_base_url() {
        assert_eq!(
            normalize_base_url("https://openrouter.ai/api/v1/"),
            Some("https://openrouter.ai/api/v1".to_string())
        );
        assert_eq!(
            normalize_base_url("  http://localhost:4000  "),
            Some("http://localhost:4000".to_string())
        );
        assert_eq!(normalize_base_url(""), None);
        assert_eq!(normalize_base_url("   "), None);
        assert_eq!(normalize_base_url("/"), None);
    }

    #[test]
    fn test_provider_urls() {
        assert!(LlmProvider::OpenAi
            .default_base_url()
            .contains("openai.com"));
        assert!(LlmProvider::Gemini
            .default_base_url()
            .contains("googleapis"));
        assert!(LlmProvider::Anthropic
            .default_base_url()
            .contains("anthropic.com"));
        assert!(LlmProvider::Groq.default_base_url().contains("groq.com"));
        assert!(LlmProvider::Ollama.default_base_url().contains("localhost"));
    }

    #[test]
    fn test_provider_models() {
        assert!(LlmProvider::OpenAi.default_model().contains("gpt"));
        assert!(LlmProvider::Gemini.default_model().contains("gemini"));
        assert!(LlmProvider::Anthropic.default_model().contains("claude"));
    }

    #[test]
    fn test_is_anthropic_api() {
        assert!(LlmProvider::Anthropic.is_anthropic_api());
        assert!(!LlmProvider::OpenAi.is_anthropic_api());
        assert!(!LlmProvider::Gemini.is_anthropic_api());
    }

    #[test]
    fn test_client_constructor() {
        let client = LlmClient::new(LlmProvider::OpenAi, "test-key".to_string(), None);
        assert_eq!(client.provider(), &LlmProvider::OpenAi);
        assert!(client.model().contains("gpt"));
    }

    #[test]
    fn test_client_custom_model() {
        let client = LlmClient::new(
            LlmProvider::OpenAi,
            "key".to_string(),
            Some("gpt-4o".to_string()),
        );
        assert_eq!(client.model(), "gpt-4o");
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("hello world", 5), "hello");
        assert_eq!(truncate("hi", 10), "hi");
        assert_eq!(truncate("", 5), "");
    }

    #[test]
    fn test_ollama_schema_field_present() {
        let schema = build_ollama_schema(5, true);
        let actions = schema_actions(&schema);
        assert!(actions.contains(&"tap".to_string()));
        assert!(!actions.contains(&"click".to_string()));
    }

    #[test]
    fn test_ollama_schema_no_element_when_zero() {
        // Nothing to address, so the element actions are not offered at all.
        let schema = build_ollama_schema(0, true);
        let actions = schema_actions(&schema);
        assert!(!actions.contains(&"tap".to_string()));
        assert!(!actions.contains(&"long_press".to_string()));
    }

    #[test]
    fn test_allow_wait_false_drops_wait_from_schema() {
        let schema = build_ollama_schema(3, false);
        assert!(
            !schema_actions(&schema).contains(&"wait".to_string()),
            "wait must be absent when the caller disallows it"
        );
    }

    #[test]
    fn test_judge_body_omits_response_format_for_gemini() {
        let body = build_judge_body(&LlmProvider::Gemini, "gemini-3.1-flash-lite", "task?");
        assert!(body.as_object().unwrap().get("response_format").is_none());
    }

    #[test]
    fn test_judge_body_omits_response_format_for_anthropic() {
        let body = build_judge_body(&LlmProvider::Anthropic, "claude", "task?");
        assert!(body.as_object().unwrap().get("response_format").is_none());
    }

    #[test]
    fn test_judge_body_uses_json_object_for_openai_compat() {
        let body = build_judge_body(&LlmProvider::OpenAi, "gpt-4o-mini", "task?");
        assert_eq!(body["response_format"]["type"], "json_object");
    }

    #[test]
    fn test_judge_body_uses_json_schema_for_ollama() {
        let body = build_judge_body(&LlmProvider::Ollama, "qwen2.5vl:7b", "task?");
        assert_eq!(body["response_format"]["type"], "json_schema");
        assert_eq!(
            body["response_format"]["json_schema"]["name"],
            "goal_verdict"
        );
    }

    #[test]
    fn test_allow_wait_true_keeps_wait_in_schema() {
        assert!(schema_actions(&build_ollama_schema(3, true)).contains(&"wait".to_string()));
    }

    /// The defect this shape exists to prevent: a flat schema let the model emit
    /// open_app with no name, the executor rejected it, and the run burned every
    /// remaining step re-proposing the same thing.
    #[test]
    fn every_action_branch_requires_the_parameters_its_executor_reads() {
        let schema = build_ollama_schema(50, true);
        for (action, needed) in [
            ("open_app", "name"),
            ("tap", "element"),
            ("long_press", "element"),
            ("type", "text"),
            ("scroll", "direction"),
            ("draw_path", "points"),
        ] {
            let branch = schema_branch(&schema, action).expect(action);
            let required: Vec<&str> = branch["required"]
                .as_array()
                .expect("required")
                .iter()
                .filter_map(|v| v.as_str())
                .collect();
            assert!(
                required.contains(&needed),
                "{action} does not require {needed}, so the model may omit it"
            );
        }
    }

    #[test]
    fn check_progress_verdict_parsing_stuck() {
        assert_eq!(ProgressVerdict::from_str("stuck"), ProgressVerdict::Stuck);
        assert_eq!(ProgressVerdict::from_str("STUCK"), ProgressVerdict::Stuck);
        assert_eq!(
            ProgressVerdict::from_str("goal_visible"),
            ProgressVerdict::GoalVisible
        );
        assert_eq!(
            ProgressVerdict::from_str("progress"),
            ProgressVerdict::MakingProgress
        );
        assert_eq!(
            ProgressVerdict::from_str("making_progress"),
            ProgressVerdict::MakingProgress
        );
        // Unrecognized verdicts default to Stuck — safer to add a "try
        // different approach" hint than to silently mask a stall.
        assert_eq!(ProgressVerdict::from_str("garbage"), ProgressVerdict::Stuck);
    }
}
