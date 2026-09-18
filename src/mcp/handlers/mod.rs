use std::sync::Arc;
use tokio::sync::Mutex;

use serde_json::{json, Value};

use super::tools::ToolResult;
use crate::explore;
use crate::network::sink::{NetworkSink, NetworkSource};
use crate::screen::annotate::{AnnotatedElement, AnnotatedScreen, ScreenAnnotator};
use crate::screen::text_scene::TextSceneBuilder;
use crate::session::Session;
use crate::situation::SituationEngine;
use crate::transport::DeviceTransport;

/// Emitted whenever a screen yields no elements, so the agent learns why its
/// element taps will miss and what to do instead. Shared by look and do: a
/// treeless screen reached mid-flow needs the same steer as one observed cold.
pub(crate) const NO_TREE_HINT: &str = "No UI elements are addressable on this screen. Element and element_text actions cannot resolve here. format='grid' returns a coordinate grid, and coordinate taps work on any screen. When tree_error is present the dump failed rather than the screen being empty, and a retry may recover the elements.";

/// State shared across MCP tool calls.
pub struct McpHandlers {
    /// Connected transports keyed by device_id. Supports multiple simultaneous devices.
    transports: Arc<Mutex<std::collections::HashMap<String, Arc<dyn DeviceTransport>>>>,

    /// Active device ID — used when tool call doesn't specify a device.
    active_device: Arc<Mutex<Option<String>>>,

    /// Situation engine for screen diffing.
    situation: Arc<Mutex<SituationEngine>>,

    /// Last annotated screen (for element number lookups in drengr_do).
    /// Wrapped in Arc to avoid cloning the JPEG buffer on every store.
    last_annotated: Arc<Mutex<Option<Arc<AnnotatedScreen>>>>,

    /// Annotator instance.
    annotator: ScreenAnnotator,

    /// Stable element ids. An element number is a name, not a position: the same
    /// element keeps its number across renders so an agent can refer back to it.
    registry: Arc<Mutex<crate::screen::registry::ElementRegistry>>,

    /// Current app package (set on launch, used for screen map lookups).
    app_package: Arc<Mutex<Option<String>>>,

    /// Active session for recording test steps + network calls.
    session: Arc<Mutex<Option<Session>>>,

    /// The one network sink. Logcat polling and the in-app SDK
    /// all push here, each event tagged with the source that produced it, and
    /// `drengr_query(network)` reads it. Before this was shared, the SDK's own
    /// store was orphaned and no SDK event could ever be read back.
    network_history: NetworkSink,

    /// Cached update notice — checked once on first tool call, shown once per session.
    update_notice: Arc<tokio::sync::Mutex<Option<String>>>,
    update_checked: std::sync::atomic::AtomicBool,
    welcomed: std::sync::atomic::AtomicBool,
}

impl Default for McpHandlers {
    fn default() -> Self {
        Self::new()
    }
}

/// Rewrite known driver error fingerprints to one-line, user-actionable text.
/// Returns `None` if the message is not from the iOS driver layer (caller keeps original).
pub(crate) fn friendly_driver_error(msg: &str) -> Option<String> {
    if msg.contains("runner_not_provisioned") {
        return Some(
            "iOS runner not built for this Xcode + iOS combination. Run \
             `drengr build-runner` from your terminal (one-time setup, ~30 seconds). \
             After install/Xcode upgrades, re-run."
                .to_string(),
        );
    }
    if msg.contains("driver_xcode_missing") {
        return Some(
            "Xcode is not installed or not selected. Install Xcode from \
             developer.apple.com, then run `sudo xcode-select -s /Applications/Xcode.app`."
                .to_string(),
        );
    }
    if msg.contains("driver_sim_runtime_missing") {
        let ios_major = msg
            .split("ios_major=")
            .nth(1)
            .and_then(|s| s.split(|c: char| !c.is_ascii_digit()).next())
            .filter(|s| !s.is_empty())
            .unwrap_or("N");
        return Some(format!(
            "iOS {} Simulator runtime is not installed. Run \
             `xcodebuild -downloadPlatform iOS` to install it.",
            ios_major
        ));
    }
    None
}

