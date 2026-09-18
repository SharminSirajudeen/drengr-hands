pub mod messages;

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

use anyhow::Result;
use subtle::ConstantTimeEq;
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;

use crate::network::events::NetworkEvent;
use crate::network::sink::{NetworkSink, NetworkSource};
use messages::SdkMessage;

// --- SDK auth backoff ---
// After 5 failed auth attempts within a 60s window, the source IP is throttled
// for the remainder of the window. Throttled IPs short-circuit before the
// constant-time compare. Map is small (only recently-failing IPs).
const AUTH_FAILURE_WINDOW: Duration = Duration::from_secs(60);
const AUTH_FAILURE_THRESHOLD: u32 = 5;

fn auth_failures() -> &'static StdMutex<HashMap<IpAddr, (u32, Instant)>> {
    static MAP: OnceLock<StdMutex<HashMap<IpAddr, (u32, Instant)>>> = OnceLock::new();
    MAP.get_or_init(|| StdMutex::new(HashMap::new()))
}

fn is_throttled(ip: IpAddr) -> bool {
    let map = auth_failures().lock().unwrap();
    if let Some((count, last)) = map.get(&ip) {
        return last.elapsed() < AUTH_FAILURE_WINDOW && *count >= AUTH_FAILURE_THRESHOLD;
    }
    false
}

fn record_auth_failure(ip: IpAddr) {
    let mut map = auth_failures().lock().unwrap();
    let entry = map.entry(ip).or_insert((0, Instant::now()));
    if entry.1.elapsed() > AUTH_FAILURE_WINDOW {
        // Window expired — reset counter
        *entry = (1, Instant::now());
    } else {
        entry.0 += 1;
        entry.1 = Instant::now();
    }
}

fn clear_auth_failures(ip: IpAddr) {
    auth_failures().lock().unwrap().remove(&ip);
}

const DEFAULT_SDK_PORT: u16 = 7879;
const MAX_SCREENS: usize = 100;

/// Hard cap on a single SDK message, sized for the payload class the wire now
/// carries: an event may hold a request and a response body, each capped by the
/// SDK at 64 KB, plus headers and JSON escaping. At the old 64 KB the first
/// body-bearing event tripped the oversize branch and disconnected the client.
const MAX_MSG_SIZE: usize = 256 * 1024;

/// Per-connection token bucket: messages allowed per second. Anything above
/// is silently dropped (after framing is consumed) so a flooding client
/// can't starve the runtime or balloon the in-memory event store.
const MAX_MSG_PER_SEC: u32 = 100;

/// Per-connection rate limiter. Token bucket refills to `MAX_MSG_PER_SEC` once
/// per wall-second; each message consumes one token. Lives inside the spawned
/// per-conn task so there's no shared state to lock.
struct RateBucket {
    tokens: u32,
    last_refill: Instant,
}

impl RateBucket {
    fn new() -> Self {
        Self {
            tokens: MAX_MSG_PER_SEC,
            last_refill: Instant::now(),
        }
    }

    fn try_consume(&mut self) -> bool {
        if self.last_refill.elapsed().as_secs_f32() >= 1.0 {
            self.tokens = MAX_MSG_PER_SEC;
            self.last_refill = Instant::now();
        }
        if self.tokens > 0 {
            self.tokens -= 1;
            true
        } else {
            false
        }
    }
}

/// Bounded store for screen/activity transitions.
pub struct ScreenStore {
    screens: VecDeque<ScreenEntry>,
    max: usize,
}

#[derive(Debug, Clone)]
pub struct ScreenEntry {
    pub activity: String,
    pub package: String,
    pub timestamp_ms: u64,
}

impl ScreenStore {
    pub fn new() -> Self {
        Self {
            screens: VecDeque::with_capacity(MAX_SCREENS),
            max: MAX_SCREENS,
        }
    }

    pub fn push(&mut self, entry: ScreenEntry) {
        if self.screens.len() >= self.max {
            self.screens.pop_front();
        }
        self.screens.push_back(entry);
    }

    /// Get the most recent activity name.
    pub fn current_activity(&self) -> Option<&str> {
        self.screens.back().map(|s| s.activity.as_str())
    }

    pub fn clear(&mut self) {
        self.screens.clear();
    }
}

