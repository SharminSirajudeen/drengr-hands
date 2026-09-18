use clap::{Parser, Subcommand};

mod cli;
use cli::commands::*;

#[derive(Parser)]
#[command(
    name = "drengr",
    version,
    about = "Eyes and hands for AI agents on mobile devices",
    long_about = "Eyes and hands for AI agents on mobile devices (Android + iOS).\n\n\
        Drive a device straight from your shell — no MCP, no restart, no API key:\n  \
        drengr look                  see the screen (numbered elements + screenshot)\n  \
        drengr do tap --element 5    act: tap / type / swipe / key\n  \
        drengr query setup --headless boot/attach a device (starts an emulator/sim if none)\n  \
        drengr query devices         list connected devices\n\n\
        You are the brain; Drengr is the actuation layer. For a persistent\n\
        connection, run `drengr mcp` and drive it from your MCP client."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
// `Do` carries every action's parameters, so it dwarfs the other variants. This
// enum is parsed once at startup and dropped; the size gap costs nothing, and
// boxing it would fight clap's derive for no gain.
#[allow(clippy::large_enum_variant)]
enum Commands {
    /// Start MCP server on stdio (primary mode)
    #[command(visible_alias = "start")]
    Mcp {
        /// Target device ID (e.g. emulator-5554, iPhone-16). Auto-detects if omitted.
        #[arg(long)]
        device: Option<String>,

        /// Default observation format: text (~300 tokens) or image (screenshot).
        #[arg(long, default_value = "text")]
        format: String,

        /// Path to adb binary (auto-detected from PATH/ANDROID_HOME if omitted).
        #[arg(long)]
        adb_path: Option<String>,

        /// Cloud provider, or any Appium hub URL: browserstack | saucelabs |
        /// aws | lambdatest | perfecto | kobiton | custom | http://your-grid:4723
        #[arg(long)]
        cloud: Option<String>,

        /// Cloud device name (used with --cloud).
        #[arg(long)]
        cloud_device: Option<String>,

        /// Cloud OS version (used with --cloud).
        #[arg(long)]
        cloud_os: Option<String>,

        /// Serve over streamable HTTP instead of stdio — for MCP clients that
        /// can't spawn local servers (e.g. Android Studio). Endpoint: /mcp.
        #[arg(long)]
        http: bool,

        /// Port for --http (binds 127.0.0.1 only).
        #[arg(long, default_value = "7878")]
        port: u16,
    },

    /// Guided setup: detect your environment, wire Drengr into your AI tools,
    /// and watch it work — all in one flow. The best first command to run.
    Onboard,

    /// List connected devices
    Devices,

    /// Zero-setup demo: an AI agent drives a system app on a connected device.
    Demo {
        /// Override the app to drive (default: the platform's Settings app).
        #[arg(long)]
        app: Option<String>,

        /// Override the task (default: a simple, visual canned task).
        #[arg(long)]
        task: Option<String>,
    },

    /// Run a task using the built-in OODA agent
    Run {
        /// App package name (e.g. com.example.app)
        #[arg(long)]
        app: String,

        /// Task to perform (natural language)
        #[arg(long)]
        task: String,

        /// Maximum OODA steps (default 30, env: DRENGR_MAX_STEPS)
        #[arg(long, default_value = "30")]
        max_steps: usize,

        /// Output format
        #[arg(long, default_value = "human")]
        format: String,

        /// Output file path
        #[arg(long)]
        output: Option<String>,

        /// Cloud provider, or any Appium hub URL: browserstack | saucelabs |
        /// aws | lambdatest | perfecto | kobiton | custom | http://your-grid:4723
        #[arg(long)]
        cloud: Option<String>,

        /// Cloud device name
        #[arg(long)]
        device: Option<String>,

        /// Cloud OS version
        #[arg(long)]
        os_version: Option<String>,

        /// Pre-load the local model (Ollama) to avoid the first-inference cold-start.
        #[arg(long)]
        warmup: bool,

        /// Send a screenshot every step instead of only when the text scene is underlabeled.
        #[arg(long)]
        force_vision: bool,

        /// Skip the post-action goal-completion judge (one extra LLM call per step).
        #[arg(long)]
        no_verify_completion: bool,

        /// Wipe app data and force-stop before the run (true cold start).
        /// Equivalent to `adb shell pm clear <pkg>` + `am force-stop <pkg>`, but
        /// transport-agnostic (works on iOS simulators via `simctl uninstall`).
        #[arg(long)]
        reset: bool,

        /// After the run finishes, uninstall the WDA runner from the simulator
        /// so the next run starts from a fresh install. iOS-only; no-op on Android.
        #[arg(long)]
        cleanup_wda: bool,
    },

    /// Perceive the screen: screenshot + numbered elements. No MCP, no key —
    /// the calling agent is the brain. The frame is saved to `~/.drengr/cli/`
    /// so you can read it as a file instead of pulling base64 through context.
    Look {
        /// Output format: clean | text | grid | json (default: annotated image + elements)
        #[arg(long)]
        format: Option<String>,

        /// Target device id (default: auto-detected)
        #[arg(long)]
        device: Option<String>,

        /// iOS only: bundle id of the app in the foreground. Needed when
        /// something other than Drengr launched it (Xcode, Flutter, by hand) —
        /// without it the element tree is SpringBoard's, not your app's.
        #[arg(long, visible_alias = "bundle")]
        app: Option<String>,
    },