/// Parse a `points` arg into [x,y] pairs. Accepts a JSON array or a JSON string
/// of one (agents send both). None if malformed.
fn parse_points(v: Option<&Value>) -> Option<Vec<(i32, i32)>> {
    let arr: Vec<Value> = match v? {
        Value::Array(a) => a.clone(),
        Value::String(s) => serde_json::from_str(s).ok()?,
        _ => return None,
    };
    arr.iter()
        .map(|p| {
            let pair = p.as_array()?;
            Some((
                pair.first()?.as_i64()? as i32,
                pair.get(1)?.as_i64()? as i32,
            ))
        })
        .collect()
}

/// Convert normalized coordinates (0-1, a fraction of the screen) into the
/// transport's native tap space (iOS points / Android pixels — `screen_size()`
/// returns each platform's own units, so the same fraction lands correctly on
/// both). Clamped so an out-of-range estimate can't tap off-screen.
fn norm_to_px(nx: f64, ny: f64, width: u32, height: u32) -> (i32, i32) {
    let x = (nx.clamp(0.0, 1.0) * width as f64).round() as i32;
    let y = (ny.clamp(0.0, 1.0) * height as f64).round() as i32;
    (x, y)
}

/// The observation format used when a tool call does not name one.
///
/// `drengr mcp --format` is documented as choosing this and defaults to "text",
/// but nothing read `McpConfig.format`: `handle_look` and `handle_do` each
/// hardcoded "image", so the flag did nothing and the help text was wrong in
/// both directions. Set once at startup, read at those two call sites.
static DEFAULT_FORMAT: std::sync::OnceLock<String> = std::sync::OnceLock::new();

pub(crate) fn set_default_format(format: String) {
    let _ = DEFAULT_FORMAT.set(format);
}

pub(crate) fn default_format() -> &'static str {
    DEFAULT_FORMAT.get().map_or("image", String::as_str)
}

/// Every question `handle_query` has an arm for. Kept honest the same way as
/// `dispatched_actions`: the guard compares it to the advertised catalog and
/// fails on drift in either direction.
#[cfg(test)]
pub(crate) fn dispatched_questions() -> &'static [&'static str] {
    &[
        "capabilities",
        "devices",
        "activity",
        "connect",
        "ui_dump",
        "setup",
        "screen_stream",
        "crash",
        "find",
        "explore",
        "session",
        "logcat",
        "keyboard",
        "network",
        "app_state",
        "assert",
        "diff",
        "analyze",
    ]
}

/// Action strings dispatched by `handle_do`. Kept in sync with the match arms
/// there and enforced equal to `actions::ACTIONS` by a test — so an advertised
/// action can never silently fall through to "Unknown action" again.
#[cfg(test)]
pub(crate) fn dispatched_actions() -> &'static [&'static str] {
    &[
        "tap",
        "type",
        "swipe",
        "long_press",
        "back",
        "home",
        "launch",
        "key",
        "start_recording",
        "stop_recording",
        "install",
        "clear_and_type",
        "scroll_to_bottom",
        "scroll_to_top",
        "wait",
        "draw_path",
        "swipe_with_velocity",
        "go_home",
        "launch_app",
        "terminate_app",
        "open_url",
        "spotlight_search",
        "clear_app_data",
        "reset_app",
        "list_installed_apps",
        "deep_link",
        "uninstall",
        "set_location",
        "clear_location",
        "set_appearance",
        "simulate_biometric",
        "pasteboard_set",
        "pasteboard_get",
        "grant_permission",
        "set_orientation",
        "unlock",
        "alert_text",
        "alert_accept",
        "alert_dismiss",
        "app_state",
    ]
}

mod do_action;
mod look;
mod query;