impl Default for ScreenStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Generate a random SDK auth token and write it to ~/.drengr/sdk_token with 0600 permissions.
fn generate_sdk_token() -> Result<String> {
    let token_path = crate::paths::drengr_dir_or("/tmp").join("sdk_token");

    // Reuse the token already on disk. Minting a new one on every start would
    // silently lock out an SDK that read the file when it was integrated, and
    // the MCP server now starts this listener too, so "every start" means
    // every Drengr launch rather than an explicit `drengr sdk-server`.
    if let Ok(existing) = std::fs::read_to_string(&token_path) {
        let existing = existing.trim();
        if existing.len() >= 32 && existing.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Ok(existing.to_string());
        }
    }

    let token = uuid::Uuid::new_v4().to_string().replace('-', "");
    let _ = std::fs::create_dir_all(token_path.parent().unwrap());
    // Write atomically with owner-only permissions
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&token_path)?
            .write_all(token.as_bytes())?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&token_path, &token)?;
    }
    tracing::info!("SDK token written to {:?}", token_path);
    Ok(token)
}

/// Start the SDK TCP server on the configured port.
pub async fn start_sdk_server(
    sink: NetworkSink,
    screen_store: Arc<Mutex<ScreenStore>>,
) -> Result<()> {
    let port = std::env::var("DRENGR_SDK_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(DEFAULT_SDK_PORT);

    // Validate port is in user range (not privileged)
    if port < 1024 {
        anyhow::bail!("SDK port must be >= 1024, got {}", port);
    }

    // Bind first: a second Drengr that loses this port must not touch the token
    // file the process holding the port is authenticating against.
    let listener = TcpListener::bind(format!("127.0.0.1:{}", port)).await?;
    let expected_token = generate_sdk_token()?;
    tracing::info!("SDK server listening on port {}", port);

    // Limit concurrent connections
    let max_connections = Arc::new(tokio::sync::Semaphore::new(10));

    loop {
        let (mut stream, addr) = listener.accept().await?;
        tracing::info!("SDK client connected from {}", addr);

        let sink = sink.clone();
        let screen_store = screen_store.clone();
        let expected_token = expected_token.clone();
        let permit = max_connections.clone().acquire_owned().await?;
        let peer_ip = addr.ip();

        // Reject early if this IP has tripped the auth-failure threshold.
        // Avoids wasting CPU on the constant-time compare and prevents
        // unbounded HashMap growth from a flooding attacker.
        if is_throttled(peer_ip) {
            tracing::warn!(
                "SDK client {} throttled (too many recent auth failures) — disconnecting",
                addr
            );
            continue;
        }

        tokio::spawn(async move {
            let _permit = permit; // held until task exits
            let mut len_buf = [0u8; 4];

            // First message must be Register with valid token (2s timeout)
            let auth_result = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                read_one_message(&mut stream, &mut len_buf),
            )
            .await;

            match auth_result {
                Ok(Ok(msg_buf)) => match SdkMessage::decode(&msg_buf) {
                    Ok(SdkMessage::Register { token: Some(t), .. }) => {
                        // Constant-time compare — short-circuit `==` would leak
                        // the token byte-by-byte via response timing.
                        if bool::from(t.as_bytes().ct_eq(expected_token.as_bytes())) {
                            clear_auth_failures(peer_ip);
                            tracing::info!("SDK client {} authenticated", addr);
                        } else {
                            record_auth_failure(peer_ip);
                            tracing::warn!(
                                "SDK client {} failed auth (bad token) — disconnecting",
                                addr
                            );
                            return;
                        }
                    }
                    Ok(SdkMessage::Register { token: None, .. }) => {
                        record_auth_failure(peer_ip);
                        tracing::warn!(
                            "SDK client {} sent Register without token — disconnecting",
                            addr
                        );
                        return;
                    }
                    _ => {
                        record_auth_failure(peer_ip);
                        tracing::warn!("SDK client {} failed auth — disconnecting", addr);
                        return;
                    }
                },
                _ => {
                    record_auth_failure(peer_ip);
                    tracing::warn!("SDK client {} auth timeout — disconnecting", addr);
                    return;
                }
            }

            let mut bucket = RateBucket::new();

            loop {
                // Read length prefix
                if stream.read_exact(&mut len_buf).await.is_err() {
                    tracing::info!("SDK client disconnected: {}", addr);
                    break;
                }

                let msg_len = u32::from_be_bytes(len_buf) as usize;
                if msg_len == 0 {
                    tracing::warn!("Zero-length message from {}", addr);
                    continue;
                }
                if msg_len > MAX_MSG_SIZE {
                    tracing::warn!(
                        "Message too large ({} bytes, cap {}) from {} — disconnecting",
                        msg_len,
                        MAX_MSG_SIZE,
                        addr
                    );
                    break;
                }

                let mut msg_buf = vec![0u8; msg_len];
                if stream.read_exact(&mut msg_buf).await.is_err() {
                    break;
                }

                // Framing already consumed; drop only the parse/handle.
                if !bucket.try_consume() {
                    tracing::warn!("SDK rate limit exceeded for {} — dropping message", addr);
                    continue;
                }

                match SdkMessage::decode(&msg_buf) {
                    Ok(msg) => {
                        handle_sdk_message(msg, &sink, &screen_store).await;
                    }
                    Err(e) => {
                        tracing::warn!("Failed to parse SDK message: {}", e);
                    }
                }
            }
        });
    }
}