    /// Act on the screen. No MCP, no key. `drengr query capabilities` lists every
    /// action with its required parameters and platform support.
    Do {
        /// tap | type | clear_and_type | swipe | scroll | long_press | draw_path | key |
        /// back | home | wait | launch | terminate_app | install | uninstall | open_url |
        /// deep_link | spotlight_search | set_location | clear_location | set_appearance |
        /// set_orientation | grant_permission | simulate_biometric | pasteboard_set |
        /// pasteboard_get | unlock | alert_accept | alert_dismiss | alert_text | app_state
        action: String,

        /// Tap a numbered element from the last `look`
        #[arg(long)]
        element: Option<u64>,

        /// Normalized x (0.0–1.0) for a framework-blind coordinate tap/swipe
        #[arg(long)]
        x: Option<f64>,

        /// Normalized y (0.0–1.0)
        #[arg(long)]
        y: Option<f64>,

        /// Swipe end x (0.0–1.0)
        #[arg(long)]
        x2: Option<f64>,

        /// Swipe end y (0.0–1.0)
        #[arg(long)]
        y2: Option<f64>,

        /// Text to type
        #[arg(long)]
        text: Option<String>,

        /// Hold time in ms for long_press (default 1000)
        #[arg(long)]
        duration_ms: Option<u64>,

        /// Post-action observation format: image | clean | text
        #[arg(long)]
        format: Option<String>,

        /// Swipe/scroll direction: up | down | left | right
        #[arg(long)]
        direction: Option<String>,

        /// Find and tap an element by its visible text
        #[arg(long)]
        element_text: Option<String>,

        /// Key for action=key (e.g. back, home, enter)
        #[arg(long)]
        key: Option<String>,

        /// App package for action=launch/install (e.g. com.example.app)
        #[arg(long, visible_alias = "app")]
        package: Option<String>,

        /// Wait condition for action=wait: 'stable' | 'element:TEXT' | 'network:idle'
        #[arg(long)]
        until: Option<String>,

        /// Max wait in seconds for --until (default 5)
        #[arg(long)]
        timeout: Option<u64>,

        /// Auto-scroll to find the element before tapping
        #[arg(long)]
        scroll_to_find: bool,

        /// Max scroll attempts with --scroll-to-find (default 12)
        #[arg(long)]
        max_scroll: Option<u64>,

        /// Path to an APK / .app bundle for action=install
        #[arg(long)]
        apk: Option<String>,

        /// URL for action=open_url (http/https) or action=deep_link (app scheme)
        #[arg(long)]
        url: Option<String>,

        /// Latitude for action=set_location
        #[arg(long)]
        lat: Option<f64>,

        /// Longitude for action=set_location
        #[arg(long)]
        lng: Option<f64>,

        /// Dark mode for action=set_appearance (omit for light)
        #[arg(long)]
        dark: bool,

        /// Successful match for action=simulate_biometric (omit for a failed match)
        #[arg(long)]
        matches: bool,

        /// Service for action=grant_permission (location, photos, camera, microphone, contacts, all)
        #[arg(long)]
        permission: Option<String>,

        /// Target orientation for action=set_orientation
        #[arg(long)]
        orientation: Option<String>,

        /// Target device id (default: auto-detected)
        #[arg(long)]
        device: Option<String>,
    },

    /// Ask Drengr: setup | devices | activity | crash | connect. No MCP, no key.
    Query {
        /// setup | devices | activity | crash | connect
        question: String,

        /// Auto-boot a device if none is running (for `setup`)
        #[arg(long)]
        headless: bool,

        /// Target device id (default: auto-detected)
        #[arg(long)]
        device: Option<String>,
    },

    /// Run tests from a YAML file. Emits GitHub annotations and step outputs under Actions.
    #[command(visible_alias = "ci")]
    Test {
        /// Path to drengr-tests.yml (auto-detected from CWD if omitted)
        file: Option<String>,

        /// Build to install before the suite runs (.apk on Android, .app on iOS).
        /// A fresh emulator has nothing installed, so CI must supply the build
        /// under test or every task fails on a missing app.
        #[arg(long = "app-file")]
        app_file: Option<String>,

        /// Output format (human, json, junit)
        #[arg(long, default_value = "human")]
        format: String,

        /// Write results to this file instead of stdout
        #[arg(long)]
        output: Option<String>,
    },

    /// Explore app screens (familiarization run)
    Explore {
        /// App package name
        #[arg(long)]
        app: String,

        /// Maximum screens to discover (default 20)
        #[arg(long, default_value = "20")]
        max_screens: usize,
    },

    /// Generate MCP config for your client
    Setup {
        /// MCP client: claude-desktop, claude-code, cursor, windsurf, vscode, android-studio, antigravity, xcode
        #[arg(long, default_value = "")]
        client: String,

        /// Write config directly to the client's config file
        #[arg(long)]
        write: bool,

        /// Port for HTTP-based clients (android-studio) — must match the port
        /// you pass to `drengr mcp --http --port <p>`.
        #[arg(long, default_value = "7878")]
        port: u16,
    },

    /// Start SDK event server only
    SdkServer,

    /// Check system health. Without flags, runs every check.
    Doctor {
        /// Delete all cached drengr-runner builds.
        #[arg(long)]
        clean_runner_cache: bool,

        /// Print a one-line runner status summary (cached, port free, sim booted).
        #[arg(long)]
        runner_status: bool,

        /// Skip interactive confirmation prompts (for scripts + CI).
        #[arg(long)]
        yes: bool,
    },

