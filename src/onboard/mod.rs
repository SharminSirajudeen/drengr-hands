//! `drengr onboard` — the guided onboarding wizard. A thin orchestrator over
//! existing primitives (device detect/boot, runner build, the OODA demo, model
//! setup, client wiring) that takes a new user from "just installed" to "wired
//! into my AI tools, having watched it work" in one flow. Terminal-only;
//! degrades to a printed transcript when there's no TTY.

pub mod model;

use std::io::IsTerminal;

use crate::mcp::clients;
use crate::ooda::LlmClient;
use crate::transport::{self, DeviceOs};

#[derive(Clone, Copy, PartialEq)]
enum Target {
    Android,
    Ios,
    Both,
}

impl Target {
    fn wants_android(self) -> bool {
        matches!(self, Target::Android | Target::Both)
    }
    fn wants_ios(self) -> bool {
        matches!(self, Target::Ios | Target::Both)
    }
}

/// Entry point for `Commands::Onboard`.
pub async fn run() -> anyhow::Result<()> {
    if std::env::var("DRENGR_LOG_LEVEL").is_err() {
        std::env::set_var("DRENGR_LOG_LEVEL", "warn");
    }

    run_inner().await;
    Ok(())
}

/// The wizard body. Returns `(outcome, platform_os)` for the funnel event:
/// outcome ∈ {"non_tty", "declined", "no_client", "complete"}.
async fn run_inner() -> (&'static str, Option<String>) {
    // Non-TTY (CI / piped): never prompt — print the manual path and exit.
    if !std::io::stdin().is_terminal() {
        print_manual_transcript();
        return ("non_tty", None);
    }

    // Idempotent front door: if Drengr is already wired into a client, offer a
    // quick menu instead of re-walking the whole wizard.
    let home = dirs::home_dir().unwrap_or_default();
    let already: Vec<&'static str> = clients::all(&home, 7878)
        .iter()
        .filter(|c| c.has_drengr())
        .map(|c| c.name)
        .collect();
    if !already.is_empty() {
        if let Some(outcome) = rerun_menu(&already).await {
            return outcome;
        }
        // "Reconfigure from scratch" → fall through to the full wizard below.
    }

    // S0 — Welcome (personal, by Sharmin).
    eprintln!();
    eprintln!("  🐉  Sharmin Sirajudeen welcomes you to Drengr.");
    eprintln!("  ─────────────────────────────────────────────");
    eprintln!();
    eprintln!("  Drengr gives AI agents eyes and hands on mobile devices.");
    eprintln!("  In about a minute, an AI will be driving a real app on your machine");
    eprintln!("  — seeing the screen, deciding, and tapping. By sight alone.");
    eprintln!();
    eprintln!("  This wizard will check your setup, wire Drengr into your AI tools,");
    eprintln!("  and let you watch it work before you go.");
    eprintln!();
    if !confirm("Ready?", true) {
        return ("declined", None);
    }

    // S1 — Platform target (the one hard question).
    let target = pick_target();

    // S2 — Environment detection + boot.
    let mut device = detect_and_boot(target).await;
    let platform = device.as_ref().map(|d| d.os.to_string());

    // S3 — iOS runner (only when an iOS sim is up).
    if target.wants_ios() {
        if let Some(d) = &device {
            if d.os == DeviceOs::Ios {
                maybe_build_runner(&d.id).await;
            }
        }
    }

    // S4 — Model brain (opt-in; MCP mode needs none).
    eprintln!();
    eprintln!("  Your AI client is the brain — for MCP mode, Drengr needs no model.");
    let mut brain: Option<LlmClient> = None;
    if confirm(
        "Set up a standalone model too (for `drengr demo` / `drengr run`)?",
        false,
    ) {
        brain = model::ensure_model_interactive(true).await;
    }

    // S5 — Wire Drengr into AI clients.
    let wired = wire_clients();

    // S6 — The aha demo.
    offer_demo(&mut device, &mut brain).await;

    // S7 — Finish.
    finish(target, &device, &brain, &wired);
    let outcome = if wired.is_empty() {
        "no_client"
    } else {
        "complete"
    };
    (outcome, platform)
}