/// Read a single length-prefixed message from a TCP stream.
async fn read_one_message(
    stream: &mut tokio::net::TcpStream,
    len_buf: &mut [u8; 4],
) -> Result<Vec<u8>> {
    stream.read_exact(len_buf).await?;
    let msg_len = u32::from_be_bytes(*len_buf) as usize;
    if msg_len == 0 || msg_len > MAX_MSG_SIZE {
        anyhow::bail!("Invalid message length: {} (max {})", msg_len, MAX_MSG_SIZE);
    }
    let mut msg_buf = vec![0u8; msg_len];
    stream.read_exact(&mut msg_buf).await?;
    Ok(msg_buf)
}

/// Process a single SDK message.
async fn handle_sdk_message(
    msg: SdkMessage,
    sink: &NetworkSink,
    screen_store: &Arc<Mutex<ScreenStore>>,
) {
    match msg {
        SdkMessage::Register {
            app_package,
            app_version,
            ..
        } => {
            tracing::info!("SDK registered: {} v{}", app_package, app_version);
        }
        SdkMessage::Deregister { app_package } => {
            tracing::info!("SDK deregistered: {}", app_package);
        }
        SdkMessage::Event {
            url,
            method,
            status,
            duration_ms,
            request_size,
            response_size,
            timestamp_ms,
            request_headers,
            response_headers,
            request_body,
            response_body,
            truncated,
        } => {
            // The one sink. This push is the whole point: before it, every
            // event four shipped SDKs sent landed in a store nothing read.
            sink.push(
                NetworkSource::InApp,
                truncated,
                NetworkEvent {
                    url,
                    // An SDK that sent no method told us nothing about it. It did
                    // not tell us the method was "".
                    method: (!method.is_empty()).then_some(method),
                    status,
                    duration_ms,
                    request_size,
                    response_size,
                    timestamp_ms,
                    request_headers,
                    response_headers,
                    request_body,
                    response_body,
                },
            );
        }
        SdkMessage::ScreenChange {
            activity,
            package,
            timestamp_ms,
        } => {
            screen_store.lock().await.push(ScreenEntry {
                activity,
                package,
                timestamp_ms,
            });
        }
        SdkMessage::Ping { timestamp_ms } => {
            tracing::trace!("Ping at {}", timestamp_ms);
            // In a full implementation, we'd send Pong back over the stream
        }
        SdkMessage::Pong { .. } => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::sink::BodyTruncation;
    use std::net::Ipv4Addr;

    // --- Auth backoff ---

    fn fresh_ip(seed: u8) -> IpAddr {
        // Use distinct IPs per test so the global tracker doesn't cross-contaminate.
        IpAddr::V4(Ipv4Addr::new(127, 0, 0, seed))
    }

    #[test]
    fn auth_backoff_throttles_after_threshold() {
        let ip = fresh_ip(101);
        clear_auth_failures(ip); // ensure clean slate
        for _ in 0..AUTH_FAILURE_THRESHOLD {
            assert!(!is_throttled(ip), "should not throttle below threshold");
            record_auth_failure(ip);
        }
        assert!(
            is_throttled(ip),
            "should throttle after {} failures",
            AUTH_FAILURE_THRESHOLD
        );
    }

    #[test]
    fn auth_backoff_clears_on_success() {
        let ip = fresh_ip(102);
        clear_auth_failures(ip);
        for _ in 0..AUTH_FAILURE_THRESHOLD {
            record_auth_failure(ip);
        }
        assert!(is_throttled(ip));
        clear_auth_failures(ip);
        assert!(!is_throttled(ip), "successful auth should clear throttle");
    }

    #[test]
    fn constant_time_compare_accepts_matching_token() {
        let token = "abc123def456";
        assert!(bool::from(token.as_bytes().ct_eq(token.as_bytes())));
    }

    #[test]
    fn constant_time_compare_rejects_mismatch() {
        let a = "abc123def456";
        let b = "abc123def457"; // last byte differs
        assert!(!bool::from(a.as_bytes().ct_eq(b.as_bytes())));
    }

    #[test]
    fn constant_time_compare_rejects_different_lengths() {
        let a = "abc";
        let b = "abc1";
        assert!(!bool::from(a.as_bytes().ct_eq(b.as_bytes())));
    }

    #[test]
    fn test_screen_store_current_activity() {
        let mut store = ScreenStore::new();
        assert!(store.current_activity().is_none());

        store.push(ScreenEntry {
            activity: "LoginActivity".to_string(),
            package: "com.app".to_string(),
            timestamp_ms: 1000,
        });

        assert_eq!(store.current_activity(), Some("LoginActivity"));

        store.push(ScreenEntry {
            activity: "DashboardActivity".to_string(),
            package: "com.app".to_string(),
            timestamp_ms: 2000,
        });

        assert_eq!(store.current_activity(), Some("DashboardActivity"));
    }

    #[test]
    fn test_screen_store_bounded() {
        let mut store = ScreenStore {
            screens: VecDeque::new(),
            max: 2,
        };

        store.push(ScreenEntry {
            activity: "A".to_string(),
            package: "com.app".to_string(),
            timestamp_ms: 0,
        });
        store.push(ScreenEntry {
            activity: "B".to_string(),
            package: "com.app".to_string(),
            timestamp_ms: 0,
        });
        store.push(ScreenEntry {
            activity: "C".to_string(),
            package: "com.app".to_string(),
            timestamp_ms: 0,
        });

        assert_eq!(store.current_activity(), Some("C"));
    }

    #[tokio::test]
    async fn sdk_events_land_in_the_shared_sink_tagged_sdk() {
        let sink = NetworkSink::new();
        let screen_store = Arc::new(Mutex::new(ScreenStore::new()));

        let msg = SdkMessage::Event {
            url: "/api/login".to_string(),
            method: "POST".to_string(),
            status: Some(200),
            duration_ms: Some(150),
            request_size: Some(100),
            response_size: Some(500),
            timestamp_ms: 1709500000000,
            request_headers: None,
            response_headers: None,
            request_body: None,
            response_body: None,
            truncated: BodyTruncation::None,
        };

        handle_sdk_message(msg, &sink, &screen_store).await;

        let entries = sink.snapshot();
        assert_eq!(
            entries.len(),
            1,
            "an SDK event that reaches no sink is unreadable"
        );
        assert_eq!(entries[0].source, NetworkSource::InApp);
        assert_eq!(entries[0].event.url, "/api/login");
        assert_eq!(entries[0].event.status, Some(200));
    }

    #[tokio::test]
    async fn an_sdk_event_carrying_bodies_keeps_them_through_the_decoder() {
        let sink = NetworkSink::new();
        let screen_store = Arc::new(Mutex::new(ScreenStore::new()));

        let wire = br#"{"type":"event","url":"https://api.example.com/login","method":"POST","status":200,"duration_ms":12,"request_size":40,"response_size":11,"timestamp_ms":7,"request_headers":[["content-type","application/json"]],"response_headers":[["x-req-id","9"]],"request_body":"{\"email\":\"[REDACTED]\",\"plan\":\"pro\"}","response_body":"{\"ok\":true}","truncated":"request"}"#;
        handle_sdk_message(SdkMessage::decode(wire).unwrap(), &sink, &screen_store).await;

        let entries = sink.snapshot();
        let e = &entries[0].event;
        assert_eq!(
            e.request_body.as_deref(),
            Some(r#"{"email":"[REDACTED]","plan":"pro"}"#)
        );
        assert_eq!(e.response_body.as_deref(), Some(r#"{"ok":true}"#));
        assert_eq!(e.request_headers.as_ref().unwrap()[0].0, "content-type");
        assert_eq!(e.response_headers.as_ref().unwrap()[0].1, "9");
        assert_eq!(
            entries[0].truncated,
            BodyTruncation::Request,
            "a body the SDK cut short must never read back as a complete one"
        );
    }

    #[tokio::test]
    async fn a_seven_field_sdk_event_behaves_exactly_as_before() {
        let sink = NetworkSink::new();
        let screen_store = Arc::new(Mutex::new(ScreenStore::new()));

        let wire = br#"{"type":"event","url":"/old","method":"GET","status":204,"duration_ms":3,"request_size":0,"response_size":0,"timestamp_ms":1}"#;
        handle_sdk_message(SdkMessage::decode(wire).unwrap(), &sink, &screen_store).await;

        let entries = sink.snapshot();
        assert_eq!(entries[0].event.url, "/old");
        assert_eq!(entries[0].event.status, Some(204));
        assert!(entries[0].event.request_body.is_none());
        assert_eq!(entries[0].truncated, BodyTruncation::None);
    }

    #[tokio::test]
    async fn test_handle_sdk_screen_change() {
        let sink = NetworkSink::new();
        let screen_store = Arc::new(Mutex::new(ScreenStore::new()));

        let msg = SdkMessage::ScreenChange {
            activity: "DashboardActivity".to_string(),
            package: "com.app".to_string(),
            timestamp_ms: 1709500000000,
        };

        handle_sdk_message(msg, &sink, &screen_store).await;

        assert_eq!(
            screen_store.lock().await.current_activity(),
            Some("DashboardActivity")
        );
    }

    // --- RateBucket: per-conn token bucket ---

    #[test]
    fn rate_bucket_starts_full() {
        let mut b = RateBucket::new();
        for _ in 0..MAX_MSG_PER_SEC {
            assert!(
                b.try_consume(),
                "first {} consumes must succeed",
                MAX_MSG_PER_SEC
            );
        }
    }

    #[test]
    fn rate_bucket_drops_after_threshold() {
        let mut b = RateBucket::new();
        for _ in 0..MAX_MSG_PER_SEC {
            assert!(b.try_consume());
        }
        // Bucket drained — subsequent calls in the same second must drop.
        assert!(!b.try_consume(), "should drop the (MAX+1)th message");
        assert!(!b.try_consume(), "and stay dropped");
    }

    #[test]
    fn rate_bucket_refills_after_one_second() {
        let mut b = RateBucket::new();
        // Drain.
        for _ in 0..MAX_MSG_PER_SEC {
            b.try_consume();
        }
        assert!(!b.try_consume());
        // Force the refill clock past 1s. Avoids a real sleep in the unit test.
        b.last_refill = Instant::now() - Duration::from_secs(2);
        assert!(b.try_consume(), "bucket should refill after the window");
    }

    #[test]
    fn an_event_at_the_sdks_own_body_cap_fits_under_max_msg_size() {
        // Measured, not reasoned about: at the old 64 KB cap this frame tripped
        // the oversize branch and disconnected the very SDK that sent it.
        let unit = r#"a"b\c"#; // quotes and backslashes: the worst case for JSON escaping
        let body = format!(r#"{{"note":"{}"}}"#, unit.repeat(64 * 1024 / unit.len()));
        let msg = SdkMessage::Event {
            url: "https://api.example.com/v1/checkout".to_string(),
            method: "POST".to_string(),
            status: Some(200),
            duration_ms: Some(12),
            request_size: Some(65_536),
            response_size: Some(65_536),
            timestamp_ms: 1,
            request_headers: Some(vec![("content-type".into(), "application/json".into())]),
            response_headers: Some(vec![("content-type".into(), "application/json".into())]),
            request_body: Some(body.clone()),
            response_body: Some(body),
            truncated: BodyTruncation::Both,
        };
        let framed = msg.encode().unwrap().len() - 4;
        assert!(
            framed <= MAX_MSG_SIZE,
            "a frame carrying two bodies at the SDK's 64 KB cap is {framed} bytes, over the {MAX_MSG_SIZE} cap"
        );
        // Constant by construction: that is the point. It fails the build if
        // anyone raises the cap back toward the pre-hardening 1 MB.
        #[allow(clippy::assertions_on_constants)]
        {
            assert!(
                MAX_MSG_SIZE < 1_048_576,
                "the cap must stay under the pre-hardening 1 MB"
            );
        }
    }
}