    /// Pre-build the iOS runner .app required for MCP mode. Run once after
    /// install (and after iOS / Xcode upgrades). Must be run from an interactive
    /// terminal — xcodebuild needs a TTY.
    BuildRunner {
        /// Target simulator UDID. If omitted, auto-picks a booted iOS sim.
        #[arg(long)]
        udid: Option<String>,
    },

    /// Update drengr to the latest version (--check reports without upgrading)
    Update {
        /// Report whether a newer version exists, without upgrading in place.
        #[arg(long)]
        check: bool,
    },

    /// Uninstall drengr from this machine
    Uninstall,

    /// Manage LLM provider API keys for standalone mode (drengr run)
    Key {
        #[command(subcommand)]
        action: Option<KeyAction>,
    },

    /// Manage anonymized diagnostic bundles saved on stuck/crash runs
    Diag {
        #[command(subcommand)]
        action: DiagAction,
    },

    /// Terminate any other drengr processes on this machine. Run this from a
    /// separate terminal — your MCP client (Claude Desktop, Cursor, Windsurf)
    /// will spawn a fresh server on its next request.
    Restart,
}

#[derive(Subcommand)]
enum DiagAction {
    /// List bundles saved at ~/.drengr/diagnostics
    List,
    /// Print a bundle's contents to stdout (already redacted on disk)
    Show {
        /// Run id from `drengr diag list`
        run_id: String,
    },
    /// Delete a bundle from disk without sharing
    Forget {
        /// Run id from `drengr diag list`
        run_id: String,
    },
}

#[derive(Subcommand)]
enum KeyAction {
    /// Save an API key for an LLM provider
    Set {
        /// Provider: openai, gemini, anthropic, groq, together, fireworks, ollama
        provider: String,
        /// The API key value
        #[arg(allow_hyphen_values = true)]
        api_key: String,
    },
    /// List all configured LLM providers (keys are masked)
    List,
    /// Remove a stored key for a provider
    Remove {
        /// Provider name to remove
        provider: String,
    },
}

/// Find MCP client config files that contain a "drengr" server entry.
pub(crate) fn find_mcp_configs(home: &std::path::Path) -> Vec<std::path::PathBuf> {
    let candidates = vec![
        // Claude Desktop
        if cfg!(target_os = "macos") {
            home.join("Library/Application Support/Claude/claude_desktop_config.json")
        } else {
            home.join(".config/claude/claude_desktop_config.json")
        },
        // Claude Code (global)
        home.join(".claude.json"),
        // Cursor (global)
        home.join(".cursor/mcp.json"),
        // Windsurf
        home.join(".codeium/windsurf/mcp_config.json"),
    ];

    candidates
        .into_iter()
        .filter(|p| {
            if let Ok(content) = std::fs::read_to_string(p) {
                content.contains("\"drengr\"")
            } else {
                false
            }
        })
        .collect()
}

/// Remove the "drengr" entry from an MCP client config JSON file.
pub(crate) fn remove_drengr_from_mcp_config(path: &std::path::Path) -> anyhow::Result<()> {
    let content = std::fs::read_to_string(path)?;
    let mut json: serde_json::Value = serde_json::from_str(&content)?;

    // MCP configs store servers under "mcpServers" (Claude Desktop) or root level
    let removed = if let Some(servers) = json.pointer_mut("/mcpServers") {
        if let Some(obj) = servers.as_object_mut() {
            obj.remove("drengr").is_some()
        } else {
            false
        }
    } else {
        false
    };

    if removed {
        let pretty = serde_json::to_string_pretty(&json)?;
        std::fs::write(path, pretty + "\n")?;
    }

    Ok(())
}

/// On Apple Silicon, refuse to run as an x86_64 binary. Rosetta translation
/// propagates into spawned xcodebuild/simctl children and silently breaks
/// iOS Simulator destination resolution. Bail loudly at startup.
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
pub(crate) fn check_arch_or_die() {
    if let Ok(out) = std::process::Command::new("sysctl")
        .args(["-n", "hw.optional.arm64"])
        .output()
    {
        if out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "1" {
            eprintln!("drengr: this binary is x86_64 but the host is Apple Silicon.");
            eprintln!("Rosetta propagates to xcodebuild/simctl children and breaks");
            eprintln!("iOS Simulator destination resolution. Rebuild with arm64:");
            eprintln!("  PATH=\"$HOME/.cargo/bin:$PATH\" cargo build --release");
            eprintln!("or install rustup if missing: https://rustup.rs");
            std::process::exit(2);
        }
    }
}
#[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
pub(crate) fn check_arch_or_die() {}

