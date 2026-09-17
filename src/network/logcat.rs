use crate::network::events::NetworkEvent;

/// Parse OkHttp logcat output into NetworkEvent list.
/// Expected format from `adb logcat -d -s okhttp.OkHttpClient:I`:
///   --> POST https://api.example.com/path
///   <-- 200 https://api.example.com/path (507ms)
///   {"json":"body"}
pub fn parse_okhttp_logcat(logcat: &str) -> Vec<NetworkEvent> {
    let mut events: Vec<NetworkEvent> = Vec::new();
    let mut pending_method = String::new();
    let mut pending_url = String::new();
    let mut pending_body_lines: Vec<String> = Vec::new();
    let mut in_response_body = false;

    for line in logcat.lines() {
        let content = strip_logcat_prefix(line);

        // Request start: "--> POST https://..."
        if content.starts_with("--> ") && !content.starts_with("--> END") {
            let parts: Vec<&str> = content[4..].splitn(2, ' ').collect();
            if parts.len() == 2 {
                pending_method = parts[0].to_string();
                pending_url = parts[1].to_string();
                in_response_body = false;
                pending_body_lines.clear();
            }
            continue;
        }

        // Response start: "<-- 200 https://... (507ms)"
        if content.starts_with("<-- ") && !content.starts_with("<-- END") {
            let rest = &content[4..];
            let parts: Vec<&str> = rest.splitn(3, ' ').collect();
            if parts.len() >= 2 {
                // A status field we could not parse is a status we do not have.
                let status: Option<u16> = parts[0].parse().ok();
                let url = parts[1].to_string();
                let duration_ms = extract_duration_ms(rest);

                // The method is only known when this response correlates to a
                // request line we already saw. Uncorrelated means unknown.
                let method = (url == pending_url && !pending_method.is_empty())
                    .then(|| pending_method.clone());

                events.push(NetworkEvent {
                    url,
                    method,
                    status,
                    duration_ms,
                    // A logcat line carries no body sizes. Not zero: absent.
                    request_size: None,
                    response_size: None,
                    timestamp_ms: now_ms(),
                    request_headers: None,
                    response_headers: None,
                    request_body: None,
                    response_body: None,
                });

                in_response_body = true;
                pending_body_lines.clear();
            }
            continue;
        }

        // End markers
        if content.starts_with("<-- END HTTP") {
            // Attach collected body to last event
            if !pending_body_lines.is_empty() {
                if let Some(last) = events.last_mut() {
                    let body = pending_body_lines.join("");
                    // Truncate large bodies (keep first 2KB)
                    let cut = crate::network::truncate_on_char_boundary(&body, 2048);
                    last.response_body = Some(if cut.len() < body.len() {
                        format!("{cut}...(truncated)")
                    } else {
                        body
                    });
                    // Now we HAVE measured it, so it stops being absent.
                    last.response_size = last.response_body.as_ref().map(|b| b.len() as u64);
                }
            }
            in_response_body = false;
            pending_body_lines.clear();
            pending_method.clear();
            pending_url.clear();
            continue;
        }

        if content.starts_with("--> END") {
            // Extract request size from "--> END POST (449-byte body)"
            if let Some(size) = extract_body_size(content) {
                // Attach to next event (not yet created)
                // Store for later
                pending_body_lines.clear();
                // We'll set request_size when we create the event
                // For now, skip — the response event will be created later
                let _ = size;
            }
            continue;
        }

        // Response body lines (between <-- and <-- END)
        if in_response_body && !content.is_empty() && !is_header_line(content) {
            pending_body_lines.push(content.to_string());
        }
    }

    events
}

/// Parse iOS CFNetwork/NSURLSession os_log output into NetworkEvent list.
/// Expected format from `log show --predicate 'subsystem == "com.apple.CFNetwork"'`:
///   Task <ID>.<ID> resuming, QOS(0x19) Blocking
///   Task <ID>.<ID> received response, status 200 content K
pub fn parse_ios_network_log(log_output: &str) -> Vec<NetworkEvent> {
    let mut events: Vec<NetworkEvent> = Vec::new();

    for line in log_output.lines() {
        // Look for HTTP status lines in os_log
        if line.contains("received response") && line.contains("status") {
            if let Some(event) = parse_cfnetwork_response(line) {
                events.push(event);
            }
        }
    }

    events
}

fn parse_cfnetwork_response(line: &str) -> Option<NetworkEvent> {
    // Try to extract status code from "status XXX"
    let status_idx = line.find("status ")?;
    let rest = &line[status_idx + 7..];
    let status_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    let status: u16 = status_str.parse().ok()?;

    Some(NetworkEvent {
        // FIXME(url-sentinel): this line carries no URL either, and "unknown" is
        // the same lie `method` and `status` just stopped telling. `url` needs to
        // become Option too; the guard below records it rather than hiding it.
        url: "unknown".to_string(),
        method: None,
        status: Some(status),
        duration_ms: None,
        request_size: None,
        response_size: None,
        timestamp_ms: now_ms(),
        request_headers: None,
        response_headers: None,
        request_body: None,
        response_body: None,
    })
}