fn print_manual_transcript() {
    eprintln!();
    eprintln!("  🐉  Drengr onboarding (non-interactive)");
    eprintln!("  ─────────────────────────────────────────────");
    eprintln!("  No terminal detected — here's the manual path:");
    eprintln!();
    eprintln!("    1. Check your setup:     drengr doctor");
    eprintln!("    2. (iOS) build runner:   drengr build-runner");
    eprintln!("    3. Wire your AI client:  drengr setup --client claude-desktop --write");
    eprintln!("    4. See it work:          drengr demo");
    eprintln!();
    eprintln!("  Run `drengr onboard` from an interactive terminal for the guided version.");
    eprintln!();
}

fn pick_target() -> Target {
    eprintln!();
    if !cfg!(target_os = "macos") {
        eprintln!("  Platform: Android  (iOS simulators need macOS + Xcode).");
        return Target::Android;
    }
    let items = [
        "Android apps   (Android emulator · via ADB)",
        "iOS apps       (iOS Simulator · needs Xcode on this Mac)",
        "Both",
    ];
    match select("What do you want Drengr to drive?", &items).unwrap_or(0) {
        1 => Target::Ios,
        2 => Target::Both,
        _ => Target::Android,
    }
}

async fn detect_and_boot(target: Target) -> Option<transport::DetectedDevice> {
    eprintln!();
    eprintln!("  Checking your environment");
    eprintln!("  ─────────────────────────────────────────────");

    if target.wants_android() {
        let adb = transport::adb::resolve_adb();
        let ok = crate::transport::adb::run_adb(&adb, None, &["version"], 5)
            .await
            .is_ok();
        line(
            ok,
            "ADB",
            if ok {
                &adb
            } else {
                "not found — brew install android-platform-tools"
            },
        );
    }
    if target.wants_ios() {
        let xcrun = transport::simctl::resolve_xcrun();
        let ok = tokio::process::Command::new(&xcrun)
            .args(["simctl", "list", "devices"])
            .output()
            .await
            .map(|o| o.status.success())
            .unwrap_or(false);
        line(
            ok,
            "Xcode / simctl",
            if ok {
                "available"
            } else {
                "not found — xcode-select --install"
            },
        );
    }

    // Already-connected device?
    if let Ok(d) = transport::detect::auto_select_device().await {
        line(
            true,
            "Device",
            &format!("{} — {} ({})", d.id, d.model, d.os),
        );
        return Some(d);
    }
    line(false, "Device", "none running");

    // Offer to boot one.
    if !confirm("Boot a simulator/emulator now? (recommended)", true) {
        eprintln!("  Skipping — you can connect a device later.");
        return None;
    }
    let spin = spinner("Booting (first boot can take a minute)…");
    let booted = if target.wants_ios() && cfg!(target_os = "macos") {
        match transport::boot::boot_ios_simulator(None).await {
            Ok(b) => Ok(b),
            Err(_) if target.wants_android() => transport::boot::boot_android(None, true).await,
            Err(e) => Err(e),
        }
    } else {
        transport::boot::boot_android(None, true).await
    };
    spin.finish_and_clear();

    if booted.is_err() {
        line(
            false,
            "Device",
            "couldn't boot — start one manually, then re-run",
        );
        return None;
    }
    match transport::detect::auto_select_device().await {
        Ok(d) => {
            line(
                true,
                "Device",
                &format!("{} — {} ({})", d.id, d.model, d.os),
            );
            Some(d)
        }
        Err(_) => None,
    }
}

async fn maybe_build_runner(udid: &str) {
    eprintln!();
    eprintln!("  iOS needs a small on-device runner — it's how Drengr taps inside the");
    eprintln!("  simulator. Provisioned once per iOS version + Xcode, reused across all");
    eprintln!("  simulators; a new iOS version provisions itself on first use.");
    if !confirm(
        "Provision it now for this simulator? (makes the first run instant)",
        true,
    ) {
        eprintln!("  [–] Deferred — Drengr provisions it automatically on first iOS use.");
        return;
    }
    let spin = spinner("Provisioning the iOS runner…");
    let result = crate::driver::bootstrap::prebuild_runner(udid).await;
    spin.finish_and_clear();
    match result {
        Ok(p) => line(true, "iOS runner", &format!("ready · {}", p.display())),
        Err(e) => {
            line(
                false,
                "iOS runner",
                "couldn't provision — auto-retries on first use",
            );
            eprintln!("      ({e})");
        }
    }
}

