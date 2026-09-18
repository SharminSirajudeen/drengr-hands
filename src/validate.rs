/// Validate device ID — allows alphanumeric, hyphens, dots, colons, underscores.
/// Rejects path traversal, shell metacharacters, and flag-like strings.
pub fn is_valid_device_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && !id.starts_with('-')
        && !id.contains("..")
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' || b == b':')
}

/// Validate that a string is a safe package name.
/// Allows alphanumeric chars, dots, underscores, and hyphens.
/// Rejects empty strings, strings over 256 bytes, path traversal (`..`), and shell metacharacters.
pub fn is_valid_package_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 256
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
        && !name.contains("..")
}

/// Strict allow-list for tokens interpolated into an NSPredicate string
/// (`log show --predicate ...`). Single-quote stripping alone is not enough —
/// the predicate grammar accepts operators (`==`, `&&`, `OR`, `LIKE`) that
/// can change query semantics. Only `[A-Za-z0-9._:/-]{1,200}` is permitted.
pub fn is_valid_predicate_token(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 200
        && s.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || b == b'.'
                || b == b'_'
                || b == b':'
                || b == b'/'
                || b == b'-'
        })
}

/// Shell metacharacters that must be rejected in user input. Parens are
/// deliberately excluded — they're common in real filenames (e.g. Gradle's
/// `app-6.8.9(578).apk`), harmless on exec-arg paths, and literal inside the
/// single-quoted logcat filter. The quote-breaking chars (`'`, `;`, `$`, …) stay.
const SHELL_CHARS: &[char] = &[
    ';', '\'', '"', '`', '$', '|', '&', '>', '<', '\\', '{', '}', '!', '#',
];

/// Validate a file path: must exist, have the correct extension, and contain no traversal.
/// Returns the canonical (resolved) path or an error.
pub fn validate_file_path(path: &str, allowed_ext: &str) -> Result<String, String> {
    if path.is_empty() {
        return Err("File path is empty".to_string());
    }
    if path.len() > 4096 {
        return Err("File path too long".to_string());
    }

    // Reject path traversal
    if path.contains("..") {
        return Err("Path traversal (..) not allowed".to_string());
    }

    // Reject shell metacharacters in path
    if path.chars().any(|c| SHELL_CHARS.contains(&c)) {
        return Err("Path contains invalid characters".to_string());
    }

    // Check extension
    if !path.ends_with(allowed_ext) {
        return Err(format!(
            "Invalid file extension. Expected '{}', got: {}",
            allowed_ext, path
        ));
    }

    // Check file exists
    let path_buf = std::path::Path::new(path);
    if !path_buf.exists() {
        return Err(format!("File does not exist: {}", path));
    }

    // Reject symlinks (could point outside allowed directories)
    if path_buf.is_symlink() {
        return Err("Symbolic links not allowed".to_string());
    }

    // Return canonical path
    path_buf
        .canonicalize()
        .map(|p| p.to_string_lossy().to_string())
        .map_err(|e| format!("Cannot resolve path: {}", e))
}

/// Validate JSON conditions for the assert query.
/// Input must be a JSON array, each item must have a "text" field, total size < 10KB.
pub fn validate_assert_conditions(json_str: &str) -> Result<Vec<serde_json::Value>, String> {
    if json_str.len() > 10_240 {
        return Err(format!(
            "Assert conditions too large: {} bytes (max 10240)",
            json_str.len()
        ));
    }

    let parsed: serde_json::Value =
        serde_json::from_str(json_str).map_err(|e| format!("Invalid JSON: {}", e))?;

    let array = parsed
        .as_array()
        .ok_or_else(|| "Conditions must be a JSON array".to_string())?;

    if array.is_empty() {
        return Err("Conditions array is empty".to_string());
    }

    if array.len() > 50 {
        return Err(format!("Too many conditions: {} (max 50)", array.len()));
    }

    for (i, item) in array.iter().enumerate() {
        if !item.is_object() {
            return Err(format!("Condition {} must be an object", i));
        }
        if item.get("text").is_none() {
            return Err(format!("Condition {} missing required 'text' field", i));
        }
    }

    Ok(array.clone())
}

