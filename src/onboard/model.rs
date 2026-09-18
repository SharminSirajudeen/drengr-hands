//! Model "brain" setup — gets the user a working *vision-capable* LLM for
//! standalone mode (`drengr demo` / `drengr run`). MCP mode never needs this:
//! the host AI client is the brain. Cheapest-to-aha first: reuse an existing
//! key, then a local Ollama model, then offer a pull, else a free cloud key.
//!
//! Guardrail: only vision-capable models are recommended/accepted (a live image
//! probe gates what we save), so a user can't footgun into a text-only model.

use std::io::IsTerminal;
use std::time::Duration;

use crate::ooda::{LlmClient, LlmProvider};

const OLLAMA_BASE: &str = "http://localhost:11434";
const VISION_MODEL: &str = "qwen2.5vl:7b";
// Ollama tags that can see images (base name before the ':' tag).
const VISION_OLLAMA_TAGS: &[&str] = &[
    "qwen2.5vl",
    "qwen2-vl",
    "llama3.2-vision",
    "llava",
    "moondream",
    "minicpm-v",
    "bakllava",
];
// 1×1 transparent PNG — the smallest valid image for the vision probe.
const PROBE_PNG_B64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

/// Vision-capable by default model. Drives what we *show/recommend*.
pub fn provider_default_is_vision(p: &LlmProvider) -> bool {
    matches!(
        p,
        LlmProvider::Gemini | LlmProvider::OpenAi | LlmProvider::Anthropic | LlmProvider::Ollama
    )
}

struct ModelEnv {
    ollama_running: bool,
    ollama_vision_tag: Option<String>,
    total_ram_gb: u64,
    capable_local_hw: bool, // macOS + Apple Silicon
}

async fn detect() -> ModelEnv {
    let (ollama_running, ollama_vision_tag) = detect_ollama().await;
    ModelEnv {
        ollama_running,
        ollama_vision_tag,
        total_ram_gb: total_ram_gb(),
        capable_local_hw: cfg!(target_os = "macos") && cfg!(target_arch = "aarch64"),
    }
}

/// GET /api/tags — is Ollama up, and is a vision model already pulled?
async fn detect_ollama() -> (bool, Option<String>) {
    let resp = crate::http::client()
        .get(format!("{OLLAMA_BASE}/api/tags"))
        .timeout(Duration::from_millis(400))
        .send()
        .await;
    let Ok(resp) = resp else { return (false, None) };
    if !resp.status().is_success() {
        return (true, None);
    }
    let body: serde_json::Value = resp.json().await.unwrap_or_default();
    let tag = body["models"].as_array().and_then(|models| {
        models.iter().find_map(|m| {
            let name = m["name"].as_str()?;
            let base = name.split(':').next().unwrap_or(name);
            VISION_OLLAMA_TAGS
                .iter()
                .any(|v| base.contains(v))
                .then(|| name.to_string())
        })
    });
    (true, tag)
}