/// Shown whenever a tool needs a device but none is connected. Advertises that
/// Drengr can BOOT one (the capability agents miss and hand-roll), not just
/// attach to a running one.
pub(super) const NO_DEVICE_HINT: &str =
    "No device connected. Drengr can boot one for you: drengr_query(question='setup', headless=true) \
     — or `drengr query setup --headless` from a shell. \
     (To attach to an already-running device instead: drengr_query(question='connect').)";

impl McpHandlers {
    pub fn new() -> Self {
        Self {
            transports: Arc::new(Mutex::new(std::collections::HashMap::new())),
            active_device: Arc::new(Mutex::new(None)),
            situation: Arc::new(Mutex::new(SituationEngine::new())),
            last_annotated: Arc::new(Mutex::new(None)),
            annotator: ScreenAnnotator::new(),
            registry: Arc::new(Mutex::new(crate::screen::registry::ElementRegistry::new())),
            app_package: Arc::new(Mutex::new(None)),
            session: Arc::new(Mutex::new(None)),
            network_history: NetworkSink::new(),
            update_notice: Arc::new(tokio::sync::Mutex::new(None)),
            update_checked: std::sync::atomic::AtomicBool::new(false),
            welcomed: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Construct and start the in-app SDK listener (production).
    pub fn with_sdk_listener() -> Self {
        let handlers = Self::new();
        handlers.spawn_sdk_listener();
        handlers
    }

    /// Serve the in-app SDK on its TCP port, pushing what it reports into the
    /// same sink `drengr_query(network)` reads. This is the only production
    /// constructor, so there is one place this can be forgotten rather than one
    /// per server transport. Fail-open: the port already being taken (a second
    /// Drengr, or a standalone `drengr sdk-server`) must not stop the MCP
    /// server, so it is logged and the rest of Drengr runs unchanged.
    fn spawn_sdk_listener(&self) {
        static STARTED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        if STARTED.set(()).is_err() {
            return;
        }
        let sink = self.network_history.clone();
        tokio::spawn(async move {
            let screens = Arc::new(Mutex::new(crate::sdk::ScreenStore::new()));
            if let Err(e) = crate::sdk::start_sdk_server(sink, screens).await {
                tracing::warn!(
                    "SDK listener not started ({e}) — in-app SDK events will not be captured"
                );
            }
        });
    }

    fn wrap_result(&self, mut result: ToolResult) -> ToolResult {
        // One-time-ever credit line — PLAIN INFO, never an embedded instruction.
        // A tool whose job is to be driven by agents must not put imperatives in
        // its output (prompt-injection shape). Gated on a disk marker so it shows
        // once total, not on every fresh CLI process (which resets `welcomed`).
        if !self
            .welcomed
            .swap(true, std::sync::atomic::Ordering::Relaxed)
            && claim_welcome_marker()
        {
            let welcome = "✨ Drengr — eyes & hands for AI agents on mobile, by Sharmin Sirajudeen. (Optional: `drengr login` links machines to a free account — never required for local use.)";
            for content in &mut result.content {
                if content.content_type == "text" {
                    if let Some(ref text) = content.text {
                        content.text = Some(format!("{}\n\n{}", welcome, text));
                    }
                }
            }
        }
        // One update notice per released version, not per session. Per-session
        // was already the intent, but `drengr <cmd>` builds a fresh McpHandlers
        // every run, so a CLI user met the nag on every single call. The marker
        // on disk is what makes "once" survive process boundaries.
        if !self
            .update_checked
            .swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            let notice = self.update_notice.clone();
            tokio::spawn(async move {
                let current = env!("CARGO_PKG_VERSION");
                let latest = async {
                    let resp = crate::http::client()
                        .get("https://registry.npmjs.org/drengr/latest")
                        .send()
                        .await
                        .ok()?;
                    let json: serde_json::Value = resp.json().await.ok()?;
                    json.get("version")?.as_str().map(|s| s.to_string())
                }
                .await;
                if let Some(ver) = latest {
                    if ver != current && !update_notice_already_shown(&ver) {
                        // Hold the version, not the sentence. The marker must be
                        // written where the notice is actually appended, not
                        // here: a CLI run renders its response before this task
                        // finishes, so marking now would retire a notice the
                        // user never saw.
                        *notice.lock().await = Some(ver);
                    }
                }
            });
        } else if let Ok(guard) = self.update_notice.try_lock() {
            if let Some(ref ver) = *guard {
                let message = format!(
                    "\n\n📦 Drengr {} is available (you have {}). Run `drengr update` in your terminal.",
                    ver,
                    env!("CARGO_PKG_VERSION")
                );
                let mut shown = false;
                for content in &mut result.content {
                    if content.content_type == "text" {
                        if let Some(ref mut text) = content.text {
                            text.push_str(&message);
                            shown = true;
                        }
                    }
                }
                if shown {
                    mark_update_notice_shown(ver);
                }
                drop(guard);
                if let Ok(mut g) = self.update_notice.try_lock() {
                    *g = None;
                }
            }
        }
        result
    }

    /// Add a transport with a known ID. Use when the device ID is already available
    /// (startup detection, local connect). Keeps previous connections alive.
    pub async fn set_transport_with_id(&self, id: String, transport: Box<dyn DeviceTransport>) {
        let _ = self.set_transport_with_id_returning(id, transport).await;
    }

    /// Same as `set_transport_with_id` but hands back the registered Arc, so
    /// callers that need to use the transport immediately don't have to
    /// re-acquire the map lock and risk a TOCTOU on a concurrent swap.
    pub async fn set_transport_with_id_returning(
        &self,
        id: String,
        transport: Box<dyn DeviceTransport>,
    ) -> Option<Arc<dyn DeviceTransport>> {
        if !crate::validate::is_valid_device_id(&id) {
            tracing::warn!("Rejected invalid device ID: {:?}", id);
            return None;
        }
        let arc: Arc<dyn DeviceTransport> = Arc::from(transport);
        // Learn who this device really is before anything stamps a record with it:
        // the same phone on USB and on WiFi has two transport ids and one serial.
        arc.resolve_identity().await;
        self.transports.lock().await.insert(id.clone(), arc.clone());
        *self.active_device.lock().await = Some(id);
        Some(arc)
    }

    /// Add a transport, deriving the ID via device_info(). Used for cloud devices
    /// where the stable ID is computed inside the transport itself.
    pub async fn set_transport(&self, transport: Box<dyn DeviceTransport>) {
        let id = transport
            .device_info()
            .await
            .map(|d| d.id.clone())
            .unwrap_or_else(|_| "default".to_string());
        self.set_transport_with_id(id, transport).await;
    }

    async fn resolve_transport(&self, args: &Value) -> Option<Arc<dyn DeviceTransport>> {
        self.resolve_transport_keyed(args).await.map(|(_, t)| t)
    }

    /// The same resolution, keeping the map key. Capture setup drives
    /// `adb -s <serial>` directly and the key is that serial; `id()` resolves to
    /// the hardware serial, which adb does not accept for a WiFi transport.
    async fn resolve_transport_keyed(
        &self,
        args: &Value,
    ) -> Option<(String, Arc<dyn DeviceTransport>)> {
        let requested = args.get("device").and_then(|d| d.as_str());
        let transports = self.transports.lock().await;

        if let Some(id) = requested {
            // Exact match first, then prefix match
            if let Some(t) = transports.get(id) {
                return Some((id.to_string(), t.clone()));
            }
            for (key, t) in transports.iter() {
                if device_id_prefix_matches(key, id) {
                    return Some((key.clone(), t.clone()));
                }
            }
            return None;
        }

        // No device specified — use active
        let active = self.active_device.lock().await;
        if let Some(ref id) = *active {
            return transports.get(id).map(|t| (id.clone(), t.clone()));
        }

        // Fallback: first transport in the map
        transports
            .iter()
            .next()
            .map(|(k, t)| (k.clone(), t.clone()))
    }

    /// Resolve a transport, auto-provisioning a device when none is connected:
    /// prefer an already-booted device, else headless-boot one (Android first —
    /// fastest), connect it, and make it active. This turns the first
    /// drengr_look / drengr_do into "it just works" instead of erroring with
    /// NO_DEVICE_HINT — the #1 activation cliff (the external funnel died
    /// entirely at device-attach). Errors only if booting genuinely fails.
    pub(super) async fn ensure_or_autoprovision(
        &self,
        args: &Value,
    ) -> Result<Arc<dyn DeviceTransport>, String> {
        if let Some(t) = self.resolve_transport(args).await {
            return Ok(t);
        }
        // Unit tests must never detect or boot a real device — keep the
        // no-device path deterministic regardless of what's running on the host.
        #[cfg(test)]
        {
            Err(NO_DEVICE_HINT.to_string())
        }
        #[cfg(not(test))]
        {
            self.autoprovision_boot().await
        }
    }

    /// Detect-or-boot the actual device (production only). Split out so the
    /// hot-path autoprovision never compiles real device I/O into test builds.
    #[cfg(not(test))]
    async fn autoprovision_boot(&self) -> Result<Arc<dyn DeviceTransport>, String> {
        // A device may be booted but not yet connected — adopt it. Goes through
        // `classify` so this path cannot silently pick a different device than
        // the CLI would, and so DRENGR_DEVICE is honoured here too. Ambiguity
        // and a bad pin are refused rather than falling through to a boot,
        // which would quietly target a device the caller never chose.
        use crate::transport::detect::Selection;
        let selection = crate::transport::detect::classify(
            crate::transport::detect::detect_devices().await,
            std::env::var("DRENGR_DEVICE").ok(),
        );
        let adopt = match selection {
            Selection::Resolved(d) => Some(d),
            Selection::PinnedButMissing { wanted, available } => {
                return Err(format!(
                    "DRENGR_DEVICE is set to `{wanted}`, which is not connected. Attached: {}",
                    available.join(", ")
                ));
            }
            Selection::Ambiguous(devices) => {
                return Err(format!(
                    "{} devices are connected and none is pinned: {}. Set DRENGR_DEVICE to one of                      these ids, or disconnect the others.",
                    devices.len(),
                    devices
                        .iter()
                        .map(|d| format!("{} ({})", d.id, d.model))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            Selection::None => None,
        };
        if let Some(d) = adopt {
            let transport = crate::transport::create_transport(&d);
            if let Some(t) = self
                .set_transport_with_id_returning(d.id.clone(), transport)
                .await
            {
                *self.active_device.lock().await = Some(d.id);
                return Ok(t);
            }
        }
        // Nothing booted — headless-boot one (Android first, iOS fallback).
        let (id, os) = match crate::transport::boot::boot_android(None, true).await {
            Ok(b) => (b.id, crate::transport::DeviceOs::Android),
            Err(ea) => match crate::transport::boot::boot_ios_simulator(None).await {
                Ok(b) => (b.id, crate::transport::DeviceOs::Ios),
                Err(ei) => {
                    return Err(format!(
                        "No device connected and auto-boot failed (android: {ea}; ios: {ei}). \
                         Create an AVD with `avdmanager create avd`, or download an iOS runtime \
                         in Xcode > Settings > Components."
                    ))
                }
            },
        };
        let os_str = os.to_string();
        let d = crate::transport::DetectedDevice {
            id: id.clone(),
            os,
            model: format!("{os_str} (auto-booted)"),
            sdk_version: None,
        };
        let transport = crate::transport::create_transport(&d);
        match self
            .set_transport_with_id_returning(id.clone(), transport)
            .await
        {
            Some(t) => {
                *self.active_device.lock().await = Some(id);
                Ok(t)
            }
            None => Err(format!("Auto-booted device id rejected: {id:?}")),
        }
    }

    /// Start a fresh recording session, finalizing any prior one first so its
    /// `ended_at` + rollup are written before we drop it (no leaked open sessions).
    pub(crate) async fn start_session(&self, app_package: &str, device_id: &str) {
        self.finalize_session().await;
        *self.session.lock().await = Some(crate::session::Session::new(app_package, device_id));
    }

    /// Finalize the active session: `finish()` (stamp end + rollup) then `save()`.
    /// Idempotent — no-op when none is active. Called on session replacement and
    /// on MCP server shutdown.
    pub(crate) async fn finalize_session(&self) {
        if let Some(mut session) = self.session.lock().await.take() {
            session.finish();
            let _ = session.save();
        }
    }

    /// Get navigation context from screen map (if one exists for current app).
    async fn nav_context_for(&self, activity: &str) -> Option<Value> {
        let pkg = self.app_package.lock().await.clone()?;
        let map = explore::load_screen_map(&pkg).ok()??;
        Some(explore::navigation_context(&map, activity))
    }

    /// Scroll the screen to find an element matching target text, return its tap coordinates.
    async fn scroll_to_find_element(
        &self,
        transport: &dyn DeviceTransport,
        target_text: &str,
        max_attempts: usize,
        screen_size: (u32, u32),
    ) -> Option<(i32, i32)> {
        let target_lower = target_text.to_lowercase();
        let (width, height) = screen_size;

        // Check current screen first
        if let Some(coords) = self.find_element_in_tree(transport, &target_lower).await {
            return Some(coords);
        }

        // Scroll down (swipe up) to find element
        for _ in 0..max_attempts {
            let (from, to) = crate::transport::swipe_coords("up", width, height);
            if transport
                .swipe(from, to, crate::transport::DEFAULT_SWIPE_DURATION_MS)
                .await
                .is_err()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            if let Some(coords) = self.find_element_in_tree(transport, &target_lower).await {
                return Some(coords);
            }
        }

        // Scroll back up then past start
        for _ in 0..(max_attempts * 2) {
            let (from, to) = crate::transport::swipe_coords("down", width, height);
            if transport
                .swipe(from, to, crate::transport::DEFAULT_SWIPE_DURATION_MS)
                .await
                .is_err()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            if let Some(coords) = self.find_element_in_tree(transport, &target_lower).await {
                return Some(coords);
            }
        }

        None
    }

    /// Search current UI tree for element matching text, return center coordinates.
    async fn find_element_in_tree(
        &self,
        transport: &dyn DeviceTransport,
        target_lower: &str,
    ) -> Option<(i32, i32)> {
        let elements = transport.ui_tree().await.ok()?;
        let matches_text =
            |el: &crate::screen::ui_element::UiElement| el.matches_text(target_lower);
        // Prefer a directly tappable match...
        if let Some(el) = elements
            .iter()
            .find(|el| (el.clickable || el.editable) && matches_text(el))
        {
            return Some((el.bounds.center_x(), el.bounds.center_y()));
        }
        // ...but fall back to ANY visible element with the text. The vision-first
        // iOS driver often doesn't flag product labels clickable even when the
        // cell behind them is tappable; tapping the label's center hits the cell.
        let el = elements.iter().find(|el| matches_text(el))?;
        Some((el.bounds.center_x(), el.bounds.center_y()))
    }

    /// Generate an upsell response for gated premium features.
    /// Dispatch a tool call to the appropriate handler. Errors from the iOS
    /// driver layer are rewritten to actionable hints before reaching the MCP
    /// client (see `friendly_driver_error`).
    pub async fn dispatch(&self, tool_name: &str, args: Value) -> ToolResult {
        let mut result = match tool_name {
            "drengr_look" => self.handle_look(args).await,
            "drengr_do" => self.handle_do(args).await,
            "drengr_query" => self.handle_query(args).await,
            _ => ToolResult::error(format!("Unknown tool: {}", tool_name)),
        };

        let raw_error_text = if result.is_error.unwrap_or(false) {
            result.content.first().and_then(|c| c.text.clone())
        } else {
            None
        };

        if let Some(text) = &raw_error_text {
            if let Some(friendly) = friendly_driver_error(text) {
                result = ToolResult::error(friendly);
            } else if is_device_lost_error(text) {
                result = ToolResult::error(
                    "Device lost — the emulator/simulator died or was disconnected. \
                     Reconnect or boot one (drengr_query setup headless), then retry.",
                );
            }
        }

        self.wrap_result(result)
    }
}

/// Extract package name from activity string (e.g. "com.app/.LoginActivity" → "com.app").
/// A requested device id matches a candidate when either is a prefix of the
/// other, so `emulator` reaches `emulator-5554` and a full udid reaches itself.
/// Callers try an exact match first; this decides the rest.
pub(super) fn device_id_prefix_matches(candidate: &str, requested: &str) -> bool {
    candidate.starts_with(requested) || requested.starts_with(candidate)
}

fn extract_package(activity: &str) -> &str {
    activity.split('/').next().unwrap_or(activity)
}

/// True the first time it's called on this machine — atomically claims a
/// `~/.drengr/welcomed` marker. Keeps the one-time credit line from repeating on
/// every fresh CLI process (each resets the in-memory `welcomed` flag).
fn claim_welcome_marker() -> bool {
    let Ok(dir) = crate::paths::ensure_drengr_dir() else {
        return false;
    };
    let marker = dir.join("welcomed");
    if marker.exists() {
        return false;
    }
    // create_new = atomic claim; if another process wins the race, we lose.
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&marker)
        .is_ok()
}

/// Recognize the device-vanished errors adb/simctl emit, so we surface
/// "device lost" instead of a bare "device not found".
fn is_device_lost_error(text: &str) -> bool {
    let t = text.to_lowercase();
    t.contains("device not found")
        || t.contains("device offline")
        || t.contains("no devices/emulators found")
        || (t.contains("device '") && t.contains("' not found"))
}

/// Path of the marker recording the last update notice we printed.
pub(crate) fn update_notice_marker_in(dir: &std::path::Path) -> std::path::PathBuf {
    dir.join("update_notice")
}

/// Whether this exact version has already been announced on this machine.
pub(crate) fn update_notice_shown_in(dir: &std::path::Path, version: &str) -> bool {
    std::fs::read_to_string(update_notice_marker_in(dir))
        .ok()
        .is_some_and(|seen| seen.trim() == version)
}

/// Record that we announced `version`. Best effort: a machine that cannot write
/// the marker gets the notice again, which is far better than losing it.
pub(crate) fn mark_update_notice_shown_in(dir: &std::path::Path, version: &str) {
    let path = update_notice_marker_in(dir);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, version);
}

fn update_notice_already_shown(version: &str) -> bool {
    crate::paths::drengr_dir().is_some_and(|d| update_notice_shown_in(&d, version))
}

fn mark_update_notice_shown(version: &str) {
    if let Some(dir) = crate::paths::drengr_dir() {
        mark_update_notice_shown_in(&dir, version);
    }
}

/// Elements a user can tap but nothing can name: no text, no content-desc, no id.
/// Computed from what Drengr already parses rather than read from uiautomator's NAF
/// attribute, which modern Android does not emit at all, so this lint reports on
/// every device instead of only the vendors that flag it.
impl McpHandlers {
    /// Stamp where and when an observation happened, so the record a later
    /// process reads can be judged rather than trusted.
    pub(super) async fn observation_context(
        &self,
        transport: &dyn DeviceTransport,
        activity: &str,
        package: &str,
    ) -> crate::screen::annotate::ObservationContext {
        // Bounded: a long session accumulates one entry per distinct element ever
        // seen, and this file is rewritten on every action.
        const MAX_PERSISTED_IDS: usize = 1000;
        crate::screen::annotate::ObservationContext {
            // The device this observation actually came from, not whichever one
            // happens to be active: stamping the wrong id is worse than none,
            // because it makes the mismatch check confidently wrong.
            device: transport.id().to_string(),
            package: package.to_string(),
            activity: activity.to_string(),
            written_at: chrono::Utc::now().to_rfc3339(),
            assignments: self.registry.lock().await.assignments(MAX_PERSISTED_IDS),
        }
    }
}

/// Annotate a frame with STABLE element numbers. The registry hands the same
/// element the same number across renders, so a number an agent read in one
/// observation still names that element in the next. Both look and do go through
/// here: numbering in two places is how they drift apart.
impl McpHandlers {
    pub(super) async fn annotate_stable(
        &self,
        frame: &[u8],
        elements: &[crate::screen::ui_element::UiElement],
        logical_size: (u32, u32),
        max: Option<usize>,
        transport_id: &str,
    ) -> anyhow::Result<AnnotatedScreen> {
        let addressable = crate::screen::ui_element::addressable(
            elements,
            max.unwrap_or_else(crate::screen::ui_element::max_addressable),
        );
        let mut registry = self.registry.lock().await;
        // A one-shot CLI process starts with an empty registry, so numbers would
        // restart at 1 and mean something different from what the last process
        // handed out. Adopt that process's ids first.
        if registry.is_empty() {
            if let Some(prev) =
                crate::screen::annotate::ScreenAnnotator::observation_for_device(transport_id)
            {
                // The whole map, so a screen navigated away from keeps its
                // numbers, not just the elements of the last observation.
                registry.seed(prev.ids);
                registry.seed(
                    prev.elements
                        .iter()
                        .filter_map(|e| e.fingerprint.map(|fp| (fp, e.number))),
                );
            }
        }
        let numbered = registry.assign(addressable);
        self.annotator
            .annotate_with_ids(frame, &numbered, logical_size)
    }
}

pub(crate) fn clickable_without_label(elements: &[AnnotatedElement]) -> usize {
    elements
        .iter()
        .filter(|e| {
            e.element.clickable && !e.element.has_label() && e.element.resource_id.is_empty()
        })
        .count()
}

/// Reject oversized screenshots before anything decodes them (reads the header
/// only, no decode). Format-agnostic: PNG screencap or iOS JPEG.
pub(super) fn oversized(screenshot: &[u8]) -> Option<String> {
    let reader = image::ImageReader::new(std::io::Cursor::new(screenshot))
        .with_guessed_format()
        .ok()?;
    let (w, h) = reader.into_dimensions().ok()?;
    let max = crate::transport::MAX_SCREENSHOT_DIM;
    (w > max || h > max).then(|| {
        format!("Screenshot too large ({w}x{h}). Max {max}x{max}. Try reducing device resolution.")
    })
}

/// The ONE element projection. Every surface that returns elements goes
/// through here: a second hand-rolled copy is how bounds and state went missing
/// from find and the accessibility audit.
fn annotated_elements_to_json(elements: &[AnnotatedElement]) -> Vec<Value> {
    elements
        .iter()
        .map(|e| {
            let el = &e.element;
            // display_label() falls back to the class name, so an unlabelled
            // node used to arrive as text:"View", indistinguishable from one
            // genuinely labelled "View". Report the absence instead of guessing.
            let labelled = el.is_labelled();
            let b = &el.bounds;
            let mut obj = json!({
                "n": e.number,
                "text": el.label_or_empty(),
                "type": el.short_class(),
                // Device pixels, same space as ui_dump. A tap on this element
                // lands at the centre, so bounds also show where Drengr aims.
                "bounds": [b.left, b.top, b.right, b.bottom],
            });
            if !labelled {
                obj["unlabelled"] = json!(true);
            }
            if el.checked {
                obj["checked"] = json!(true);
            }
            if el.selected {
                obj["selected"] = json!(true);
            }
            if !el.enabled {
                // A disabled control passes is_relevant, so it is in the list. The
                // text scene marks it read-only and element_audit reports clickable;
                // this projection said nothing, so an agent tapped a greyed-out
                // button and read the resulting no-op as "stuck".
                obj["disabled"] = json!(true);
            }
            if el.focused {
                obj["focused"] = json!(true);
            }
            if el.is_password {
                obj["is_password"] = json!(true);
            }
            obj
        })
        .collect()
}

#[cfg(test)]
mod coord_tests;

#[cfg(test)]
mod tests;