/// Sanitize logcat filter string.
/// Rejects shell metacharacters and enforces max length of 200 chars.
pub fn sanitize_logcat_filter(filter: &str) -> Result<String, String> {
    if filter.is_empty() {
        return Err("Logcat filter is empty".to_string());
    }
    if filter.len() > 200 {
        return Err(format!(
            "Logcat filter too long: {} chars (max 200)",
            filter.len()
        ));
    }
    if filter.chars().any(|c| SHELL_CHARS.contains(&c)) {
        return Err("Logcat filter contains shell metacharacters".to_string());
    }
    // Strip control characters
    let sanitized: String = filter.chars().filter(|c| !c.is_control()).collect();
    Ok(sanitized)
}

/// Validate a URL before passing it to `open_url`/`deep_link`.
/// Deny-list of dangerous schemes (intent://, javascript:, file:, etc.) so
/// arbitrary app deep links (`myapp://...`) still work without configuration.
pub fn validate_url(url: &str) -> Result<(), String> {
    if url.is_empty() {
        return Err("URL is empty".to_string());
    }
    if url.len() > 2048 {
        return Err(format!("URL too long: {} bytes (max 2048)", url.len()));
    }
    if url.chars().any(|c| c.is_control()) {
        return Err("URL contains control characters".to_string());
    }

    let scheme = match url.split_once(':') {
        Some((s, _)) => s,
        None => return Err("URL missing scheme (expected scheme:...)".to_string()),
    };
    if scheme.is_empty() {
        return Err("URL has empty scheme".to_string());
    }

    // RFC 3986 scheme grammar: ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )
    let mut chars = scheme.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_alphabetic() {
        return Err(format!(
            "Invalid scheme '{scheme}' (must start with letter)"
        ));
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.') {
        return Err(format!("Invalid scheme syntax: '{scheme}'"));
    }

    let scheme_lc = scheme.to_ascii_lowercase();
    if BLOCKED_URL_SCHEMES.contains(&scheme_lc.as_str()) {
        return Err(format!(
            "URL scheme '{scheme}' is blocked (potentially dangerous)"
        ));
    }

    Ok(())
}

/// URI schemes that can launch sensitive system functionality or execute code.
/// Always rejected by `validate_url` regardless of the rest of the URL.
const BLOCKED_URL_SCHEMES: &[&str] = &[
    "intent",      // Android: arbitrary Intent launching
    "android-app", // Android: explicit package-targeted intent
    "javascript",  // webview code execution
    "vbscript",
    "file",                      // local filesystem read
    "data",                      // embedded arbitrary content (XSS in webviews)
    "jar",                       // Java archive launching
    "content",                   // Android ContentProvider read
    "ms-settings",               // Windows: opens Settings panes
    "x-apple-systempreferences", // macOS: opens System Preferences panes
];

#[cfg(test)]
mod tests {
    use super::*;

    // --- Package name tests (existing) ---

    #[test]
    fn test_valid_packages() {
        assert!(is_valid_package_name("com.example.app"));
        assert!(is_valid_package_name("com.my_app"));
        assert!(is_valid_package_name("com.my-app"));
        assert!(is_valid_package_name("a"));
    }

    #[test]
    fn test_invalid_packages() {
        assert!(!is_valid_package_name(""));
        assert!(!is_valid_package_name("com/../etc/passwd"));
        assert!(!is_valid_package_name("com.app; rm -rf /"));
        assert!(!is_valid_package_name("com.app'"));
        assert!(!is_valid_package_name("com.app\""));
        assert!(!is_valid_package_name("com.app`id`"));
        assert!(!is_valid_package_name("com.app$(cmd)"));
        assert!(!is_valid_package_name(&"a".repeat(257)));
    }

    #[test]
    fn test_path_traversal() {
        assert!(!is_valid_package_name(".."));
        assert!(!is_valid_package_name("com..app"));
    }

    #[test]
    fn predicate_token_accepts_typical_filters() {
        assert!(is_valid_predicate_token("login"));
        assert!(is_valid_predicate_token("auth.failed"));
        assert!(is_valid_predicate_token("net:request"));
        assert!(is_valid_predicate_token("HTTP/1.1"));
        assert!(is_valid_predicate_token("a"));
        assert!(is_valid_predicate_token(&"x".repeat(200)));
    }

    #[test]
    fn predicate_token_rejects_injection() {
        assert!(!is_valid_predicate_token(""));
        assert!(!is_valid_predicate_token(&"x".repeat(201)));
        assert!(!is_valid_predicate_token("foo' OR 1=1 --"));
        assert!(!is_valid_predicate_token("foo OR bar"));
        assert!(!is_valid_predicate_token("foo && bar"));
        assert!(!is_valid_predicate_token("foo == 'bar'"));
        assert!(!is_valid_predicate_token("foo LIKE '%pwn%'"));
        assert!(!is_valid_predicate_token("foo bar")); // space
        assert!(!is_valid_predicate_token("foo$(cmd)"));
    }