/// Total RAM in GB, no extra crate (sysctl on macOS, /proc on Linux).
fn total_ram_gb() -> u64 {
    #[cfg(target_os = "macos")]
    {
        if let Ok(out) = std::process::Command::new("sysctl")
            .args(["-n", "hw.memsize"])
            .output()
        {
            if let Ok(bytes) = String::from_utf8_lossy(&out.stdout).trim().parse::<u64>() {
                return bytes / 1_000_000_000;
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(s) = std::fs::read_to_string("/proc/meminfo") {
            for line in s.lines() {
                if let Some(kb) = line.strip_prefix("MemTotal:") {
                    if let Ok(kb) = kb.trim().trim_end_matches(" kB").trim().parse::<u64>() {
                        return kb / 1_000_000;
                    }
                }
            }
        }
    }
    16 // unknown → assume enough, let it fail loud rather than block
}

/// Ensure a usable, vision-capable standalone model. Returns a ready client, or
/// `None` if the user skipped or no terminal is available to set one up.
/// `required` only changes the framing copy (demo/run vs the opt-in wizard step).
pub async fn ensure_model_interactive(required: bool) -> Option<LlmClient> {
    // Already have a working brain (env or saved keychain key)? Use it silently.
    if let Ok(client) = LlmClient::from_env() {
        if provider_default_is_vision(client.provider()) || std::env::var("DRENGR_MODEL").is_ok() {
            return Some(client);
        }
    }

    if !std::io::stdin().is_terminal() {
        // Non-interactive (CI / piped): never prompt — print the manual paths.
        eprintln!("  Standalone mode needs a vision model. Set one up:");
        eprintln!("    • Cloud:  drengr key set anthropic <key>   (or gemini / openai)");
        eprintln!("    • Local:  install Ollama, then `ollama pull {VISION_MODEL}`");
        return None;
    }

    if required {
        eprintln!("  To drive an app by itself, Drengr needs its own vision model");
        eprintln!("  (standalone only — your MCP client never needs this). ~30s.\n");
    }

    let env = detect().await;

    // 1. Local vision model already pulled → just use it.
    if let Some(tag) = &env.ollama_vision_tag {
        let client = LlmClient::new(LlmProvider::Ollama, "ollama".into(), Some(tag.clone()));
        eprintln!("  [✓] Using your local model: {tag}");
        warmup(&client).await;
        return Some(client);
    }

    // 2. Ollama running, nothing pulled, enough RAM → offer the pull.
    if env.ollama_running && env.total_ram_gb >= 8 {
        if confirm(
            &format!(
                "Ollama's running. Pull the local vision model {VISION_MODEL} (~6 GB, one-time)?"
            ),
            true,
        ) {
            if pull_ollama_model(VISION_MODEL).await {
                let client = LlmClient::new(
                    LlmProvider::Ollama,
                    "ollama".into(),
                    Some(VISION_MODEL.into()),
                );
                warmup(&client).await;
                return Some(client);
            }
            eprintln!("  Pull didn't finish. Falling back to a cloud key.");
        } else {
            return cloud_key_flow().await;
        }
    }

    // 3. Strong local HW, no Ollama → recommend local; offer cloud escape.
    if env.capable_local_hw && env.total_ram_gb >= 16 && !env.ollama_running {
        eprintln!("  Your Mac can run the model locally — private, free, offline.");
        eprintln!("    1. Install Ollama:  curl -fsSL https://ollama.com/install.sh | sh");
        eprintln!("    2. Re-run, and Drengr will pull the vision model for you.\n");
        if !confirm("Set up a cloud key instead for now?", true) {
            return None;
        }
        return cloud_key_flow().await;
    }

    // 4. Low RAM or everything else → cloud.
    if env.total_ram_gb < 8 {
        eprintln!("  This machine is below what the local vision model needs (~6–8 GB).");
        eprintln!("  A free cloud key is faster here.\n");
    }
    cloud_key_flow().await
}

/// Capture + validate a cloud key (Gemini recommended). Vision-probes before save.
async fn cloud_key_flow() -> Option<LlmClient> {
    let providers = [
        (
            LlmProvider::Gemini,
            "Gemini — free tier, strong vision (recommended)",
            "https://aistudio.google.com/apikey",
        ),
        (
            LlmProvider::OpenAi,
            "OpenAI — gpt-4o-mini (needs billing)",
            "https://platform.openai.com/api-keys",
        ),
        (
            LlmProvider::Anthropic,
            "Anthropic — Claude (best driver, paid)",
            "https://console.anthropic.com/settings/keys",
        ),
    ];
    let labels: Vec<&str> = providers.iter().map(|(_, l, _)| *l).collect();
    let idx = match select("Pick a cloud vision provider:", &labels) {
        Some(i) => i,
        None => return None,
    };
    let (provider, _, link) = &providers[idx];

    eprintln!("  Get a key: {link}");
    let key = match password(&format!("Paste your {} API key", provider.as_str())) {
        Some(k) if !k.trim().is_empty() => k.trim().to_string(),
        _ => return None,
    };

    let client = LlmClient::new(provider.clone(), key.clone(), None);
    eprint!("  Checking the key… ");
    if !vision_probe(&client).await {
        eprintln!("rejected.");
        eprintln!(
            "  That key didn't pass a vision check — double-check it's a {} key.",
            provider.as_str()
        );
        return None;
    }
    eprintln!("✓ valid, vision works.");

    // Persist via the existing keychain-first store.
    let mut store = crate::key_store::KeyStore::load();
    store.set(provider.as_str(), &key);
    if let Err(e) = store.save() {
        eprintln!("  (Couldn't save to keychain: {e} — set DRENGR_API_KEY to persist.)");
    } else {
        eprintln!("  Saved to your keychain.");
    }
    Some(client)
}

/// Send a real 1×1 image — confirms auth AND that the model accepts images.
/// Bounded so a network stall (captive portal, dead DNS) can't freeze the
/// wizard — every other outbound call in the crate sets a timeout too.
async fn vision_probe(client: &LlmClient) -> bool {
    let probe =
        client.complete_with_image("Reply with the single word: ok", PROBE_PNG_B64, 1, false);
    match tokio::time::timeout(Duration::from_secs(12), probe).await {
        Ok(Ok(_)) => true,
        Ok(Err(_)) => false,
        Err(_) => {
            eprintln!("\n  (timed out reaching the provider — check your connection)");
            false
        }
    }
}

/// POST /api/pull with streamed NDJSON progress, rendered as one rewriting bar.
async fn pull_ollama_model(model: &str) -> bool {
    use indicatif::{ProgressBar, ProgressStyle};
    eprintln!("  Pulling {model} (~6 GB, one-time). Subsequent runs are instant.");
    let resp = crate::http::client()
        .post(format!("{OLLAMA_BASE}/api/pull"))
        .json(&serde_json::json!({ "name": model, "stream": true }))
        .timeout(Duration::from_secs(60 * 60))
        .send()
        .await;
    let Ok(mut resp) = resp else { return false };

    let bar = ProgressBar::new(100);
    bar.set_style(
        ProgressStyle::with_template("  {bar:30} {percent:>3}%  {msg}")
            .unwrap_or_else(|_| ProgressStyle::default_bar()),
    );

    // Stream NDJSON via Response::chunk() (no `stream` feature / futures_util).
    let mut ok = false;
    let mut buf = Vec::new();
    while let Ok(Some(chunk)) = resp.chunk().await {
        buf.extend_from_slice(&chunk);
        while let Some(nl) = buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = buf.drain(..=nl).collect();
            let Ok(v) = serde_json::from_slice::<serde_json::Value>(&line) else {
                continue;
            };
            if v.get("error").is_some() {
                bar.finish_and_clear();
                return false;
            }
            if let (Some(done), Some(total)) = (v["completed"].as_u64(), v["total"].as_u64()) {
                if let Some(pct) = (done * 100).checked_div(total) {
                    bar.set_position(pct);
                    bar.set_message(format!(
                        "{:.1} / {:.1} GB",
                        done as f64 / 1e9,
                        total as f64 / 1e9
                    ));
                }
            }
            if v["status"].as_str() == Some("success") {
                ok = true;
            }
        }
    }
    bar.finish_and_clear();
    if ok {
        eprintln!("  [✓] Pulled {model}.");
    }
    ok
}

async fn warmup(client: &LlmClient) {
    eprintln!("  Warming up the model (one-time)…");
    let _ = client.warmup().await;
}

// ── thin dialoguer wrappers (all already TTY-guarded by the caller) ──

fn confirm(prompt: &str, default: bool) -> bool {
    dialoguer::Confirm::new()
        .with_prompt(format!("  {prompt}"))
        .default(default)
        .interact()
        .unwrap_or(default)
}

fn select(prompt: &str, items: &[&str]) -> Option<usize> {
    dialoguer::Select::new()
        .with_prompt(format!("  {prompt}"))
        .items(items)
        .default(0)
        .interact_opt()
        .ok()
        .flatten()
}

fn password(prompt: &str) -> Option<String> {
    dialoguer::Password::new()
        .with_prompt(format!("  {prompt}"))
        .interact()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vision_allowlist_excludes_text_providers() {
        assert!(provider_default_is_vision(&LlmProvider::Gemini));
        assert!(provider_default_is_vision(&LlmProvider::Ollama));
        assert!(!provider_default_is_vision(&LlmProvider::Groq));
        assert!(!provider_default_is_vision(&LlmProvider::Together));
        assert!(!provider_default_is_vision(&LlmProvider::Fireworks));
    }

    #[test]
    fn ram_detection_is_plausible() {
        let gb = total_ram_gb();
        assert!((1..100_000).contains(&gb), "implausible RAM: {gb} GB");
    }
}
