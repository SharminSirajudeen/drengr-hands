pub mod actions;
pub mod capabilities;
pub mod cli;
pub mod clients;
pub mod handlers;
pub mod http;
pub mod tools;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use handlers::McpHandlers;

const SERVER_NAME: &str = "drengr";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
/// MCP protocol revisions Drengr speaks, newest first. Per the spec lifecycle,
/// `initialize` echoes the client's requested version when we support it, else
/// falls back to LATEST — so legacy hosts (2024-11-05) and modern hosts that
/// read the newer display fields (title, icons) both negotiate correctly.
const SUPPORTED_PROTOCOL_VERSIONS: [&str; 4] =
    ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const LATEST_PROTOCOL_VERSION: &str = SUPPORTED_PROTOCOL_VERSIONS[0];
/// Drengr's mark, inlined so the server icon is self-contained (no network fetch).
const ICON_SVG: &str = include_str!("../../assets/drengr_runner_icon.svg");

/// JSON-RPC 2.0 request.
#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    pub params: Option<Value>,
}

/// JSON-RPC 2.0 response.
#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
}

impl JsonRpcResponse {
    pub fn success(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Option<Value>, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
            }),
        }
    }

    pub fn method_not_found(id: Option<Value>, method: &str) -> Self {
        Self::error(id, -32601, format!("Method not found: {}", method))
    }
}

