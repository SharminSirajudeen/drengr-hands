//! One function per subcommand that does real work. `main` stays a dispatcher.

use super::doctor::*;
use crate::KeyAction;
use crate::*;

pub(crate) async fn cmd_demo(app: Option<String>, task: Option<String>) -> anyhow::Result<()> {
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

pub(crate) async fn cmd_test(
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

pub(crate) async fn cmd_setup(client: String, write: bool, port: u16) -> anyhow::Result<()> {
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
        None => (
            if resolved_client == "raw" {
                "MCP".to_string()
            } else {
                resolved_client.clone()
            },
            drengr_hands::mcp::clients::Client::generic_stdio_json(android_home.as_deref()),
            None,
            Some("your MCP client's config file".to_string()),
            None,
        ),
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

pub(crate) async fn cmd_doctor(
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

pub(crate) async fn cmd_uninstall() -> anyhow::Result<()> {
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

pub(crate) async fn cmd_key(action: Option<KeyAction>) -> anyhow::Result<()> {
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
