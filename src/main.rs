use clap::{Parser, Subcommand};

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

        /// Cloud provider for remote devices (browserstack, saucelabs).
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

        /// Cloud provider (browserstack, saucelabs)
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
fn find_mcp_configs(home: &std::path::Path) -> Vec<std::path::PathBuf> {
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
fn remove_drengr_from_mcp_config(path: &std::path::Path) -> anyhow::Result<()> {
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
fn check_arch_or_die() {
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
fn check_arch_or_die() {}

/// Initialize tracing with DRENGR_LOG_LEVEL support.
/// In release builds, file paths and line numbers are stripped from log output.
fn init_logging() {
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
async fn check_for_update() -> Result<Option<String>, String> {
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
async fn print_update_notice() {
    if let Ok(Some(latest)) = check_for_update().await {
        let current = env!("CARGO_PKG_VERSION");
        eprintln!(
            "\n  ⬆ Update available: {} → {}\n  Run: drengr update",
            current, latest
        );
    }
}

/// Perform the actual update.
async fn do_update() -> anyhow::Result<()> {
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
fn other_drengr_pids(pgrep_output: &str, my_pid: u32) -> Vec<u32> {
    pgrep_output
        .lines()
        .filter_map(|l| l.trim().parse::<u32>().ok())
        .filter(|&pid| pid > 1 && pid != my_pid)
        .collect()
}

/// SIGTERM every OTHER `drengr` process (never self, never pid ≤ 1). Returns
/// the count signalled. Shared by `restart` and `uninstall` (the latter kills
/// siblings before /forget so a heartbeat can't re-register the machine).
fn kill_other_drengr_processes() -> usize {
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

const SUITE_CANDIDATES: [&str; 3] = ["drengr-tests.yml", "drengr-tests.yaml", ".drengr/tests.yml"];

fn find_suite_file() -> Option<std::path::PathBuf> {
    SUITE_CANDIDATES
        .iter()
        .map(std::path::PathBuf::from)
        .find(|p| p.is_file())
}

/// Escape a workflow-command message. Task text and LLM reasoning are free-form —
/// unescaped they would break the annotation or inject a `::command::` of their own.
fn gha_data(s: &str) -> String {
    s.replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

/// Escape a workflow-command property value (stricter: `:` and `,` delimit properties).
fn gha_prop(s: &str) -> String {
    gha_data(s).replace(':', "%3A").replace(',', "%2C")
}

/// Publish exact suite counts to `$GITHUB_OUTPUT` so the action never has to parse stdout.
fn write_step_outputs(result: &drengr_hands::runner::SuiteResult) {
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

async fn handle_restart() -> anyhow::Result<()> {
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
async fn handle_diag(action: DiagAction) -> anyhow::Result<()> {
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
async fn resolve_udid(udid: Option<String>) -> anyhow::Result<String> {
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

async fn detect_device_or_exit() -> drengr_hands::transport::DetectedDevice {
    match drengr_hands::transport::detect::auto_select_device().await {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(2);
        }
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `xcrun xcodebuild -version` → ("[✓]", "Xcode 16.0 (16A242d)") or ("[✗]", hint).
async fn check_xcode_installed() -> (&'static str, String) {
    let fut = tokio::process::Command::new("xcrun")
        .args(["xcodebuild", "-version"])
        .output();
    let out = match tokio::time::timeout(std::time::Duration::from_secs(5), fut).await {
        Ok(Ok(o)) if o.status.success() => o,
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            return (
                "[✗]",
                "not installed — install Xcode, then sudo xcode-select -s /Applications/Xcode.app"
                    .to_string(),
            );
        }
        _ => {
            return (
                "[✗]",
                "xcrun xcodebuild -version failed (Xcode CLT only?) — install full Xcode"
                    .to_string(),
            );
        }
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut version = String::new();
    let mut build = String::new();
    for line in text.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("Xcode ") {
            version = rest.to_string();
        } else if let Some(rest) = t.strip_prefix("Build version ") {
            build = rest.to_string();
        }
    }
    let label = match (version.is_empty(), build.is_empty()) {
        (false, false) => format!("Xcode {} ({})", version, build),
        (false, true) => format!("Xcode {}", version),
        _ => "installed".to_string(),
    };
    ("[✓]", label)
}

/// `xcrun simctl list -j runtimes` → ("[✓]", "iOS 18.5, iOS 17.5") or ("[✗]", hint).
async fn check_ios_sim_runtimes() -> (&'static str, String) {
    let fut = tokio::process::Command::new("xcrun")
        .args(["simctl", "list", "-j", "runtimes"])
        .output();
    let out = match tokio::time::timeout(std::time::Duration::from_secs(5), fut).await {
        Ok(Ok(o)) if o.status.success() => o,
        _ => {
            return (
                "[✗]",
                "not installed — run xcodebuild -downloadPlatform iOS".to_string(),
            )
        }
    };
    let json: serde_json::Value = match serde_json::from_slice(&out.stdout) {
        Ok(v) => v,
        Err(_) => {
            return (
                "[✗]",
                "simctl list runtimes returned malformed JSON".to_string(),
            )
        }
    };
    let mut versions: Vec<String> = Vec::new();
    if let Some(arr) = json.get("runtimes").and_then(|v| v.as_array()) {
        for r in arr {
            let id = r.get("identifier").and_then(|v| v.as_str()).unwrap_or("");
            let available = r
                .get("isAvailable")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if available && id.contains(".iOS-") {
                if let Some(v) = r.get("version").and_then(|v| v.as_str()) {
                    versions.push(format!("iOS {}", v));
                }
            }
        }
    }
    if versions.is_empty() {
        (
            "[✗]",
            "not installed — run xcodebuild -downloadPlatform iOS".to_string(),
        )
    } else {
        versions.sort();
        versions.dedup();
        ("[✓]", versions.join(", "))
    }
}

/// `xcrun simctl list -j devices` → ("[✓]", "N available (X booted)") or ("[–]", hint).
async fn check_ios_sim_devices() -> (&'static str, String) {
    let fut = tokio::process::Command::new("xcrun")
        .args(["simctl", "list", "-j", "devices", "available"])
        .output();
    let out = match tokio::time::timeout(std::time::Duration::from_secs(5), fut).await {
        Ok(Ok(o)) if o.status.success() => o,
        _ => {
            return (
                "[–]",
                "none configured — see: xcrun simctl create -h".to_string(),
            )
        }
    };
    let json: serde_json::Value = match serde_json::from_slice(&out.stdout) {
        Ok(v) => v,
        Err(_) => {
            return (
                "[–]",
                "simctl list devices returned malformed JSON".to_string(),
            )
        }
    };
    let mut total = 0usize;
    let mut booted = 0usize;
    if let Some(map) = json.get("devices").and_then(|v| v.as_object()) {
        for (runtime, list) in map {
            if !runtime.contains(".iOS-") {
                continue;
            }
            if let Some(arr) = list.as_array() {
                for d in arr {
                    let avail = d
                        .get("isAvailable")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true);
                    if !avail {
                        continue;
                    }
                    total += 1;
                    if d.get("state").and_then(|s| s.as_str()) == Some("Booted") {
                        booted += 1;
                    }
                }
            }
        }
    }
    if total == 0 {
        (
            "[–]",
            "none configured — see: xcrun simctl create -h".to_string(),
        )
    } else {
        ("[✓]", format!("{} available ({} booted)", total, booted))
    }
}

/// Probe whether the runner port is free. Drops the listener before return.
fn check_port_8200() -> (&'static str, &'static str) {
    match std::net::TcpListener::bind(("127.0.0.1", drengr_hands::driver::RUNNER_DEFAULT_PORT)) {
        Ok(_) => ("[✓]", "free"),
        Err(_) => ("[–]", "in use (another runner may be running)"),
    }
}

/// Walk `~/.drengr/runner/builds/` for any cached `*-Runner.app`.
/// Best-effort: cache-key freshness is verified at use time, not here.
fn check_ios_runner_cache() -> (bool, String) {
    let builds = match drengr_hands::paths::drengr_dir() {
        Some(d) => d.join("runner").join("builds"),
        None => return (false, "no home directory".to_string()),
    };
    if !builds.exists() {
        return (
            false,
            "not cached — provisions automatically on first use".to_string(),
        );
    }
    let entries = match std::fs::read_dir(&builds) {
        Ok(e) => e,
        Err(_) => {
            return (
                false,
                "not cached — provisions automatically on first use".to_string(),
            )
        }
    };
    for entry in entries.flatten() {
        let key_dir = entry.path();
        if !key_dir.is_dir() {
            continue;
        }
        if let Ok(inner) = std::fs::read_dir(&key_dir) {
            for sub in inner.flatten() {
                let name = sub.file_name();
                let s = name.to_string_lossy();
                if s.ends_with("-Runner.app") {
                    return (true, format!("cached {}", sub.path().display()));
                }
            }
        }
    }
    (
        false,
        "not cached — provisions automatically on first use".to_string(),
    )
}

/// Sum the total size of all files under `path`, in megabytes.
fn fs_dir_size_mb(path: &std::path::Path) -> Option<f64> {
    let mut total: u64 = 0;
    let mut stack = vec![path.to_path_buf()];
    while let Some(p) = stack.pop() {
        let entries = match std::fs::read_dir(&p) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                total += meta.len();
            }
        }
    }
    Some(total as f64 / 1_048_576.0)
}

/// `drengr doctor --clean-runner-cache` — wipe `~/.drengr/runner/builds/`.
async fn run_clean_runner_cache(yes: bool) -> anyhow::Result<()> {
    let builds = drengr_hands::paths::drengr_dir()
        .ok_or_else(|| anyhow::anyhow!("no home directory"))?
        .join("runner")
        .join("builds");
    if !builds.exists() {
        println!(
            "Runner cache does not exist ({}); nothing to clean.",
            builds.display()
        );
        return Ok(());
    }
    let mut to_delete: Vec<std::path::PathBuf> = Vec::new();
    let mut total_bytes_mb = 0.0f64;
    for entry in std::fs::read_dir(&builds)?.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if let Some(mb) = fs_dir_size_mb(&path) {
            total_bytes_mb += mb;
        }
        to_delete.push(path);
    }
    if to_delete.is_empty() {
        println!("No cached runner builds found.");
        return Ok(());
    }
    println!(
        "\n  Runner cache cleanup ({:.1} MB to reclaim)\n",
        total_bytes_mb
    );
    for p in &to_delete {
        println!("    {}", p.display());
    }
    println!();
    if !yes {
        use std::io::Write;
        print!("  Delete these directories? [y/N] ");
        std::io::stdout().flush().ok();
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !matches!(input.trim().to_lowercase().as_str(), "y" | "yes") {
            println!("  Cancelled.");
            return Ok(());
        }
    }
    for p in &to_delete {
        if let Err(e) = std::fs::remove_dir_all(p) {
            eprintln!("  ✗ failed to delete {}: {}", p.display(), e);
        }
    }
    println!("\n  ✓ Reclaimed ~{:.1} MB\n", total_bytes_mb);
    Ok(())
}

/// `drengr doctor --runner-status` — one-line machine-friendly summary.
async fn run_runner_status() -> anyhow::Result<()> {
    use drengr_hands::driver::{RUNNER_DEFAULT_PORT, RUNNER_VERSION};

    let (cached, _) = check_ios_runner_cache();
    let cache_glyph = if cached { "cached" } else { "missing" };

    let port_free = std::net::TcpListener::bind(("127.0.0.1", RUNNER_DEFAULT_PORT)).is_ok();
    let port_glyph = if port_free { "free" } else { "in-use" };

    let booted_count =
        tokio::process::Command::new(drengr_hands::transport::simctl::resolve_xcrun())
            .args(["simctl", "list", "devices", "booted"])
            .output()
            .await
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .matches("(Booted)")
                    .count()
            })
            .unwrap_or(0);

    println!(
        "runner {} {} • port {} {} • {} sim{} booted",
        RUNNER_VERSION,
        cache_glyph,
        RUNNER_DEFAULT_PORT,
        port_glyph,
        booted_count,
        if booted_count == 1 { "" } else { "s" },
    );
    Ok(())
}

async fn cmd_demo(app: Option<String>, task: Option<String>) -> anyhow::Result<()> {
    // Keep the demo output clean — suppress info logs unless the user opts in.
    if std::env::var("DRENGR_LOG_LEVEL").is_err() {
        std::env::set_var("DRENGR_LOG_LEVEL", "warn");
    }
    init_logging();

    eprintln!();
    eprintln!("  🐉  Drengr demo — an AI agent is about to use an app by itself.");
    eprintln!();

    // 1) Model — the agent's brain. Standalone needs one (cloud key or
    // Ollama). If none is configured, walk the user through setup.
    let llm = match drengr_hands::ooda::LlmClient::from_env() {
        Ok(c) => c,
        Err(_) => match drengr_hands::onboard::model::ensure_model_interactive(true).await {
            Some(c) => c,
            None => return Ok(()),
        },
    };

    // 2) Device — use whatever's connected, else boot one ourselves.
    fn print_device_help() {
        eprintln!("  Couldn't find or boot a device. Start one, then run `drengr demo` again:");
        eprintln!();
        eprintln!(
            "    • iOS (macOS):  xcrun simctl boot \"iPhone 15\"   (or open a simulator in Xcode)"
        );
        eprintln!(
            "    • Android:      launch an emulator in Android Studio, or `emulator -avd <name>`"
        );
        eprintln!();
    }
    let detected = match drengr_hands::transport::detect::auto_select_device().await {
        Ok(d) => d,
        Err(_) => {
            eprintln!("  No device detected — booting one (first boot can take a minute)…");
            let booted = if cfg!(target_os = "macos") {
                match drengr_hands::transport::boot::boot_ios_simulator(None).await {
                    Ok(b) => Ok(b),
                    Err(_) => drengr_hands::transport::boot::boot_android(None, true).await,
                }
            } else {
                drengr_hands::transport::boot::boot_android(None, true).await
            };
            if booted.is_err() {
                print_device_help();
                return Ok(());
            }
            match drengr_hands::transport::detect::auto_select_device().await {
                Ok(d) => d,
                Err(_) => {
                    print_device_help();
                    return Ok(());
                }
            }
        }
    };
    let is_ios = detected.os == drengr_hands::transport::DeviceOs::Ios;

    // 3) Canned, visual task per platform (unless overridden).
    let (app, task) = match (app, task) {
        (Some(a), Some(t)) => (a, t),
        _ if is_ios => (
            "com.apple.Preferences".to_string(),
            "Turn on Airplane Mode".to_string(),
        ),
        _ => (
            "com.android.settings".to_string(),
            "Open the Network & internet settings".to_string(),
        ),
    };

    eprintln!(
        "  Device:  {} ({} {})",
        detected.id, detected.model, detected.os
    );
    eprintln!("  Model:   {:?} / {}", llm.provider(), llm.model());
    eprintln!("  Task:    \"{}\"   (app: {})", task, app);
    eprintln!();

    // 4) Warm the local model so the first step isn't a cold-start stall.
    if *llm.provider() == drengr_hands::ooda::LlmProvider::Ollama {
        eprintln!("  Warming up the local model (one-time)…");
        let _ = llm.warmup().await;
    }

    let transport = drengr_hands::transport::create_transport(&detected);

    eprintln!("  Watch it work — it sees the screen, decides, and taps:");
    eprintln!();

    let config = drengr_hands::ooda::OodaConfig {
        task: task.clone(),
        app_package: app.clone(),
        max_steps: 15,
        device_id: "local".to_string(),
        force_vision: false,
        verify_completion: true,
        allowed_apps: None,
    };

    let outcome = drengr_hands::ooda::run_ooda(transport.as_ref(), &llm, &config).await;

    eprintln!();
    match outcome {
        Ok(r) if r.success => {
            eprintln!(
                "  ✅  Done in {} steps — by sight alone. No element IDs, no script.",
                r.steps
            );
            eprintln!();
            eprintln!("  That's Drengr. Next:");
            if find_mcp_configs(&dirs::home_dir().unwrap_or_default()).is_empty() {
                eprintln!("    • Give your AI client these hands:");
                eprintln!("        Claude Code:    claude mcp add drengr -- drengr mcp");
                eprintln!("        Claude Desktop: drengr setup --client claude-desktop --write");
            } else {
                eprintln!("    • Your MCP client is already wired — just ask it to drive an app.");
            }
            eprintln!("    • Run your own task:  drengr run --app <pkg> --task \"…\"");
        }
        Ok(r) => {
            eprintln!(
                "  The agent ran {} steps but didn't confirm completion:",
                r.steps
            );
            eprintln!("    {}", r.final_reasoning);
            eprintln!(
                "  (Small local models wobble on longer tasks — a frontier model is steadier.)"
            );
        }
        Err(e) => {
            eprintln!("  The demo hit an error: {}", e);
        }
    }
    Ok(())
}

async fn cmd_test(
    file: Option<String>,
    app_file: Option<String>,
    fmt: String,
    output: Option<String>,
) -> anyhow::Result<()> {
    init_logging();

    let path = file.map(std::path::PathBuf::from).unwrap_or_else(|| {
        find_suite_file().unwrap_or_else(|| {
            eprintln!(
                "No test file found. Create drengr-tests.yml, or pass a path: drengr test <file>"
            );
            std::process::exit(2);
        })
    });

    let suite = drengr_hands::runner::load_suite(&path)
        .map_err(|e| {
            eprintln!("Error: {:#}", e);
            std::process::exit(2);
        })
        .unwrap();

    let llm = drengr_hands::ooda::LlmClient::from_env()
        .map_err(|e| {
            eprintln!("Error: {:#}", e);
            std::process::exit(2);
        })
        .unwrap();

    let detected = detect_device_or_exit().await;

    let transport = drengr_hands::transport::create_transport(&detected);

    if let Some(build) = app_file.as_deref() {
        if !std::path::Path::new(build).exists() {
            eprintln!("Error: no build at {}", build);
            std::process::exit(2);
        }
        eprintln!("   Installing {}...", build);
        if let Err(e) = transport.install_app(build).await {
            eprintln!("Error: could not install {}: {:#}", build, e);
            std::process::exit(2);
        }
    }

    let in_actions = std::env::var_os("GITHUB_ACTIONS").is_some();
    if in_actions {
        let n = suite.tasks.len();
        println!(
            "::group::Drengr — {} ({} task{} on {})",
            gha_data(&suite.app),
            n,
            if n == 1 { "" } else { "s" },
            gha_data(&detected.id)
        );
    }

    let network = drengr_hands::network::sink::NetworkSink::new();
    let result =
        drengr_hands::runner::run_suite(&suite, transport.as_ref(), &llm, &detected.id, &network)
            .await;

    let formatted = match fmt.as_str() {
        "junit" => drengr_hands::runner::format_junit(&result),
        "json" => drengr_hands::runner::format_json(&result),
        _ => drengr_hands::runner::format_human(&result),
    };

    if let Some(path) = output {
        std::fs::write(&path, &formatted)?;
        eprintln!("Results written to {}", path);
    } else {
        print!("{}", formatted);
    }

    if in_actions {
        for task in result.results.iter().filter(|t| !t.passed) {
            println!(
                "::error title={}::{} — {}",
                gha_prop(&task.name),
                gha_data(&task.task),
                gha_data(&task.reasoning)
            );
        }
        println!("::endgroup::");
        write_step_outputs(&result);
    }

    std::process::exit(result.exit_code());
}

async fn cmd_setup(client: String, write: bool, port: u16) -> anyhow::Result<()> {
    let adb_bin = drengr_hands::transport::adb::resolve_adb();
    let has_adb = drengr_hands::transport::adb::run_adb(&adb_bin, None, &["version"], 5)
        .await
        .is_ok();

    let xcrun_bin = drengr_hands::transport::simctl::resolve_xcrun();
    let has_simctl = tokio::process::Command::new(&xcrun_bin)
        .args(["simctl", "list", "devices"])
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false);

    if !has_adb && !has_simctl {
        println!("  No device tools found.\n");
        println!("  For Android:  brew install android-platform-tools");
        println!("                or set ANDROID_HOME / DRENGR_ADB_PATH\n");
        if cfg!(target_os = "macos") {
            println!("  For iOS:      xcode-select --install\n");
        }
        println!("  After installing, run:  drengr setup");
        return Ok(());
    }

    // The resolved SDK root is injected into stdio configs by mcp::clients.
    let android_home = if has_adb {
        drengr_hands::transport::android_sdk::sdk_root_env()
    } else {
        None
    };

    let resolved_client = if client.is_empty() {
        use std::io::IsTerminal;
        if std::io::stdin().is_terminal() {
            println!("  Drengr Setup");
            println!("  ─────────────────────────────────────────────\n");
            println!("  Which MCP client are you configuring?\n");
            println!("    1) Claude Desktop");
            println!("    2) Claude Code (CLI)");
            println!("    3) Cursor");
            println!("    4) Windsurf");
            println!("    5) VS Code (with MCP extension)");
            println!("    6) Android Studio (Gemini agent mode)");
            println!("    7) Antigravity");
            println!("    8) Xcode (Coding Assistant, 26.3+)");
            println!("    9) Other (show raw config)\n");
            eprint!("  Enter number [1-9]: ");
            let mut input = String::new();
            std::io::stdin().read_line(&mut input).unwrap_or(0);
            let trimmed = input.trim();
            match trimmed {
                "1" => "claude-desktop".to_string(),
                "2" => "claude-code".to_string(),
                "3" => "cursor".to_string(),
                "4" => "windsurf".to_string(),
                "5" => "vscode".to_string(),
                "6" => "android-studio".to_string(),
                "7" => "antigravity".to_string(),
                "8" => "xcode".to_string(),
                "9" => "raw".to_string(),
                _ => {
                    if !trimmed.is_empty() {
                        println!("  Invalid choice: \"{}\". Showing raw config.\n", trimmed);
                    }
                    "raw".to_string()
                }
            }
        } else {
            // Non-interactive: print raw JSON for piping
            "raw".to_string()
        }
    } else {
        client.clone()
    };

    let home_dir = match dirs::home_dir() {
        Some(p) => p.to_string_lossy().to_string(),
        None => {
            if write {
                eprintln!("  Cannot determine home directory. Use --client without --write.");
                std::process::exit(1);
            }
            "~".to_string()
        }
    };

    // Single source of truth: resolve the client from the mcp::clients
    // matrix (transport, path, config JSON, notes all live there).
    let home = std::path::PathBuf::from(&home_dir);
    let catalog = drengr_hands::mcp::clients::all(&home, port);
    let platform = if has_adb && has_simctl {
        "Android + iOS"
    } else if has_adb {
        "Android"
    } else {
        "iOS"
    };

    #[allow(clippy::type_complexity)]
    let (name, json, target, note, cli): (
        String,
        String,
        Option<std::path::PathBuf>,
        Option<String>,
        Option<Vec<String>>,
    ) = match catalog.iter().find(|c| c.key == resolved_client) {
        Some(c) => (
            c.name.to_string(),
            c.config_json(android_home.as_deref()),
            c.path.clone(),
            c.note.clone(),
            c.cli_fallback.clone(),
        ),
        // "raw" / unknown → a generic stdio snippet, print-only.
        None => {
            let generic = catalog.iter().find(|c| c.key == "claude-desktop").unwrap();
            (
                if resolved_client == "raw" {
                    "MCP".to_string()
                } else {
                    resolved_client.clone()
                },
                generic.config_json(android_home.as_deref()),
                None,
                Some("your MCP client's config file".to_string()),
                None,
            )
        }
    };

    // Write mode: only when we know a writable file for this host.
    if write {
        if let Some(path) = &target {
            // Atomic file-merge first; CLI fallback on failure (Claude Code).
            let ok = drengr_hands::mcp::clients::write_merge(path, &json).is_ok()
                || cli
                    .as_ref()
                    .map(|c| drengr_hands::mcp::clients::run_cli_fallback(c))
                    .unwrap_or(false);
            if ok {
                println!("\n  ✓ Config written to {}\n", path.display());
                if let Some(n) = &note {
                    println!("  {}\n", n);
                }
                println!("  Restart {} and you're ready to go.", name);
                println!("  Verify any time:  drengr doctor");
                return Ok(());
            }
            println!(
                "  Couldn't write {}'s config — add it manually below.",
                name
            );
        } else {
            println!(
                "  Can't write {}'s config for you — add it manually below.",
                name
            );
        }
    }

    // Print mode. For writable hosts the location is the path; for
    // print-only hosts the note carries the where-to-put-it hint.
    let location = target
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| {
            note.clone()
                .unwrap_or_else(|| "your MCP client config".to_string())
        });
    println!("\n  Drengr Setup — {}", name);
    println!("  ─────────────────────────────────────────────");
    println!("  Platform: {}\n", platform);
    println!("  Add this to {}:\n", location);
    println!("{}\n", json);
    // A writable host's note is supplementary (e.g. the --http reminder);
    // a print-only host's note already served as the location above.
    if target.is_some() {
        if let Some(n) = &note {
            println!("  {}\n", n);
        }
    }
    println!("  → Restart {}", name);
    println!("  → Ask it: \"use drengr to take a screenshot of the device\"\n");
    println!("  Verify any time:  drengr doctor");
    Ok(())
}

async fn cmd_doctor(
    clean_runner_cache: bool,
    runner_status: bool,
    yes: bool,
) -> anyhow::Result<()> {
    if clean_runner_cache {
        return run_clean_runner_cache(yes).await;
    }
    if runner_status {
        return run_runner_status().await;
    }

    println!("\n  Drengr Doctor");
    println!("  ─────────────────────────────────────────────\n");

    let adb_bin = drengr_hands::transport::adb::resolve_adb();
    let adb_output = drengr_hands::transport::adb::run_adb(&adb_bin, None, &["version"], 5)
        .await
        .ok();
    let adb_ok = adb_output.is_some();
    let adb_version = if adb_ok {
        adb_output
            .and_then(|o| String::from_utf8(o).ok())
            .and_then(|s| {
                s.lines()
                    .next()
                    .and_then(|l| l.split_whitespace().last().map(|v| v.to_string()))
            })
            .unwrap_or_else(|| "unknown".to_string())
    } else {
        String::new()
    };

    if adb_ok {
        println!("  [✓] ADB                    {} ({})", adb_bin, adb_version);
    } else {
        println!("  [✗] ADB                    not found");
    }

    let xcrun_bin = drengr_hands::transport::simctl::resolve_xcrun();
    let simctl_ok = tokio::process::Command::new(&xcrun_bin)
        .args(["simctl", "list", "devices"])
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false);

    if simctl_ok {
        println!("  [✓] simctl (iOS)           available ({})", xcrun_bin);
    } else {
        println!("  [–] simctl (iOS)           not found");
    }

    if cfg!(target_os = "macos") {
        let (xc_glyph, xc_line) = check_xcode_installed().await;
        println!("  {} Xcode                  {}", xc_glyph, xc_line);

        let (rt_glyph, rt_line) = check_ios_sim_runtimes().await;
        println!("  {} iOS Simulator runtime  {}", rt_glyph, rt_line);

        let (sd_glyph, sd_line) = check_ios_sim_devices().await;
        println!("  {} iOS sim devices        {}", sd_glyph, sd_line);

        let (runner_ok, runner_line) = check_ios_runner_cache();
        let glyph = if runner_ok { "[✓]" } else { "[–]" };
        println!("  {} iOS runner             {}", glyph, runner_line);

        let (p_glyph, p_line) = check_port_8200();
        println!("  {} Port 8200              {}", p_glyph, p_line);
    }

    let devices = drengr_hands::transport::detect::detect_devices().await;
    if devices.is_empty() {
        if !adb_ok && !simctl_ok {
            println!("  [–] Devices                no transport available");
        } else {
            println!("  [✗] Devices                none connected");
        }
    } else {
        println!("  [✓] Devices                {} connected", devices.len());
        for d in &devices {
            let os_label = match d.os {
                drengr_hands::transport::DeviceOs::Android => "Android",
                drengr_hands::transport::DeviceOs::Ios => "iOS",
            };
            // Omit version when unknown (avoids a trailing "(iOS )").
            let os = match d.sdk_version.as_deref().filter(|s| !s.is_empty()) {
                Some(v) => format!("{os_label} {v}"),
                None => os_label.to_string(),
            };
            println!("       {} — {} ({})", d.id, d.model, os);
        }
    }

    // Resolve exactly the way `drengr run`/`test` does, so doctor can never
    // disagree with the real path about whether a key was found.
    let has_key = match drengr_hands::ooda::LlmClient::from_env() {
        Ok(llm) => {
            println!(
                "  [✓] Vision API key         configured — {} · {}",
                llm.provider().as_str(),
                llm.model()
            );
            println!("      → endpoint: {}", llm.base_url());
            true
        }
        Err(_) => {
            println!("  [–] Vision API key         not set — needed only for `drengr run`");
            println!("      → no key needed to drive a device: drengr look / do / query");
            false
        }
    };

    // Cloud device providers — the user's own BrowserStack/Sauce account.
    let cloud_checks: &[(&str, &str, Option<&str>)] = &[
        (
            "BrowserStack",
            "BROWSERSTACK_USERNAME",
            Some("BROWSERSTACK_ACCESS_KEY"),
        ),
        ("Sauce Labs", "SAUCE_USERNAME", Some("SAUCE_ACCESS_KEY")),
        ("AWS Device Farm", "AWS_DEVICE_FARM_ARN", None),
        (
            "LambdaTest",
            "LAMBDATEST_USERNAME",
            Some("LAMBDATEST_ACCESS_KEY"),
        ),
        ("Perfecto", "PERFECTO_SECURITY_TOKEN", None),
        ("Kobiton", "KOBITON_USERNAME", Some("KOBITON_API_KEY")),
        ("Custom Hub", "APPIUM_HUB_URL", None),
    ];
    let mut any_cloud = false;
    for (name, var1, var2) in cloud_checks {
        let ok = std::env::var(var1).is_ok() && var2.is_none_or(|v| std::env::var(v).is_ok());
        if ok {
            println!("  [✓] Cloud: {:<16} configured", name);
            any_cloud = true;
        }
    }
    if !any_cloud {
        println!("  [–] Cloud devices          not configured (set BROWSERSTACK_* or SAUCE_*)");
    }

    println!("\n  ─────────────────────────────────────────────");

    if !adb_ok && !simctl_ok {
        // BLOCKED
        println!("  Status: BLOCKED — no device transport found\n");
        println!("  ── Android ──────────────────────────────────");
        println!("  ADB not detected. Fix with any of these:\n");
        println!("    1. Install via Homebrew:");
        println!("       brew install android-platform-tools\n");
        println!("    2. Set ANDROID_HOME if SDK is already installed:");
        println!("       export ANDROID_HOME=$HOME/Library/Android/sdk\n");
        println!("    3. Set the exact path:");
        println!("       export DRENGR_ADB_PATH=/path/to/adb\n");
        println!("  ── iOS (macOS only) ─────────────────────────");
        println!("  xcrun/simctl not detected. Fix with:\n");
        println!("    1. Install Xcode Command Line Tools:");
        println!("       xcode-select --install\n");
        println!("    2. Or set the exact path:");
        println!("       export DRENGR_XCRUN_PATH=/path/to/xcrun\n");
        println!("  ─────────────────────────────────────────────");
        println!("  After fixing:");
        println!("    1. Run:  drengr doctor");
        println!("    2. Restart your MCP client (Claude Desktop, Cursor, etc.)");
        std::process::exit(1);
    } else if devices.is_empty() {
        // WAITING
        println!("  Status: WAITING — no device connected\n");
        if adb_ok {
            println!("  Android: connect a device (USB debugging on) or start an emulator.");
        } else {
            println!("  Android: ADB not found — set ANDROID_HOME or DRENGR_ADB_PATH,");
            println!("           or: brew install android-platform-tools");
        }
        if simctl_ok {
            println!("  iOS:     open a Simulator from Xcode.");
        } else if cfg!(target_os = "macos") {
            println!("  iOS:     xcrun not found — run: xcode-select --install");
        }
        println!("\n  → Then run:  drengr doctor");
        std::process::exit(1);
    } else {
        // READY or ALL SYSTEMS GO
        if has_key {
            println!("  Status: ALL SYSTEMS GO\n");
            println!("  → drengr setup         configure your MCP client");
            println!("  → drengr mcp           start MCP server");
            println!("  → drengr run           autonomous agent mode");
        } else {
            println!("  Status: READY for MCP mode\n");
            println!("  → Next:  drengr setup");
        }
    }

    // Next step for users who haven't wired an MCP client yet.
    if find_mcp_configs(&dirs::home_dir().unwrap_or_default()).is_empty() {
        println!("\n  Next: drengr setup --client claude-desktop --write   (or `drengr demo` to see it work)");
    }

    print_update_notice().await;

    println!();
    Ok(())
}

async fn cmd_uninstall() -> anyhow::Result<()> {
    let exe = std::env::current_exe().unwrap_or_default();
    let exe_str = exe.to_string_lossy().to_string();
    let home = dirs::home_dir().unwrap_or_default();
    let data_dir = drengr_hands::paths::drengr_dir_or(".");

    println!("\n  Drengr Uninstall");
    println!("  ─────────────────────────────────────────────\n");

    let is_npm = exe_str.contains("node_modules") || exe_str.contains("npm");

    println!("  The following will be removed:\n");
    println!("  Binary:     {}", exe_str);
    if data_dir.exists() {
        println!("  Data:       ~/.drengr/  (logs, sessions, screen maps, machine id)");
    }
    println!("  Keychain:   saved LLM provider keys");

    let mcp_configs = find_mcp_configs(&home);
    if !mcp_configs.is_empty() {
        println!("  MCP config: drengr entry will be removed from:");
        for path in &mcp_configs {
            println!("              {}", path.display());
        }
    }

    if is_npm {
        println!("\n  Note: Binary was installed via npm.");
        println!("  After this cleanup, also run: npm uninstall -g drengr");
    }

    println!("\n  Continue? [y/N] ");
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;

    if !input.trim().eq_ignore_ascii_case("y") {
        println!("  Cancelled.");
        return Ok(());
    }

    // Stop sibling drengr processes first so nothing holds a device lock.
    let stopped = kill_other_drengr_processes();
    if stopped > 0 {
        println!("  ✓ Stopped {} running drengr process(es)", stopped);
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }

    drengr_hands::key_store::KeyStore::purge_keychain();
    println!("  ✓ Removed keychain entries");

    // 3. Remove drengr from MCP configs
    for path in &mcp_configs {
        match remove_drengr_from_mcp_config(path) {
            Ok(()) => println!("  ✓ Cleaned MCP config: {}", path.display()),
            Err(e) => eprintln!("  ✗ Could not clean {}: {}", path.display(), e),
        }
    }

    // 4. Remove ~/.drengr data directory
    if data_dir.exists() {
        match std::fs::remove_dir_all(&data_dir) {
            Ok(()) => println!("  ✓ Removed ~/.drengr/"),
            Err(e) => eprintln!("  ✗ Could not remove ~/.drengr/: {}", e),
        }
    }

    // 5. Remove binary (last — this is the running executable)
    if !is_npm {
        let path = exe.as_path();
        if std::fs::remove_file(path).is_ok() {
            println!("  ✓ Removed {}", exe_str);
        } else {
            let status = std::process::Command::new("sudo")
                .args(["rm", "-f", &exe_str])
                .status();
            match status {
                Ok(s) if s.success() => println!("  ✓ Removed {}", exe_str),
                _ => {
                    eprintln!("  ✗ Could not remove {}. Try: sudo rm {}", exe_str, exe_str);
                }
            }
        }
    }

    println!("\n  ─────────────────────────────────────────────");
    println!("  Local uninstall complete.");
    if is_npm {
        println!("  Run `npm uninstall -g drengr` to remove the npm wrapper.");
    }
    println!("  Project-local MCP configs (.mcp.json / .cursor/mcp.json / .vscode/mcp.json)");
    println!("  are not scanned — remove drengr entries there manually.");
    println!("  Restart your MCP client to apply config changes.");
    println!();

    Ok(())
}

async fn cmd_key(action: Option<KeyAction>) -> anyhow::Result<()> {
    use drengr_hands::key_store::{self, KeyStore};

    match action {
        None => {
            // drengr key — show status for all providers
            println!("\n  Drengr LLM Keys");
            println!("  ─────────────────────────────────────────────\n");
            println!("  These are for standalone mode (drengr run).\n");

            let store = KeyStore::load();
            let providers = [
                "openai",
                "gemini",
                "anthropic",
                "groq",
                "together",
                "fireworks",
                "ollama",
            ];

            for p in providers {
                let env_key = format!("{}_API_KEY", p.to_uppercase());
                let from_env = std::env::var(&env_key)
                    .ok()
                    .or_else(|| std::env::var("DRENGR_API_KEY").ok());

                if let Some(ref k) = from_env {
                    println!("  [✓] {:<12} {} (from env)", p, key_store::mask_key(k));
                } else if let Some(k) = store.get(p) {
                    let src = if store.from_legacy_file() {
                        "from ~/.drengr/llm_keys.json (deprecated)"
                    } else {
                        "from OS keychain"
                    };
                    println!("  [✓] {:<12} {} ({})", p, key_store::mask_key(k), src);
                } else if p == "ollama" {
                    println!("  [✓] {:<12} local (no key needed)", p);
                } else {
                    println!("  [–] {:<12} not configured", p);
                }
            }
            println!();
            Ok(())
        }

        Some(KeyAction::Set { provider, api_key }) => {
            let provider =
                key_store::validate_provider(&provider).map_err(|e| anyhow::anyhow!(e))?;

            let mut store = KeyStore::load();
            store.set(&provider, &api_key);
            store
                .save()
                .map_err(|e| anyhow::anyhow!("Failed to save: {}", e))?;
            println!(
                "\n  ✓ {} key saved ({})\n",
                provider,
                key_store::mask_key(&api_key)
            );
            Ok(())
        }

        Some(KeyAction::List) => {
            let store = KeyStore::load();
            if store.is_empty() {
                println!("\n  No LLM keys stored. Use: drengr key set <provider> <key>\n");
            } else {
                let header = if store.from_legacy_file() {
                    "Stored LLM Keys (~/.drengr/llm_keys.json — deprecated)"
                } else {
                    "Stored LLM Keys (OS keychain)"
                };
                println!("\n  {}", header);
                println!("  ─────────────────────────────────────────────\n");
                for (provider, masked) in store.list() {
                    println!("  {:<12} {}", provider, masked);
                }
                println!();
            }
            Ok(())
        }

        Some(KeyAction::Remove { provider }) => {
            let provider =
                key_store::validate_provider(&provider).map_err(|e| anyhow::anyhow!(e))?;

            let mut store = KeyStore::load();
            if store.remove(&provider) {
                store
                    .save()
                    .map_err(|e| anyhow::anyhow!("Failed to save: {}", e))?;
                println!("\n  ✓ {} key removed\n", provider);
            } else {
                println!("\n  No key stored for {}\n", provider);
            }
            Ok(())
        }
    }
}

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