/// Initialize tracing with DRENGR_LOG_LEVEL support.
/// In release builds, file paths and line numbers are stripped from log output.
pub(crate) fn init_logging() {
    let level = std::env::var("DRENGR_LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
    let directive = format!("drengr={}", level);
    let filter = tracing_subscriber::EnvFilter::from_default_env()
        .add_directive(directive.parse().unwrap_or_default());

    #[cfg(debug_assertions)]
    {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }
    #[cfg(not(debug_assertions))]
    {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_file(false)
            .with_line_number(false)
            .init();
    }
}

/// Check npm registry for a newer version. Returns Some(latest) if update available.
pub(crate) async fn check_for_update() -> Result<Option<String>, String> {
    let current = env!("CARGO_PKG_VERSION");
    let resp = drengr_hands::http::client()
        .get("https://registry.npmjs.org/drengr/latest")
        .timeout(std::time::Duration::from_secs(3))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let latest = json
        .get("version")
        .and_then(|v| v.as_str())
        .ok_or("registry returned no version")?;
    Ok((latest != current).then(|| latest.to_string()))
}

/// Print update notice if a newer version is available (non-blocking, best-effort).
pub(crate) async fn print_update_notice() {
    if let Ok(Some(latest)) = check_for_update().await {
        let current = env!("CARGO_PKG_VERSION");
        eprintln!(
            "\n  ⬆ Update available: {} → {}\n  Run: drengr update",
            current, latest
        );
    }
}

/// Perform the actual update.
pub(crate) async fn do_update() -> anyhow::Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    eprintln!("  Current version: {}", current);
    eprintln!("  Checking for updates...\n");

    match check_for_update().await {
        Err(e) => anyhow::bail!(
            "could not reach the npm registry, so the installed version is unchecked: {e}"
        ),
        Ok(Some(latest)) => {
            eprintln!("  New version available: {}\n", latest);

            let exe = std::env::current_exe().unwrap_or_default();
            let exe_str = exe.to_string_lossy().to_string();
            let is_npm = exe_str.contains("node_modules") || exe_str.contains("npm");

            if is_npm {
                eprintln!("  Updating via npm...");
                let status = tokio::process::Command::new("npm")
                    .args(["install", "-g", "drengr@latest"])
                    .status()
                    .await?;
                if status.success() {
                    eprintln!("  ✓ Updated to {}", latest);
                } else {
                    eprintln!("  ✗ npm update failed. Try: sudo npm install -g drengr@latest");
                }
            } else {
                // Binary install — re-run install script
                eprintln!("  Updating via install script...");
                let status = tokio::process::Command::new("sh")
                    .args(["-c", "curl -fsSL https://drengr.dev/install.sh | bash"])
                    .status()
                    .await?;
                if status.success() {
                    eprintln!("  ✓ Updated to {}", latest);
                } else {
                    eprintln!("  ✗ Update failed. Try manually:");
                    eprintln!("    curl -fsSL https://drengr.dev/install.sh | bash");
                }
            }
        }
        Ok(None) => {
            eprintln!("  ✓ Already on the latest version ({})", current);
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    check_arch_or_die();
    drengr_hands::credentials::migration::migrate_to_keychain_if_needed();
    let cli = Cli::parse();

    match cli.command {
        Commands::Mcp {
            device,
            format,
            adb_path,
            cloud,
            cloud_device,
            cloud_os,
            http,
            port,
        } => {
            let config = drengr_hands::mcp::McpConfig {
                device,
                format,
                cloud,
                cloud_device,
                cloud_os,
                adb_path,
                xcrun_path: None,
                http_port: http.then_some(port),
            };
            drengr_hands::mcp::run_server_with_config(config).await
        }

        // Single-shot eyes-and-hands over plain shell: no MCP server to
        // register, no host restart, no LLM key. Same dispatch path as MCP.
        Commands::Look {
            format,
            device,
            app,
        } => {
            init_logging();
            if let Some(b) = app.as_deref().filter(|b| !b.is_empty()) {
                drengr_hands::transport::simctl::SimctlTransport::remember_bundle(b);
            }
            let mut a = serde_json::Map::new();
            if let Some(f) = format {
                a.insert("format".into(), f.into());
            }
            if let Some(d) = device {
                a.insert("device".into(), d.into());
            }
            let code =
                drengr_hands::mcp::cli::run_tool("drengr_look", serde_json::Value::Object(a), true)
                    .await;
            std::process::exit(code);
        }

        Commands::Do {
            action,
            element,
            x,
            y,
            x2,
            y2,
            text,
            duration_ms,
            format,
            direction,
            element_text,
            key,
            package,
            until,
            timeout,
            scroll_to_find,
            max_scroll,
            apk,
            url,
            lat,
            lng,
            dark,
            matches,
            permission,
            orientation,
            device,
        } => {
            init_logging();
            let mut a = serde_json::Map::new();
            a.insert("action".into(), action.clone().into());
            if let Some(v) = element {
                a.insert("element".into(), v.into());
            }
            if let Some(v) = x {
                a.insert("x".into(), v.into());
            }
            if let Some(v) = y {
                a.insert("y".into(), v.into());
            }
            if let Some(v) = x2 {
                a.insert("x2".into(), v.into());
            }
            if let Some(v) = y2 {
                a.insert("y2".into(), v.into());
            }
            if let Some(v) = text {
                a.insert("text".into(), v.into());
            }
            if let Some(v) = duration_ms {
                a.insert("duration_ms".into(), v.into());
            }
            if let Some(v) = format {
                a.insert("format".into(), v.into());
            }
            if let Some(v) = direction {
                a.insert("direction".into(), v.into());
            }
            if let Some(v) = element_text {
                a.insert("element_text".into(), v.into());
            }
            if let Some(v) = key {
                a.insert("keycode".into(), v.into());
            }
            if let Some(v) = package {
                a.insert("package".into(), v.into());
            }
            if let Some(v) = until {
                a.insert("until".into(), v.into());
            }
            if let Some(v) = timeout {
                a.insert("timeout".into(), v.into());
            }
            if scroll_to_find {
                a.insert("scroll_to_find".into(), true.into());
            }
            if let Some(v) = max_scroll {
                a.insert("max_scroll".into(), v.into());
            }
            if let Some(v) = apk {
                a.insert("apk".into(), v.into());
            }
            if let Some(v) = url {
                a.insert("url".into(), v.into());
            }
            if let Some(v) = lat {
                a.insert("lat".into(), v.into());
            }
            if let Some(v) = lng {
                a.insert("lng".into(), v.into());
            }
            // A flag has no "unset", so absence means light — which is exactly the
            // schema's default. Sent unconditionally or light is unrequestable.
            if action == "set_appearance" {
                a.insert("dark".into(), dark.into());
            }
            if action == "simulate_biometric" {
                a.insert("matches".into(), matches.into());
            }
            if let Some(v) = permission {
                a.insert("permission".into(), v.into());
            }
            if let Some(v) = orientation {
                a.insert("orientation".into(), v.into());
            }
            if let Some(v) = device {
                a.insert("device".into(), v.into());
            }
            let code =
                drengr_hands::mcp::cli::run_tool("drengr_do", serde_json::Value::Object(a), true)
                    .await;
            std::process::exit(code);
        }

        Commands::Query {
            question,
            headless,
            device,
        } => {
            init_logging();
            let mut a = serde_json::Map::new();
            a.insert("question".into(), question.into());
            a.insert("headless".into(), headless.into());
            if let Some(d) = device {
                a.insert("device".into(), d.into());
            }
            let code = drengr_hands::mcp::cli::run_tool(
                "drengr_query",
                serde_json::Value::Object(a),
                false,
            )
            .await;
            std::process::exit(code);
        }

        Commands::Onboard => {
            init_logging();
            drengr_hands::onboard::run().await
        }

        Commands::Devices => {
            init_logging();

            let devices = drengr_hands::transport::detect::detect_devices().await;
            if devices.is_empty() {
                println!(
                    "No devices found. Connect one, or let Drengr boot an emulator/sim for you:"
                );
                println!("  drengr query setup --headless");
                std::process::exit(2);
            }

            println!("Connected devices:");
            for d in &devices {
                println!(
                    "  {} — {} ({}){}",
                    d.id,
                    d.model,
                    d.os,
                    d.sdk_version
                        .as_deref()
                        .map(|v| format!(" SDK {}", v))
                        .unwrap_or_default()
                );
            }
            Ok(())
        }

        Commands::Demo { app, task } => cmd_demo(app, task).await,

        Commands::Run {
            app,
            task,
            max_steps,
            format: fmt,
            output,
            cloud,
            device,
            os_version,
            warmup,
            force_vision,
            no_verify_completion,
            reset,
            cleanup_wda,
        } => {
            init_logging();

            // Allow DRENGR_MAX_STEPS env to override default (but CLI flag wins)
            let max_steps = if max_steps == 30 {
                std::env::var("DRENGR_MAX_STEPS")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(max_steps)
            } else {
                max_steps
            };

            let llm = match drengr_hands::ooda::LlmClient::from_env() {
                Ok(c) => c,
                // No model configured — walk the user through setup (interactive),
                // or print the manual paths and exit when there's no terminal.
                Err(_) => {
                    match drengr_hands::onboard::model::ensure_model_interactive(true).await {
                        Some(c) => c,
                        None => std::process::exit(2),
                    }
                }
            };

            eprintln!("drengr run — {} ({})", task, app);
            eprintln!("   {}", llm.describe());

            if warmup {
                if *llm.provider() == drengr_hands::ooda::LlmProvider::Ollama {
                    eprintln!("   Warming up local model (one-time cold start)...");
                    let warm_start = std::time::Instant::now();
                    if let Err(e) = llm.warmup().await {
                        eprintln!("   Warmup failed (continuing anyway): {}", e);
                    } else {
                        eprintln!(
                            "   Warmup complete in {:.1}s",
                            warm_start.elapsed().as_secs_f32()
                        );
                    }
                } else {
                    eprintln!(
                        "   --warmup is Ollama-only; ignoring for {:?}",
                        llm.provider()
                    );
                }
            }

            let transport: Box<dyn drengr_hands::transport::DeviceTransport> =
                if let Some(ref cloud_provider) = cloud {
                    let dev = device.as_deref().unwrap_or("default");
                    let osv = os_version.as_deref().unwrap_or("latest");
                    drengr_hands::transport::create_cloud_transport(cloud_provider, dev, osv, None)
                        .await
                        .map_err(|e| {
                            eprintln!("Cloud connect error: {}", e);
                            std::process::exit(2);
                        })
                        .unwrap()
                } else {
                    let detected = detect_device_or_exit().await;
                    eprintln!(
                        "   Device: {} ({} {})",
                        detected.id, detected.model, detected.os
                    );
                    drengr_hands::transport::create_transport(&detected)
                };

            if reset {
                eprintln!("   Resetting app (wipe data + force-stop + relaunch)...");
                if let Err(e) = transport.reset_app(&app).await {
                    eprintln!("   Reset failed ({}) — continuing with existing state", e);
                }
            }

            if drengr_hands::explore::load_screen_map(&app)
                .ok()
                .flatten()
                .is_none()
            {
                eprintln!("No screen map — running quick exploration...");
                let explore_config = drengr_hands::explore::ExploreConfig {
                    app_package: app.clone(),
                    max_screens: 10,
                    ..Default::default()
                };
                if let Ok(map) =
                    drengr_hands::explore::explore_app(transport.as_ref(), &explore_config).await
                {
                    drengr_hands::explore::save_screen_map(&map).ok();
                }
            }

            let config = drengr_hands::ooda::OodaConfig {
                task: task.clone(),
                app_package: app.clone(),
                max_steps,
                device_id: cloud.as_deref().unwrap_or("local").to_string(),
                force_vision,
                verify_completion: !no_verify_completion,
                // CLI: unrestricted — the user authored --app and --task.
                allowed_apps: None,
            };

            let run_outcome = drengr_hands::ooda::run_ooda(transport.as_ref(), &llm, &config).await;

            // Cleanup runs regardless of success so a crashed run still
            // leaves a clean simulator for the next invocation.
            if cleanup_wda {
                match transport.cleanup_runtime().await {
                    Ok(_) => eprintln!("   WDA runtime cleaned up."),
                    Err(e) => eprintln!("   WDA cleanup failed: {}", e),
                }
            }

            let result = run_outcome
                .map_err(|e| {
                    eprintln!("Internal error: {}", e);
                    std::process::exit(3);
                })
                .unwrap();

            let formatted = match fmt.as_str() {
                "json" => serde_json::to_string_pretty(&serde_json::json!({
                    "task": result.task,
                    "success": result.success,
                    "steps": result.steps,
                    "reasoning": result.final_reasoning,
                }))
                .unwrap_or_default(),
                "junit" => {
                    // Reuse the canonical emitter so `run` and `test` emit identical JUnit.
                    let suite = drengr_hands::runner::SuiteResult {
                        app: app.to_string(),
                        total: 1,
                        passed: if result.success { 1 } else { 0 },
                        failed: if result.success { 0 } else { 1 },
                        duration_ms: 0,
                        results: vec![drengr_hands::runner::TaskResult {
                            name: task.to_string(),
                            task: task.to_string(),
                            passed: result.success,
                            steps: result.steps,
                            reasoning: result.final_reasoning.to_string(),
                            duration_ms: 0,
                            network_expectations: Vec::new(),
                        }],
                    };
                    drengr_hands::runner::format_junit(&suite)
                }
                _ => {
                    if result.success {
                        format!("PASSED — {} ({} steps)", task, result.steps)
                    } else {
                        format!(
                            "FAILED — {} ({} steps): {}",
                            task, result.steps, result.final_reasoning
                        )
                    }
                }
            };

            if let Some(path) = output {
                std::fs::write(&path, &formatted)?;
                eprintln!("Results written to {}", path);
            } else {
                println!("{}", formatted);
            }

            if !result.success {
                std::process::exit(1);
            }

            Ok(())
        }

        Commands::Test {
            file,
            app_file,
            format: fmt,
            output,
        } => cmd_test(file, app_file, fmt, output).await,

        Commands::Explore { app, max_screens } => {
            init_logging();

            let detected = detect_device_or_exit().await;

            let transport = drengr_hands::transport::create_transport(&detected);

            let config = drengr_hands::explore::ExploreConfig {
                app_package: app,
                max_screens,
                ..Default::default()
            };

            let map = drengr_hands::explore::explore_app(transport.as_ref(), &config)
                .await
                .map_err(|e| {
                    eprintln!("Internal error: {}", e);
                    std::process::exit(3);
                })
                .unwrap();
            let path = drengr_hands::explore::save_screen_map(&map)
                .map_err(|e| {
                    eprintln!("Internal error: {}", e);
                    std::process::exit(3);
                })
                .unwrap();
            println!("\nScreen map saved to {}", path.display());

            Ok(())
        }

        Commands::SdkServer => {
            init_logging();

            let sink = drengr_hands::network::sink::NetworkSink::new();
            let screen_store = std::sync::Arc::new(tokio::sync::Mutex::new(
                drengr_hands::sdk::ScreenStore::new(),
            ));

            drengr_hands::sdk::start_sdk_server(sink, screen_store).await
        }

        Commands::Setup {
            client,
            write,
            port,
        } => cmd_setup(client, write, port).await,

        Commands::Doctor {
            clean_runner_cache,
            runner_status,
            yes,
        } => cmd_doctor(clean_runner_cache, runner_status, yes).await,

        Commands::BuildRunner { udid } => {
            init_logging();
            let udid = resolve_udid(udid).await?;
            println!("Pre-building Drengr Runner for sim {udid}...");
            println!("(First time takes 60-90s; subsequent runs are cached.)");
            let path = drengr_hands::driver::bootstrap::prebuild_runner(&udid)
                .await
                .map_err(|e| anyhow::anyhow!("build-runner failed: {e}"))?;
            println!("  [✓] Runner cached at: {}", path.display());
            println!("  [✓] MCP mode can now launch the runner without xcodebuild.");
            Ok(())
        }

        Commands::Update { check } => {
            if check {
                init_logging();
                let current = env!("CARGO_PKG_VERSION");
                match check_for_update().await {
                    Ok(Some(latest)) => {
                        println!("Update available: {} -> {}", current, latest);
                        println!("Run `drengr update` (or `npm i -g drengr@latest`) to upgrade in place.");
                    }
                    Ok(None) => println!("Drengr is up to date ({}).", current),
                    Err(e) => {
                        println!("Could not reach the npm registry, so {current} is unchecked: {e}")
                    }
                }
                Ok(())
            } else {
                do_update().await
            }
        }

        Commands::Uninstall => cmd_uninstall().await,

        Commands::Key { action } => cmd_key(action).await,

        Commands::Diag { action } => handle_diag(action).await,
        Commands::Restart => handle_restart().await,
    }
}

/// Parse `pgrep -x drengr` output into the PIDs we may signal: every valid pid
/// except self and the kernel/init range. Pure for testability.
pub(crate) fn other_drengr_pids(pgrep_output: &str, my_pid: u32) -> Vec<u32> {
    pgrep_output
        .lines()
        .filter_map(|l| l.trim().parse::<u32>().ok())
        .filter(|&pid| pid > 1 && pid != my_pid)
        .collect()
}

/// SIGTERM every OTHER `drengr` process (never self, never pid ≤ 1). Returns
/// the count signalled. Shared by `restart` and `uninstall` (the latter kills
/// siblings before /forget so a heartbeat can't re-register the machine).
pub(crate) fn kill_other_drengr_processes() -> usize {
    let output = match std::process::Command::new("pgrep")
        .args(["-x", "drengr"])
        .output()
    {
        Ok(o) => o,
        Err(_) => return 0,
    };
    let pids = other_drengr_pids(&String::from_utf8_lossy(&output.stdout), std::process::id());
    for pid in &pids {
        let _ = std::process::Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status();
    }
    pids.len()
}

pub(crate) const SUITE_CANDIDATES: [&str; 3] =
    ["drengr-tests.yml", "drengr-tests.yaml", ".drengr/tests.yml"];

pub(crate) fn find_suite_file() -> Option<std::path::PathBuf> {
    SUITE_CANDIDATES
        .iter()
        .map(std::path::PathBuf::from)
        .find(|p| p.is_file())
}

/// Escape a workflow-command message. Task text and LLM reasoning are free-form —
/// unescaped they would break the annotation or inject a `::command::` of their own.
pub(crate) fn gha_data(s: &str) -> String {
    s.replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

/// Escape a workflow-command property value (stricter: `:` and `,` delimit properties).
pub(crate) fn gha_prop(s: &str) -> String {
    gha_data(s).replace(':', "%3A").replace(',', "%2C")
}

/// Publish exact suite counts to `$GITHUB_OUTPUT` so the action never has to parse stdout.
pub(crate) fn write_step_outputs(result: &drengr_hands::runner::SuiteResult) {
    let Some(path) = std::env::var_os("GITHUB_OUTPUT") else {
        return;
    };
    let line = format!(
        "passed={}\nfailed={}\ntotal={}\n",
        result.passed, result.failed, result.total
    );
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        use std::io::Write;
        let _ = f.write_all(line.as_bytes());
    }
}

pub(crate) async fn handle_restart() -> anyhow::Result<()> {
    init_logging();
    let killed = kill_other_drengr_processes();

    if killed == 0 {
        eprintln!("drengr restart: no other drengr processes running");
    } else {
        eprintln!("drengr restart: sent SIGTERM to {} process(es)", killed);
        eprintln!("Your MCP client (Claude Desktop, Cursor, Windsurf) will spawn a fresh");
        eprintln!("server on its next message — restart that client if you don't see one.");
    }
    Ok(())
}

/// `drengr diag` subcommand. Bundle reads happen against the local
/// `~/.drengr/diagnostics/` directory; only `share` makes a network call,
/// and only after explicit user consent.
pub(crate) async fn handle_diag(action: DiagAction) -> anyhow::Result<()> {
    use anyhow::Context;
    match action {
        DiagAction::List => {
            let bundles = drengr_hands::diag::list_bundles()?;
            if bundles.is_empty() {
                println!(
                    "No diagnostic bundles. They are saved automatically when a run\n\
                     gets stuck or crashes."
                );
                return Ok(());
            }
            println!("Saved bundles ({}):", bundles.len());
            for path in bundles {
                let modified = std::fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let run_id = path.file_stem().and_then(|s| s.to_str()).unwrap_or("?");
                let age_s = now_unix().saturating_sub(modified);
                println!("  {}  age:{}s  path:{}", run_id, age_s, path.display());
            }
            println!("\nRead one with `drengr diag show <run_id>` — redacted on disk.");
            Ok(())
        }
        DiagAction::Show { run_id } => {
            let bundle = drengr_hands::diag::load_bundle(&run_id)?;
            println!("{}", serde_json::to_string_pretty(&bundle)?);
            Ok(())
        }
        DiagAction::Forget { run_id } => {
            let path = drengr_hands::diag::bundle_path(&run_id);
            std::fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
            println!("Removed {}", path.display());
            Ok(())
        }
    }
}

/// Resolve a target sim UDID: use `--udid` if given, else first Booted iOS sim.
pub(crate) async fn resolve_udid(udid: Option<String>) -> anyhow::Result<String> {
    if let Some(u) = udid {
        return Ok(u);
    }
    let out = tokio::process::Command::new("xcrun")
        .args(["simctl", "list", "-j", "devices", "booted"])
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("spawn xcrun simctl: {e} (is Xcode installed?)"))?;
    if !out.status.success() {
        anyhow::bail!(
            "xcrun simctl list failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let json: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| anyhow::anyhow!("parse simctl json: {e}"))?;
    let devices = json
        .get("devices")
        .and_then(|v| v.as_object())
        .ok_or_else(|| anyhow::anyhow!("simctl json missing .devices"))?;
    for (runtime, arr) in devices.iter() {
        if !runtime.contains("iOS") {
            continue;
        }
        if let Some(list) = arr.as_array() {
            for d in list {
                let booted = d.get("state").and_then(|s| s.as_str()) == Some("Booted");
                if booted {
                    if let Some(u) = d.get("udid").and_then(|u| u.as_str()) {
                        return Ok(u.to_string());
                    }
                }
            }
        }
    }
    anyhow::bail!(
        "no booted iOS simulator found. Boot one first:\n  open -a Simulator\n  # or: xcrun simctl boot <udid>"
    )
}

pub(crate) async fn detect_device_or_exit() -> drengr_hands::transport::DetectedDevice {
    match drengr_hands::transport::detect::auto_select_device().await {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(2);
        }
    }
}

pub(crate) fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `xcrun xcodebuild -version` → ("[✓]", "Xcode 16.0 (16A242d)") or ("[✗]", hint).
#[cfg(test)]
mod gha_escape_tests {
    use super::{gha_data, gha_prop};

    #[test]
    fn newlines_cannot_break_out_of_an_annotation() {
        let reasoning = "step 1 failed\nstep 2 skipped";
        assert_eq!(gha_data(reasoning), "step 1 failed%0Astep 2 skipped");
        assert!(!gha_data(reasoning).contains('\n'));
    }

    #[test]
    fn model_output_cannot_inject_a_workflow_command() {
        let hostile = "done\n::error::injected\n::set-output name=passed::99";
        let escaped = gha_data(hostile);
        // The runner only parses a command at the start of a line — collapsing every
        // newline is what makes the payload inert, not stripping the `::` itself.
        assert!(!escaped.contains('\n') && !escaped.contains('\r'));
        assert_eq!(escaped.lines().count(), 1);
    }

    #[test]
    fn percent_is_escaped_before_the_sequences_it_introduces() {
        assert_eq!(gha_data("100%\n"), "100%25%0A");
    }

    #[test]
    fn property_values_also_escape_the_delimiters() {
        assert_eq!(gha_prop("Login: a,b"), "Login%3A a%2Cb");
    }

    #[test]
    fn clean_text_is_left_alone() {
        assert_eq!(
            gha_data("Checkout flow completed"),
            "Checkout flow completed"
        );
    }
}

#[cfg(test)]
mod uninstall_tests {
    use super::other_drengr_pids;

    #[test]
    fn pid_filter_excludes_self_and_kernel() {
        // self=4242; expect to signal only 100 and 200.
        let out = "1\n0\n4242\n100\n200\nnot-a-pid\n";
        assert_eq!(other_drengr_pids(out, 4242), vec![100, 200]);
        // empty output → nothing to kill.
        assert!(other_drengr_pids("", 4242).is_empty());
    }
}

/// The self-trace guard.
///
/// `anti_debug` is a deterrent, and a deterrent that stops the product is worth
/// less than no deterrent at all. `PTRACE_TRACEME` reads like the Linux twin of
/// macOS's `PT_DENY_ATTACH` and is not: it does not detect a debugger, it makes
/// our own parent our tracer. A parent that never calls `waitpid` then leaves us
/// in `ptrace_stop` at the first signal we receive, and the first signal a device
/// actuator receives is the `SIGCHLD` from the first `adb` it spawns. The binary
/// answered `--version` and hung on every command that touched a device, on Linux
/// only, which is why a macOS host never saw it.
///
/// The property: nothing under `src/` may make this process a ptrace tracee.
#[cfg(test)]
mod self_trace_guard {
    /// Requests that install a tracer on the calling process. A `libc::ptrace`
    /// call is allowed only when its request is not one of these.
    const TRACEE_REQUESTS: &[(&str, &str)] = &[
        (
            "PTRACE_TRACEME",
            "makes our parent our tracer; we then stop on every signal",
        ),
        (
            "PTRACE_ATTACH",
            "attaches a tracer; the tracee stops until someone continues it",
        ),
        (
            "PTRACE_SEIZE",
            "same as ATTACH without the initial stop, still installs a tracer",
        ),
    ];

    #[test]
    fn nothing_may_make_this_process_a_ptrace_tracee() {
        let sources =
            drengr_hands::source_guard::rust_files_under(&drengr_hands::source_guard::src_root());

        // Self-check: the guard is worthless if it stopped reading the file it
        // polices, so prove the scan still sees anti_debug and its macOS call.
        let main = sources
            .iter()
            .find(|(name, _)| name == "main.rs")
            .map(|(_, src)| drengr_hands::source_guard::without_comments(src))
            .expect("guard must still read src/main.rs");
        assert!(
            main.contains("fn anti_debug"),
            "guard no longer sees anti_debug"
        );
        assert!(
            main.contains("libc::ptrace"),
            "guard no longer sees the macOS PT_DENY_ATTACH call"
        );

        let mut found = Vec::new();
        for (name, src) in &sources {
            let code = drengr_hands::source_guard::without_comments(src);
            // Inspect the FIRST ARGUMENT of every ptrace call, not any mention of
            // a request name — the table above names them and must not match itself.
            for (at, _) in code.match_indices("ptrace(") {
                let args_at = at + "ptrace(".len();
                let mut depth = 1usize;
                let mut end = args_at;
                for (k, c) in code[args_at..].char_indices() {
                    match c {
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                end = args_at + k;
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                let request = code[args_at..end]
                    .split(',')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .rsplit("::")
                    .next()
                    .unwrap_or("")
                    .to_string();
                let line_no = code[..at].matches('\n').count() + 1;
                if request == "0" {
                    found.push(format!(
                        "{name}:{line_no} calls ptrace with request 0 — that is PTRACE_TRACEME by number"
                    ));
                }
                for (named, why) in TRACEE_REQUESTS {
                    if request == *named {
                        found.push(format!("{name}:{line_no} calls {named} — {why}"));
                    }
                }
            }
        }

        assert!(
            found.is_empty(),
            "a ptrace request that installs a tracer on this process is present.\n{}\n\
             On Linux the deterrent is prctl(PR_SET_DUMPABLE, 0), which denies a same-user \
             attach without ever stopping us.",
            found.join("\n")
        );
    }
}