fn wire_clients() -> Vec<String> {
    eprintln!();
    eprintln!("  Wire Drengr into your AI tools");
    eprintln!("  ─────────────────────────────────────────────");
    let home = dirs::home_dir().unwrap_or_default();
    let android_home = crate::transport::android_sdk::sdk_root_env();
    let catalog = clients::all(&home, 7878);

    let labels: Vec<String> = catalog
        .iter()
        .map(|c| {
            let badge = match (c.writable(), &c.wire) {
                (_, clients::Wire::Http(_)) => "HTTP",
                (true, _) => "writable",
                (false, _) => "copy snippet",
            };
            let found = if c.installed() {
                ""
            } else {
                "  (not detected)"
            };
            format!("{:<16} [{}]{}", c.name, badge, found)
        })
        .collect();
    let preselect: Vec<bool> = catalog.iter().map(|c| c.installed()).collect();

    let chosen = multiselect(
        "Pick all you want to use Drengr from (space toggles):",
        &labels,
        &preselect,
    );
    if chosen.is_empty() {
        eprintln!("  None selected — wire one later with `drengr setup`.");
        return Vec::new();
    }

    let mut wired = Vec::new();
    let mut needs_http = false;
    eprintln!();
    for i in chosen {
        let c = &catalog[i];
        let json = c.config_json(android_home.as_deref());
        if let clients::Wire::Http(_) = c.wire {
            needs_http = true;
        }
        match &c.path {
            Some(path) => {
                // Atomic file-merge first; if it fails, fall back to the host's
                // own CLI (Claude Code); only then give up to copy-paste.
                let ok = clients::write_merge(path, &json).is_ok()
                    || c.cli_fallback
                        .as_ref()
                        .map(|cli| clients::run_cli_fallback(cli))
                        .unwrap_or(false);
                if ok {
                    line(true, c.name, &format!("written → {}", path.display()));
                    if let Some(note) = &c.note {
                        eprintln!("      {note}");
                    }
                    wired.push(c.name.to_string());
                } else {
                    line(false, c.name, "couldn't write — copy this:");
                    eprintln!("{json}");
                    if let Some(note) = &c.note {
                        eprintln!("      {note}");
                    }
                }
            }
            None => {
                eprintln!("  [⎘] {} — copy this into the location below:", c.name);
                eprintln!("{json}");
                if let Some(note) = &c.note {
                    eprintln!("      {note}");
                }
                wired.push(format!("{} (manual)", c.name));
            }
        }
    }
    if needs_http {
        eprintln!();
        eprintln!("  Android Studio talks to Drengr over HTTP — keep this running:");
        eprintln!("      drengr mcp --http");
    }
    wired
}

async fn offer_demo(device: &mut Option<transport::DetectedDevice>, brain: &mut Option<LlmClient>) {
    let Some(dev) = device.clone() else { return };
    eprintln!();
    if !confirm("Want to see it work right now? (~20s)", true) {
        return;
    }
    // Need a brain. Use the one set up in S4, else try env, else pivot.
    if brain.is_none() {
        if let Ok(c) = LlmClient::from_env() {
            *brain = Some(c);
        }
    }
    let Some(llm) = brain.as_ref() else {
        eprintln!();
        eprintln!("  The terminal demo needs a model, which MCP mode doesn't — so instead,");
        eprintln!("  ask your newly-wired AI client: \"use drengr to drive the Settings app.\"");
        eprintln!("  You'll watch it work right there.");
        return;
    };
    run_demo(&dev, llm).await;
}

/// Run the canned OODA demo on a device. Fires the `demo` telemetry outcome.
async fn run_demo(dev: &transport::DetectedDevice, llm: &LlmClient) {
    let is_ios = dev.os == DeviceOs::Ios;
    let (app, task) = if is_ios {
        (
            "com.apple.Preferences".to_string(),
            "Turn on Airplane Mode".to_string(),
        )
    } else {
        (
            "com.android.settings".to_string(),
            "Open the Network & internet settings".to_string(),
        )
    };
    eprintln!();
    eprintln!("  Watch it work — it sees the screen, decides, and taps:");
    eprintln!();
    let transport = transport::create_transport(dev);
    let config = crate::ooda::OodaConfig {
        task,
        app_package: app,
        max_steps: 15,
        device_id: "local".to_string(),
        force_vision: false,
        verify_completion: true,
        allowed_apps: None,
    };
    match crate::ooda::run_ooda(transport.as_ref(), llm, &config).await {
        Ok(r) if r.success => {
            eprintln!();
            eprintln!(
                "  ✅  Done in {} steps — by sight alone. No element IDs, no script.",
                r.steps
            );
        }
        Ok(r) => {
            eprintln!(
                "  The agent ran {} steps but didn't confirm completion.",
                r.steps
            );
        }
        Err(e) => {
            eprintln!("  The demo hit an error: {e}");
        }
    }
}