    // --- Text input sanitization ---

    // --- File path validation ---

    #[test]
    fn test_file_path_rejects_traversal() {
        assert!(validate_file_path("../etc/passwd.png", ".png").is_err());
        assert!(validate_file_path("/tmp/../../etc/shadow.apk", ".apk").is_err());
    }

    #[test]
    fn test_file_path_rejects_wrong_extension() {
        assert!(validate_file_path("/tmp/test.jpg", ".png").is_err());
        assert!(validate_file_path("/tmp/test.png", ".apk").is_err());
    }

    #[test]
    fn test_file_path_rejects_shell_chars() {
        assert!(validate_file_path("/tmp/test;rm.png", ".png").is_err());
        assert!(validate_file_path("/tmp/test$(cmd).png", ".png").is_err());
    }

    #[test]
    fn test_file_path_rejects_nonexistent() {
        assert!(validate_file_path("/nonexistent/path/file.png", ".png").is_err());
    }

    #[test]
    fn test_file_path_rejects_empty() {
        assert!(validate_file_path("", ".png").is_err());
    }

    // --- Assert conditions validation ---

    #[test]
    fn test_assert_conditions_valid() {
        let json = r#"[{"text": "Login", "visible": true}]"#;
        let result = validate_assert_conditions(json);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 1);
    }

    #[test]
    fn test_assert_conditions_multiple() {
        let json = r#"[{"text": "A"}, {"text": "B", "type": "Button"}]"#;
        let result = validate_assert_conditions(json);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 2);
    }

    #[test]
    fn test_assert_conditions_rejects_invalid_json() {
        assert!(validate_assert_conditions("not json").is_err());
    }

    #[test]
    fn test_assert_conditions_rejects_non_array() {
        assert!(validate_assert_conditions(r#"{"text": "Login"}"#).is_err());
    }

    #[test]
    fn test_assert_conditions_rejects_missing_text() {
        assert!(validate_assert_conditions(r#"[{"visible": true}]"#).is_err());
    }

    #[test]
    fn test_assert_conditions_rejects_empty_array() {
        assert!(validate_assert_conditions("[]").is_err());
    }

    #[test]
    fn test_assert_conditions_rejects_oversized() {
        let big = format!("[{}]", r#"{"text":"x"},"#.repeat(1000));
        assert!(validate_assert_conditions(&big).is_err());
    }

    #[test]
    fn test_assert_conditions_rejects_too_many() {
        let items: Vec<String> = (0..51)
            .map(|i| format!(r#"{{"text":"item{}"}}"#, i))
            .collect();
        let json = format!("[{}]", items.join(","));
        assert!(validate_assert_conditions(&json).is_err());
    }

    // --- Logcat filter sanitization ---

    #[test]
    fn test_logcat_filter_valid() {
        assert_eq!(sanitize_logcat_filter("CHHHECK").unwrap(), "CHHHECK");
        assert_eq!(sanitize_logcat_filter("OkHttp").unwrap(), "OkHttp");
    }

    #[test]
    fn test_logcat_filter_rejects_shell_chars() {
        assert!(sanitize_logcat_filter("tag; rm -rf /").is_err());
        assert!(sanitize_logcat_filter("tag`id`").is_err());
        assert!(sanitize_logcat_filter("tag$(cmd)").is_err());
    }

    #[test]
    fn test_logcat_filter_rejects_too_long() {
        assert!(sanitize_logcat_filter(&"a".repeat(201)).is_err());
    }

    #[test]
    fn test_logcat_filter_rejects_empty() {
        assert!(sanitize_logcat_filter("").is_err());
    }

    // --- URL filter sanitization ---

    // --- validate_url ---

    #[test]
    fn test_validate_url_allows_http_https() {
        assert!(validate_url("http://example.com").is_ok());
        assert!(validate_url("https://example.com/path?q=1#frag").is_ok());
    }

    #[test]
    fn test_validate_url_allows_app_deep_links() {
        assert!(validate_url("myapp://settings/account").is_ok());
        assert!(validate_url("com.example.app://open").is_ok());
    }

    #[test]
    fn test_validate_url_blocks_android_intent_scheme() {
        // Audit C1: prompt-injected URL launching arbitrary Android intents.
        assert!(validate_url("intent://settings#Intent;package=com.victim;end").is_err());
        assert!(validate_url("intent:#Intent;action=android.intent.action.DELETE;end").is_err());
        assert!(validate_url("INTENT://uppercase-still-blocked").is_err());
    }

    #[test]
    fn test_validate_url_blocks_dangerous_schemes() {
        for s in &[
            "javascript:alert(1)",
            "file:///etc/passwd",
            "data:text/html,<script>alert(1)</script>",
            "vbscript:msgbox",
            "jar:http://evil/x.jar!/",
            "content://com.android.providers.contacts/contacts",
            "android-app://com.victim",
            "ms-settings:network",
            "x-apple-systempreferences:com.apple.preference.network",
        ] {
            assert!(validate_url(s).is_err(), "should block: {s}");
        }
    }

    #[test]
    fn test_validate_url_rejects_missing_scheme() {
        assert!(validate_url("no-scheme.example.com/path").is_err());
        assert!(validate_url("/just/a/path").is_err());
        assert!(validate_url("").is_err());
    }

    #[test]
    fn test_validate_url_rejects_invalid_scheme_grammar() {
        assert!(validate_url("1http://example.com").is_err());
        assert!(validate_url("my_app://x").is_err());
        assert!(validate_url("my app://x").is_err());
    }

    #[test]
    fn test_validate_url_rejects_control_characters() {
        assert!(validate_url("https://example.com/\nrm -rf").is_err());
        assert!(validate_url("https://example.com/\0").is_err());
        assert!(validate_url("https://example.com/\tx").is_err());
    }

    #[test]
    fn test_validate_url_length_cap() {
        let long = format!("https://example.com/{}", "a".repeat(2025));
        assert!(validate_url(&long).is_ok());
        let too_long = format!("https://example.com/{}", "a".repeat(2050));
        assert!(validate_url(&too_long).is_err());
    }

    #[test]
    fn test_validate_url_scheme_only_first_colon_split() {
        // URL with multiple colons (port number) — only first colon splits scheme.
        assert!(validate_url("https://example.com:8080/path").is_ok());
        assert!(validate_url("myapp://path?q=a:b").is_ok());
    }

    // --- Shell injection in package names ---

    #[test]
    fn test_valid_package_name_rejects_shell_injection() {
        assert!(!is_valid_package_name("com.app;rm -rf /"));
        assert!(!is_valid_package_name("com.app&& echo pwned"));
        assert!(!is_valid_package_name("com.app|cat /etc/passwd"));
    }

    #[test]
    fn test_valid_package_name_rejects_single_quotes() {
        assert!(!is_valid_package_name("com.app'"));
        assert!(!is_valid_package_name("com.app'--"));
    }

    #[test]
    fn test_valid_package_name_accepts_ios_bundle_ids() {
        assert!(is_valid_package_name("com.apple.mobilesafari"));
        assert!(is_valid_package_name("com.apple.Preferences"));
        assert!(is_valid_package_name("io.flutter.demo"));
        assert!(is_valid_package_name("dev.drengr.test-app"));
    }

    // --- Logcat filter boundary ---

    #[test]
    fn test_logcat_filter_max_length_boundary() {
        assert!(sanitize_logcat_filter(&"a".repeat(200)).is_ok());
        assert!(sanitize_logcat_filter(&"a".repeat(201)).is_err());
        assert!(sanitize_logcat_filter(&"a".repeat(257)).is_err());
    }

    // --- File path extension validation ---

    #[test]
    fn test_file_path_validates_apk_extension() {
        let dir = tempfile::tempdir().unwrap();
        let apk = dir.path().join("test.apk");
        std::fs::write(&apk, b"fake").unwrap();
        assert!(validate_file_path(apk.to_str().unwrap(), ".apk").is_ok());

        let exe = dir.path().join("test.exe");
        std::fs::write(&exe, b"fake").unwrap();
        assert!(validate_file_path(exe.to_str().unwrap(), ".apk").is_err());
    }

    #[test]
    fn test_file_path_validates_ipa_extension() {
        let dir = tempfile::tempdir().unwrap();
        let ipa = dir.path().join("test.ipa");
        std::fs::write(&ipa, b"fake").unwrap();
        assert!(validate_file_path(ipa.to_str().unwrap(), ".ipa").is_ok());
    }

    #[test]
    fn test_file_path_validates_app_extension() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("MyApp.app");
        std::fs::write(&app, b"fake").unwrap();
        assert!(validate_file_path(app.to_str().unwrap(), ".app").is_ok());
    }
}