/// Route a JSON-RPC request to the appropriate handler.
pub async fn handle_request(
    request: JsonRpcRequest,
    handlers: &McpHandlers,
) -> Option<JsonRpcResponse> {
    let id = request.id.clone();

    match request.method.as_str() {
        "initialize" => {
            let instructions = "\
Drengr controls a real or simulated MOBILE DEVICE (an Android phone/emulator or iOS simulator). Its tools act ONLY on that device's screen — they do NOT read or edit source code, files, or the IDE/project. Do not treat them as code, repository, or file tools.

Drengr gives you eyes and hands on mobile devices. YOU are the brain — Drengr is the
actuation layer you drive. This MCP mode is the FULL, primary way to use Drengr and
needs NO API key of its own: you observe with drengr_look, decide, and Drengr executes
with drengr_do. Do not report Drengr as unusable or key-gated — nothing here requires a
key, because you are the intelligence.

(Drengr also ships an OPTIONAL standalone command — `drengr run` — that drives a device
autonomously using its own LLM key, for headless/CI use with no AI host connected. It is
a convenience, not the main mode; you never need it, and its absence does not limit
anything you do here.)

HOW TO USE (observe→act loop):
1. drengr_look → see numbered elements on screen
2. Decide what to do based on what you see
3. drengr_do(action, element) → execute + get situation report + new screen state
4. Repeat until task is done

drengr_do returns a fresh observation after each action automatically.
So the loop is: look once → then act repeatedly (each act shows you the result).

APP NAMES: display_name is the app's real name when name_source is 'known'. When it
is 'derived' it was inferred from the package id and can be wrong, so match on
`package`. Android carries no label on the device, so most Android names are derived.

A11Y: drengr_query(question='analyze') returns accessibility.clickable_without_label,
the count of elements a user can tap but nothing can name (no text, content-desc or
id). Drengr computes it, so it is reported on every device.

FORMATS (drengr_look and drengr_do both take `format`):
- 'image' (default): annotated frame, numbered red markers. A marker sits BESIDE
  its element so it does not cover the text, so a marker's position is not the tap
  point: the tap lands at the centre of `bounds`.
- 'clean': the same frame with nothing drawn on it. Use it to judge typography,
  spacing and layout, which markers otherwise sit on top of.
- 'text': text scene only, no image, no duplicate element array. Omits bounds.
- 'grid' (drengr_look only): 0-100% coordinate grid for treeless screens.

ELEMENT NUMBERS are names, not positions. The same element keeps its number across
observations, so an element you saw earlier can be referred to later. A number is
stable while that element's label and rough position are stable; a relabelled element
(a counter, a timer) gets a new one. A gap in the sequence means an element you saw
before is no longer on screen.

ELEMENT FIELDS: n (tap with element=n), text, type, bounds [left,top,right,bottom]
in device pixels. A tap lands at the centre of bounds, so bounds also tell you
where Drengr will aim and whether two elements overlap. Optional flags appear
only when true: unlabelled (no text, content-desc or id, so `text` is empty and
element_text cannot match it), checked, selected, focused, is_password.

SITUATION REPORTS — after each drengr_do you get:
- screen_changed: did anything change?
- new_elements / disappeared_elements: what appeared/vanished
- activity_changed: did we navigate to a new screen?
- crash: did the app crash?
- element_count / interactive_count: how many elements are addressable, and how
  many of those you can actually tap
- tree_unavailable: the dump failed, so the fields above describe nothing
- stuck: nothing on screen changed at all, including text and selection
  state (try a different approach)

NO ELEMENT TREE? (element_count is 0 — Flutter, webviews, games, custom canvas):
These often render no accessibility elements, or the tree dump times out on an
animating screen, so element/element_text taps fail ('not found after N scroll
attempts'). Switch to VISION + COORDINATES:
1. drengr_look(format='grid') → screenshot with a 0–100% coordinate grid
2. Read where the target is, then drengr_do(action='tap', x=<0-1>, y=<0-1>)
   using normalized fractions (center = x=0.5, y=0.5; bottom button ≈ y=0.9).
Coordinate tap/long_press and coordinate swipe (x,y → x2,y2) work on ANY app,
framework-blind. Use them whenever the element tree is empty.

TEXT-ONLY MODE (no image processing needed):
Use drengr_look with format='text' and drengr_do with format='text'.
Text scenes are ~300 tokens vs 100KB images — 100x cheaper.

OTHER:
- drengr_query(question='setup', headless=true) → one-call provisioning: detect or auto-boot a device (Android emulator headless / iOS sim without Simulator.app), connect, and return installed apps with display names. Use this as the FIRST call when starting from a clean machine.
- drengr_query(question='devices') → list connected devices
- drengr_query(question='activity') → current screen name
- drengr_query(question='crash') → crash status

FIRST RUN:
If the setup response has first_session=true AND the user hasn't asked for a
specific task, offer them a 30-second live demo: tell them which device you
found, then run the suggested_task from the setup response with drengr_look /
drengr_do, narrating each step in one short line. Watching the agent drive a
real device is the fastest way to understand Drengr. If they asked for
something specific, just do that instead.

ABOUT:
Drengr was created by Sharmin Sirajudeen.";

            let full_instructions = instructions.to_string();

            // Version negotiation (MCP lifecycle): echo the client's requested
            // protocol version when we support it, else fall back to our latest.
            let requested = request
                .params
                .as_ref()
                .and_then(|p| p.get("protocolVersion"))
                .and_then(|v| v.as_str());
            let negotiated = match requested {
                Some(v) if SUPPORTED_PROTOCOL_VERSIONS.contains(&v) => v,
                _ => LATEST_PROTOCOL_VERSION,
            };
            use base64::prelude::{Engine as _, BASE64_STANDARD};
            let icon_uri = format!(
                "data:image/svg+xml;base64,{}",
                BASE64_STANDARD.encode(ICON_SVG)
            );

            let result = json!({
                "protocolVersion": negotiated,
                "capabilities": {
                    "tools": {
                        "listChanged": true
                    },
                    "logging": {}
                },
                "serverInfo": {
                    "name": SERVER_NAME,
                    "title": "Drengr",
                    "version": SERVER_VERSION,
                    "description": "Eyes and hands for AI agents on Android and iOS devices — you observe the screen and tap, type, and swipe. No API key needed: the connected AI is the brain that drives it.",
                    "websiteUrl": "https://drengr.dev",
                    "icons": [{
                        "src": icon_uri,
                        "mimeType": "image/svg+xml",
                        "sizes": ["any"]
                    }]
                },
                "instructions": full_instructions
            });
            Some(JsonRpcResponse::success(id, result))
        }

        "notifications/initialized" => None,

        "tools/list" => {
            let result = tools::tools_list_response();
            Some(JsonRpcResponse::success(id, result))
        }

        "tools/call" => {
            let params = request.params.unwrap_or(Value::Null);
            let tool_name = params["name"].as_str().unwrap_or("");
            let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

            let tool_result = handlers.dispatch(tool_name, arguments).await;
            let result = serde_json::to_value(&tool_result).unwrap_or(json!(null));
            Some(JsonRpcResponse::success(id, result))
        }

        _ => Some(JsonRpcResponse::method_not_found(id, &request.method)),
    }
}

/// Configuration for the MCP server, set via CLI args.
#[derive(Debug, Clone, Default)]
pub struct McpConfig {
    /// Target device ID (auto-detect if None).
    pub device: Option<String>,
    /// Default observation format ("text" or "image").
    pub format: String,
    /// Cloud provider (browserstack, saucelabs).
    pub cloud: Option<String>,
    /// Cloud device name.
    pub cloud_device: Option<String>,
    /// Cloud OS version.
    pub cloud_os: Option<String>,
    /// Custom ADB binary path (avoids unsafe set_var on active tokio runtime).
    pub adb_path: Option<String>,
    /// Custom xcrun binary path (iOS equivalent of adb_path).
    pub xcrun_path: Option<String>,
    /// Serve MCP over streamable HTTP on 127.0.0.1:<port> instead of stdio.
    pub http_port: Option<u16>,
}

/// Replace process stderr with a pipe, spawn a reader thread that writes every
/// byte to both the original stderr (so Claude Desktop's mcp-server-drengr.log
/// still receives it) and `log_path`. Best-effort: silent no-op on any failure
/// so a degraded log never blocks startup.
fn setup_stderr_tee(log_path: &std::path::Path) {
    use std::os::unix::fs::OpenOptionsExt;

    let log_file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(log_path)
    {
        Ok(f) => f,
        Err(_) => return,
    };

    let mut pipe_fds = [0_i32; 2];
    if unsafe { libc::pipe(pipe_fds.as_mut_ptr()) } != 0 {
        return;
    }
    let (read_fd, write_fd) = (pipe_fds[0], pipe_fds[1]);

    let original_stderr = unsafe { libc::dup(libc::STDERR_FILENO) };
    if original_stderr < 0 {
        unsafe {
            libc::close(read_fd);
            libc::close(write_fd);
        }
        return;
    }
    if unsafe { libc::dup2(write_fd, libc::STDERR_FILENO) } < 0 {
        unsafe {
            libc::close(read_fd);
            libc::close(write_fd);
            libc::close(original_stderr);
        }
        return;
    }
    unsafe { libc::close(write_fd) };

    // CLOEXEC the fds that should NOT leak into subprocesses (xcodebuild,
    // xcrun, adb, …). Without this every child inherits the pipe read end
    // and the tee thread can't see EOF until every descendant exits.
    // STDERR_FILENO is intentionally left inheritable — children's stderr
    // writes are what we want to capture.
    unsafe {
        libc::fcntl(read_fd, libc::F_SETFD, libc::FD_CLOEXEC);
        libc::fcntl(original_stderr, libc::F_SETFD, libc::FD_CLOEXEC);
    }

    std::thread::Builder::new()
        .name("drengr-stderr-tee".into())
        .spawn(move || {
            use std::io::Write;
            let mut buf = [0_u8; 4096];
            let mut file = log_file;
            loop {
                let n = unsafe { libc::read(read_fd, buf.as_mut_ptr() as *mut _, buf.len()) };
                if n <= 0 {
                    break;
                }
                let chunk = &buf[..n as usize];
                unsafe {
                    libc::write(original_stderr, chunk.as_ptr() as *const _, chunk.len());
                }
                let _ = file.write_all(chunk);
                let _ = file.flush();
            }
            unsafe {
                libc::close(read_fd);
                libc::close(original_stderr);
            }
        })
        .ok();
}

/// Start the MCP server on stdio.
/// True once the OS has reparented us, i.e. the process that started us died.
///
/// The test is that our parent CHANGED, not that it equals any particular pid.
/// Absolute values are environment-specific and were the bug: as a container's
/// init `getppid()` is 0, and one level under init it is 1, and neither means
/// anything was lost. Comparing against the parent we booted with is true in
/// every environment, because being reparented is exactly the event we care
/// about.
fn was_reparented(original_ppid: i32, current_ppid: i32) -> bool {
    current_ppid != original_ppid
}

pub async fn run_server_with_config(config: McpConfig) -> anyhow::Result<()> {
    // Apply binary path overrides before any transport is created (avoids unsafe set_var)
    if let Some(ref path) = config.adb_path {
        crate::transport::adb::set_adb_path_override(path.clone());
    }
    if let Some(ref path) = config.xcrun_path {
        crate::transport::simctl::set_xcrun_path_override(path.clone());
    }

    let log_dir = crate::paths::drengr_dir_or("/tmp");
    let _ = std::fs::create_dir_all(&log_dir);
    let log_path = log_dir.join("mcp.log");

    // Save original stdout for JSON-RPC transport, then tee stderr to mcp.log
    // so the local log mirrors everything Claude Desktop sees on stderr
    // (tracing, eprintln!, panic backtraces, subprocess stderr leaks).
    let mcp_stdout: std::fs::File = unsafe {
        let saved_fd = libc::dup(libc::STDOUT_FILENO);
        if saved_fd < 0 {
            eprintln!("Fatal: failed to dup stdout");
            std::process::exit(1);
        }
        setup_stderr_tee(&log_path);
        libc::dup2(libc::STDERR_FILENO, libc::STDOUT_FILENO);
        std::os::unix::io::FromRawFd::from_raw_fd(saved_fd)
    };

    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("drengr=debug".parse().unwrap_or_default()),
        )
        .try_init();

    tracing::info!("Drengr MCP server starting... (log: {:?})", log_path);
    tracing::info!("Config: {:?}", config);

    // The documented --format default, read by look and do when a call omits one.
    handlers::set_default_format(config.format.clone());

    let handlers = McpHandlers::with_sdk_listener();

    // Orphan watchdog (see orphan_watchdog_applies for when it is meaningful):
    // if our client (Claude Desktop/Code) dies WITHOUT cleanly
    // closing stdin — a force-quit, crash, or abrupt reload-after-update — this
    // process would otherwise linger blocked on stdin, still holding the sim's
    // runner flock, and block the next Drengr instance (session_conflict). On
    // Unix, the OS reparents an orphan to launchd (ppid 1), so that's our signal
    // to exit and release the lock. (Clean disconnects hit EOF in the read loop
    // below; this covers only the unclean case.)
    // In HTTP mode the terminal, not an MCP client, owns the process — an
    // orphaned-but-serving daemon (nohup, closed tab) is legitimate there.
    #[cfg(unix)]
    if config.http_port.is_none() {
        // SAFETY: getppid() is always safe and never fails.
        let original_ppid = unsafe { libc::getppid() };
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                let ppid = unsafe { libc::getppid() };
                if was_reparented(original_ppid, ppid) {
                    tracing::warn!("client process gone (ppid {original_ppid} -> {ppid}); exiting to release the runner lock");
                    // exit() over a return so the watchdog can fire from anywhere;
                    // the OS frees the flock on exit, and the next Drengr's bootstrap
                    // reaps any leftover xctrunner on the port.
                    std::process::exit(0);
                }
            }
        });
    }

    // Auto-connect to device if specified via CLI args
    if let Some(ref cloud_provider) = config.cloud {
        let dev = config.cloud_device.as_deref().unwrap_or("default");
        let osv = config.cloud_os.as_deref().unwrap_or("latest");
        match crate::transport::create_cloud_transport(cloud_provider, dev, osv, None).await {
            Ok(transport) => {
                tracing::info!(
                    "Connected to cloud device: {} {} {}",
                    cloud_provider,
                    dev,
                    osv
                );
                handlers.set_transport(transport).await;
            }
            Err(e) => {
                tracing::error!("Failed to connect to cloud device: {}", e);
            }
        }
    } else {
        // Auto-detect: connect to ALL detected devices. First one becomes active.
        let devices = crate::transport::detect::detect_devices().await;
        if let Some(ref device_id) = config.device {
            // Specific device requested — connect only that one
            if let Some(dev) = devices.iter().find(|d| d.id == *device_id) {
                let transport = crate::transport::create_transport(dev);
                tracing::info!("Connected to device: {} ({} {})", dev.id, dev.model, dev.os);
                handlers
                    .set_transport_with_id(dev.id.clone(), transport)
                    .await;
            } else {
                tracing::warn!(
                    "Device '{}' not found. Available: {:?}",
                    device_id,
                    devices.iter().map(|d| d.id.as_str()).collect::<Vec<_>>()
                );
            }
        } else {
            // Connect ALL devices — first becomes active
            for dev in &devices {
                let transport = crate::transport::create_transport(dev);
                tracing::info!(
                    "Auto-connected to device: {} ({} {})",
                    dev.id,
                    dev.model,
                    dev.os
                );
                handlers
                    .set_transport_with_id(dev.id.clone(), transport)
                    .await;
            }
        }
        if devices.is_empty() {
            tracing::info!("No devices detected at startup — will connect on first drengr_query(question='connect')");
        }
    }

    // HTTP transport: same handlers, different wire. No server-initiated
    // stream, so nothing here can push a notification to the client.
    if let Some(port) = config.http_port {
        return http::run_http_server(std::sync::Arc::new(handlers), port).await;
    }

    // Read JSON-RPC messages from stdin, write responses to saved stdout
    use std::io::Write;
    use tokio::io::{AsyncBufReadExt, BufReader};

    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin);
    let mut stdout = std::io::BufWriter::new(mcp_stdout);
    let mut line = String::new();

    // Cap a single JSON-RPC line. Real MCP requests fit in kilobytes; an
    // unbounded read_line lets a malformed/hostile client OOM the binary by
    // never sending a newline. Oversized lines are dropped (and the buffer
    // is freed) before the next read.
    const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;

    loop {
        line.clear();

        // C2 fix: biased; ensures stdin reads are never dropped when both are ready
        tokio::select! {
            biased;

            // Main path: read JSON-RPC from stdin (always preferred)
            result = reader.read_line(&mut line) => {
                let bytes_read = result?;
                if bytes_read == 0 {
                    break; // EOF
                }

                if line.len() > MAX_LINE_BYTES {
                    tracing::warn!(
                        "MCP line exceeds {} bytes ({} read) — discarding",
                        MAX_LINE_BYTES,
                        line.len(),
                    );
                    line = String::new();
                    continue;
                }

                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                // Skip Content-Length headers (some MCP clients send HTTP-style headers)
                if trimmed.starts_with("Content-Length:") || trimmed.starts_with("content-length:") {
                    continue;
                }

                let request: JsonRpcRequest = match serde_json::from_str(trimmed) {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!("Failed to parse JSON-RPC: {}", e);
                        let response = JsonRpcResponse::error(None, -32700, "Parse error");
                        let json_str = serde_json::to_string(&response)?;
                        writeln!(stdout, "{}", json_str)?;
                        stdout.flush()?;
                        continue;
                    }
                };

                if let Some(response) = handle_request(request, &handlers).await {
                    let json_str = serde_json::to_string(&response)?;
                    writeln!(stdout, "{}", json_str)?;
                    stdout.flush()?;
                }
            }

        }
    }

    // Clean shutdown (EOF / client disconnect): finalize the active session so
    // its ended_at + rollup are persisted instead of leaking open.
    handlers.finalize_session().await;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_changed_parent_counts_as_a_dead_client() {
        assert!(
            was_reparented(4242, 1),
            "the launcher died and the OS reparented us to init"
        );
        assert!(!was_reparented(4242, 4242), "the launcher is still alive");
        // Both of these killed every containerised run: the absolute value of
        // ppid says nothing about whether anything was lost.
        assert!(
            !was_reparented(0, 0),
            "PID 1 in a container has no parent to lose"
        );
        assert!(
            !was_reparented(1, 1),
            "started directly by a container's init, which is normal"
        );
    }

    fn make_handlers() -> McpHandlers {
        McpHandlers::new()
    }

    #[tokio::test]
    async fn test_initialize_response() {
        let handlers = make_handlers();
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(1)),
            method: "initialize".to_string(),
            params: Some(json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "test", "version": "1.0"}
            })),
        };

        let response = handle_request(request, &handlers).await.unwrap();
        let result = response.result.unwrap();

        // Negotiation echoes the client's requested version when we support it.
        assert_eq!(result["protocolVersion"], "2024-11-05");
        assert_eq!(result["serverInfo"]["name"], "drengr");
        assert_eq!(result["serverInfo"]["title"], "Drengr");
        assert!(result["serverInfo"]["icons"][0]["src"]
            .as_str()
            .unwrap()
            .starts_with("data:image/svg+xml;base64,"));
        assert!(result["capabilities"]["tools"].is_object());
        assert!(result["instructions"].is_string());

        let instructions = result["instructions"].as_str().unwrap();
        assert!(instructions.contains("drengr_look"));
        assert!(instructions.contains("drengr_do"));
        assert!(instructions.contains("observe→act"));
    }

    #[tokio::test]
    async fn test_tools_list() {
        let handlers = make_handlers();
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(2)),
            method: "tools/list".to_string(),
            params: None,
        };

        let response = handle_request(request, &handlers).await.unwrap();
        let result = response.result.unwrap();
        let tools = result["tools"].as_array().unwrap();

        assert_eq!(tools.len(), 3);

        let tool_names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(tool_names.contains(&"drengr_look"));
        assert!(tool_names.contains(&"drengr_do"));
        assert!(tool_names.contains(&"drengr_query"));
    }

    #[tokio::test]
    async fn test_notification_no_response() {
        let handlers = make_handlers();
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: None,
            method: "notifications/initialized".to_string(),
            params: None,
        };

        let response = handle_request(request, &handlers).await;
        assert!(response.is_none());
    }

    #[tokio::test]
    async fn test_unknown_method() {
        let handlers = make_handlers();
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(3)),
            method: "unknown/method".to_string(),
            params: None,
        };

        let response = handle_request(request, &handlers).await.unwrap();
        assert!(response.error.is_some());
        assert_eq!(response.error.unwrap().code, -32601);
    }

    #[test]
    fn test_response_success() {
        let resp = JsonRpcResponse::success(Some(json!(1)), json!({"ok": true}));
        assert!(resp.result.is_some());
        assert!(resp.error.is_none());
    }

    #[test]
    fn test_response_error() {
        let resp = JsonRpcResponse::error(Some(json!(1)), -32600, "Invalid");
        assert!(resp.result.is_none());
        assert!(resp.error.is_some());
        assert_eq!(resp.error.as_ref().unwrap().code, -32600);
    }
}