/// Idempotent front door: when Drengr is already wired into a client, offer a
/// quick menu instead of re-walking the whole wizard. Returns `Some(outcome)`
/// when handled here, or `None` to fall through to a full reconfigure.
async fn rerun_menu(already: &[&'static str]) -> Option<(&'static str, Option<String>)> {
    eprintln!();
    eprintln!("  🐉  Drengr is already set up.");
    eprintln!("  ─────────────────────────────────────────────");
    eprintln!("  Wired into: {}", already.join(" · "));
    eprintln!();
    let items = [
        "Run the demo",
        "Wire another AI tool",
        "Re-check my environment",
        "Reconfigure from scratch",
        "Quit",
    ];
    let t = if cfg!(target_os = "macos") {
        Target::Both
    } else {
        Target::Android
    };
    match select("What would you like to do?", &items) {
        Some(0) => {
            let device = detect_and_boot(t).await;
            let brain = LlmClient::from_env().ok();
            match (device.as_ref(), brain.as_ref()) {
                (Some(dev), Some(llm)) => run_demo(dev, llm).await,
                (Some(_), None) => {
                    eprintln!();
                    eprintln!("  The terminal demo needs a model — or just ask your wired AI");
                    eprintln!("  client: \"use drengr to drive the Settings app.\"");
                }
                (None, _) => eprintln!("  No device available — start one and try again."),
            }
            Some(("rerun_demo", device.map(|d| d.os.to_string())))
        }
        Some(1) => {
            let _ = wire_clients();
            Some(("rerun_add_tool", None))
        }
        Some(2) => {
            let device = detect_and_boot(t).await;
            eprintln!();
            eprintln!("  Re-check done. Run `drengr doctor` any time for the full report.");
            Some(("rerun_recheck", device.map(|d| d.os.to_string())))
        }
        Some(3) => None, // reconfigure → fall through to the full wizard
        _ => Some(("rerun_quit", None)),
    }
}

fn finish(
    target: Target,
    device: &Option<transport::DetectedDevice>,
    brain: &Option<LlmClient>,
    wired: &[String],
) {
    eprintln!();
    eprintln!("  ✅  You're set up.");
    eprintln!("  ─────────────────────────────────────────────");
    if !wired.is_empty() {
        eprintln!("  Wired into:  {}", wired.join(" · "));
    }
    let plat = match target {
        Target::Android => "Android",
        Target::Ios => "iOS",
        Target::Both => "Android + iOS",
    };
    eprintln!("  Target:      {plat}");
    if let Some(d) = device {
        eprintln!("  Device:      {} ({})", d.model, d.os);
    }
    if let Some(b) = brain {
        eprintln!("  Model:       {} / {}", b.provider().as_str(), b.model());
    }
    eprintln!();
    eprintln!("  Try it:");
    eprintln!("    • In your AI client:  \"use drengr to take a screenshot of the device\"");
    eprintln!("    • Your own task:      drengr run --app <pkg> --task \"…\"");
    eprintln!("    • Re-check anytime:   drengr doctor");
    eprintln!();
    eprintln!("  Drengr is MIT — github.com/SharminSirajudeen/drengr-hands");
    eprintln!();
    eprintln!("  🐉  Happy driving. — Sharmin");
    eprintln!();
}

// ── output + dialoguer helpers (caller already TTY-guarded) ──

fn line(ok: bool, label: &str, detail: &str) {
    let mark = if ok { "[✓]" } else { "[✗]" };
    eprintln!("  {mark} {label:<18} {detail}");
}

fn spinner(msg: &str) -> indicatif::ProgressBar {
    let pb = indicatif::ProgressBar::new_spinner();
    pb.set_message(format!("  {msg}"));
    pb.enable_steady_tick(std::time::Duration::from_millis(120));
    pb
}

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

fn multiselect(prompt: &str, items: &[String], defaults: &[bool]) -> Vec<usize> {
    dialoguer::MultiSelect::new()
        .with_prompt(format!("  {prompt}"))
        .items(items)
        .defaults(defaults)
        .interact_opt()
        .ok()
        .flatten()
        .unwrap_or_default()
}