/// Strip logcat prefix "03-09 15:28:46.443 30845 32178 I okhttp.OkHttpClient: " → content
fn strip_logcat_prefix(line: &str) -> &str {
    // Find the tag separator ": " after the tag name
    if let Some(pos) = line.find("okhttp.OkHttpClient: ") {
        return &line[pos + 21..];
    }
    if let Some(pos) = line.find("CURL_TAG: ") {
        return &line[pos + 10..];
    }
    // For iOS logs or generic format
    if let Some(pos) = line.find(": ") {
        return &line[pos + 2..];
    }
    line
}

/// Extract duration from "(507ms)" at end of response line. `None` when the
/// line carried no duration, which is not a request that took no time.
fn extract_duration_ms(line: &str) -> Option<u64> {
    let start = line.rfind('(')?;
    let rest = &line[start + 1..];
    let end = rest.find("ms)")?;
    rest[..end].trim().parse().ok()
}

/// Extract body size from "--> END POST (449-byte body)"
fn extract_body_size(line: &str) -> Option<u64> {
    if let Some(start) = line.find('(') {
        let rest = &line[start + 1..];
        if let Some(end) = rest.find("-byte") {
            return rest[..end].trim().parse().ok();
        }
    }
    None
}

/// Check if a line looks like an HTTP header (Key: Value).
fn is_header_line(line: &str) -> bool {
    // Headers have format "Key-Name: value" — at least one letter before colon
    if let Some(colon_pos) = line.find(": ") {
        let key = &line[..colon_pos];
        return !key.is_empty()
            && key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            && key
                .chars()
                .next()
                .map(|c| c.is_ascii_alphabetic())
                .unwrap_or(false);
    }
    false
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_okhttp_basic() {
        let logcat = r#"03-09 15:28:46.443 30845 32178 I okhttp.OkHttpClient: --> POST https://api.example.com/login
03-09 15:28:46.443 30845 32178 I okhttp.OkHttpClient: Content-Type: application/json
03-09 15:28:46.443 30845 32178 I okhttp.OkHttpClient: --> END POST (100-byte body)
03-09 15:28:46.952 30845 32178 I okhttp.OkHttpClient: <-- 200 https://api.example.com/login (507ms)
03-09 15:28:46.952 30845 32178 I okhttp.OkHttpClient: content-type: application/json
03-09 15:28:46.952 30845 32178 I okhttp.OkHttpClient: {"success":true}
03-09 15:28:46.952 30845 32178 I okhttp.OkHttpClient: <-- END HTTP (508ms, 16-byte body)"#;

        let events = parse_okhttp_logcat(logcat);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].method.as_deref(), Some("POST"));
        assert_eq!(events[0].url, "https://api.example.com/login");
        assert_eq!(events[0].status, Some(200));
        assert_eq!(events[0].duration_ms, Some(507));
        assert_eq!(
            events[0].response_body.as_deref(),
            Some(r#"{"success":true}"#)
        );
    }

    #[test]
    fn test_parse_okhttp_error() {
        let logcat = r#"I okhttp.OkHttpClient: --> POST https://api.example.com/save
I okhttp.OkHttpClient: --> END POST (50-byte body)
I okhttp.OkHttpClient: <-- 400 https://api.example.com/save (218ms)
I okhttp.OkHttpClient: {"statusCode":400,"message":"Invalid date"}
I okhttp.OkHttpClient: <-- END HTTP (219ms, 42-byte body)"#;

        let events = parse_okhttp_logcat(logcat);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].status, Some(400));
        assert_eq!(events[0].is_error(), Some(true));
        assert!(events[0]
            .response_body
            .as_deref()
            .unwrap()
            .contains("Invalid date"));
    }

    #[test]
    fn test_parse_okhttp_multiple_requests() {
        let logcat = r#"I okhttp.OkHttpClient: --> PUT https://api.example.com/a
I okhttp.OkHttpClient: --> END PUT (0-byte body)
I okhttp.OkHttpClient: --> POST https://api.example.com/b
I okhttp.OkHttpClient: --> END POST (100-byte body)
I okhttp.OkHttpClient: <-- 200 https://api.example.com/a (100ms)
I okhttp.OkHttpClient: <-- END HTTP (101ms, 0-byte body)
I okhttp.OkHttpClient: <-- 200 https://api.example.com/b (200ms)
I okhttp.OkHttpClient: {"ok":true}
I okhttp.OkHttpClient: <-- END HTTP (201ms, 11-byte body)"#;

        let events = parse_okhttp_logcat(logcat);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].url, "https://api.example.com/a");
        assert_eq!(events[1].url, "https://api.example.com/b");
    }

    #[test]
    fn test_parse_empty_logcat() {
        assert!(parse_okhttp_logcat("").is_empty());
        assert!(parse_okhttp_logcat("random noise\nmore noise").is_empty());
    }

    #[test]
    fn test_is_header_line() {
        assert!(is_header_line("Content-Type: application/json"));
        assert!(is_header_line("Authorization: Bearer abc"));
        assert!(!is_header_line("{\"json\":true}"));
        assert!(!is_header_line("random text"));
    }

    #[test]
    fn test_extract_duration_ms() {
        assert_eq!(
            extract_duration_ms("<-- 200 https://api.example.com (507ms)"),
            Some(507)
        );
        // A line with no duration did not take 0ms; it was never timed.
        assert_eq!(extract_duration_ms("no duration here"), None);
    }
}
