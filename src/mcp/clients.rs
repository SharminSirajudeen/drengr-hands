//! MCP-client matrix + config JSON + merge-write, shared by the onboarding
//! wizard and (the merge helper) by `drengr setup`. Single source of truth for
//! "which AI hosts can run Drengr, over what transport, and where their config
//! lives". Transports verified live (June 2026): Android Studio is HTTP-only;
//! Antigravity + Xcode 26.3 are stdio; the rest are stdio.

use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq)]
pub enum Wire {
    Stdio,
    /// Streamable HTTP — Drengr must run `drengr mcp --http --port <n>`.
    Http(u16),
}

#[derive(Clone)]
pub struct Client {
    pub key: &'static str,
    pub name: &'static str,
    pub wire: Wire,
    /// `Some` → we can merge-write this file. `None` → project-local or an
    /// unverified target, so we only print the snippet.
    pub path: Option<PathBuf>,
    /// Extra one-line guidance shown after wiring (HTTP note, Xcode verify).
    pub note: Option<String>,
    /// Xcode's agent runs a restricted shell — emit the absolute drengr path
    /// and an explicit `env.PATH` so a bare `drengr`/`xcrun` resolves.
    pub abs_cmd: bool,
    /// If the file write fails, register via this host's own CLI instead
    /// (argv incl. the binary, e.g. `["claude","mcp","add","--scope","user",…]`).
    pub cli_fallback: Option<Vec<String>>,
}

impl Client {
    pub fn writable(&self) -> bool {
        self.path.is_some()
    }

    /// The `{"mcpServers": {"drengr": …}}` snippet for this host.
    pub fn config_json(&self, android_home: Option<&str>) -> String {
        match self.wire {
            Wire::Http(port) => format!(
                "{{\n  \"mcpServers\": {{\n    \"drengr\": {{\n      \"httpUrl\": \"http://localhost:{port}/mcp\"\n    }}\n  }}\n}}"
            ),
            Wire::Stdio => {
                let servers_key = if self.key == "vscode" { "servers" } else { "mcpServers" };
                // Some hosts' docs require an explicit transport type.
                let type_field = if matches!(self.key, "vscode" | "claude-code" | "xcode") {
                    ",\n      \"type\": \"stdio\""
                } else {
                    ""
                };
                let command = if self.abs_cmd { drengr_exe() } else { "drengr".to_string() };
                // env: PATH (restricted-shell hosts) + ANDROID_HOME if set.
                let mut env: Vec<(&str, String)> = Vec::new();
                if self.abs_cmd {
                    env.push(("PATH", abs_path_env()));
                }
                if let Some(h) = android_home {
                    env.push(("ANDROID_HOME", h.to_string()));
                }
                let env_block = if env.is_empty() {
                    String::new()
                } else {
                    let inner = env
                        .iter()
                        .map(|(k, v)| {
                            format!(
                                "        {}: {}",
                                serde_json::to_string(k).unwrap_or_default(),
                                serde_json::to_string(v).unwrap_or_default()
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(",\n");
                    format!(",\n      \"env\": {{\n{inner}\n      }}")
                };
                format!(
                    "{{\n  \"{servers_key}\": {{\n    \"drengr\": {{\n      \"command\": {},\n      \"args\": [\"mcp\"]{type_field}{env_block}\n    }}\n  }}\n}}",
                    serde_json::to_string(&command).unwrap_or_else(|_| "\"drengr\"".into())
                )
            }
        }
    }

    /// True if this host's config file already contains a `drengr` server entry
    /// (used to detect an already-set-up machine). Print-only hosts → false.
    pub fn has_drengr(&self) -> bool {
        let Some(path) = &self.path else { return false };
        let Ok(content) = std::fs::read_to_string(path) else {
            return false;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) else {
            return false;
        };
        let key = if self.key == "vscode" {
            "servers"
        } else {
            "mcpServers"
        };
        v.get(key).and_then(|s| s.get("drengr")).is_some()
    }

    /// Best-effort "is this host installed on the machine?" for pre-selection.
    pub fn installed(&self) -> bool {
        if self.path.as_ref().map(|p| p.exists()).unwrap_or(false) {
            return true;
        }
        let home = dirs::home_dir().unwrap_or_default();
        let app = |name: &str| Path::new("/Applications").join(name).exists();
        let on_path = |bin: &str| {
            std::process::Command::new("command")
                .args(["-v", bin])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        };
        match self.key {
            "claude-desktop" => app("Claude.app"),
            "claude-code" => on_path("claude") || home.join(".claude.json").exists(),
            "cursor" => app("Cursor.app") || home.join(".cursor").exists(),
            "windsurf" => app("Windsurf.app") || home.join(".codeium").exists(),
            "vscode" => app("Visual Studio Code.app") || on_path("code"),
            "antigravity" => app("Antigravity.app") || home.join(".gemini").exists(),
            "xcode" => app("Xcode.app") || on_path("xcrun"),
            _ => false,
        }
    }
}

/// Absolute path to the running drengr binary (for hosts whose restricted
/// shell can't resolve a bare `drengr` on PATH).
fn drengr_exe() -> String {
    std::env::current_exe()
        .ok()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "drengr".to_string())
}

/// PATH value for restricted-shell hosts: the standard bin dirs plus the
/// drengr binary's own directory.
fn abs_path_env() -> String {
    const BASE: &str = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin";
    match std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_string_lossy().to_string()))
    {
        Some(dir) if !dir.is_empty() && !BASE.split(':').any(|p| p == dir) => {
            format!("{dir}:{BASE}")
        }
        _ => BASE.to_string(),
    }
}

/// Xcode 26.3+ Claude Agent custom-MCP config — only when the agent dir
/// exists (i.e. the user has set up the Coding Assistant). macOS-only.
fn xcode_agent_config(home: &Path) -> Option<PathBuf> {
    let dir = home.join("Library/Developer/Xcode/CodingAssistant/ClaudeAgentConfig");
    dir.is_dir().then(|| dir.join(".claude.json"))
}

/// User-global VS Code MCP config (distinct from a workspace `.vscode/mcp.json`).
fn vscode_user_config(home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Code/User/mcp.json")
    } else {
        home.join(".config/Code/User/mcp.json")
    }
}

/// Newest Android Studio config dir — `mcp.json` lives there.
fn android_studio_config(home: &Path) -> Option<PathBuf> {
    let base = if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Google")
    } else {
        home.join(".config/Google")
    };
    let mut dirs: Vec<_> = std::fs::read_dir(&base)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with("AndroidStudio"))
                    .unwrap_or(false)
        })
        .collect();
    dirs.sort();
    dirs.pop().map(|d| d.join("mcp.json"))
}

/// The full client matrix, with resolved paths for this machine.
pub fn all(home: &Path, http_port: u16) -> Vec<Client> {
    let claude_desktop = if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Claude/claude_desktop_config.json")
    } else {
        home.join(".config/claude/claude_desktop_config.json")
    };
    vec![
        Client { key: "claude-desktop", name: "Claude Desktop", wire: Wire::Stdio,
            path: Some(claude_desktop), note: None, abs_cmd: false, cli_fallback: None },
        // ~/.claude.json (top-level mcpServers = user scope). Atomic merge
        // preserves projects/auth keys; falls back to the CLI on any write error.
        Client { key: "claude-code", name: "Claude Code", wire: Wire::Stdio,
            path: Some(home.join(".claude.json")), note: None, abs_cmd: false,
            cli_fallback: Some(vec![
                "claude".into(), "mcp".into(), "add".into(), "--scope".into(),
                "user".into(), "drengr".into(), "--".into(), "drengr".into(), "mcp".into(),
            ]) },
        Client { key: "cursor", name: "Cursor", wire: Wire::Stdio,
            path: Some(home.join(".cursor/mcp.json")), note: None, abs_cmd: false, cli_fallback: None },
        Client { key: "windsurf", name: "Windsurf", wire: Wire::Stdio,
            path: Some(home.join(".codeium/windsurf/mcp_config.json")), note: None,
            abs_cmd: false, cli_fallback: None },
        Client { key: "vscode", name: "VS Code", wire: Wire::Stdio,
            path: Some(vscode_user_config(home)), note: None, abs_cmd: false, cli_fallback: None },
        Client { key: "android-studio", name: "Android Studio", wire: Wire::Http(http_port),
            path: android_studio_config(home),
            note: Some(format!("Android Studio ▸ Settings ▸ Tools ▸ AI ▸ MCP Servers (JSON view); keep `drengr mcp --http{}` running",
                if http_port == 7878 { String::new() } else { format!(" --port {http_port}") })),
            abs_cmd: false, cli_fallback: None },
        Client { key: "antigravity", name: "Antigravity", wire: Wire::Stdio,
            path: Some(home.join(".gemini/config/mcp_config.json")), note: None,
            abs_cmd: false, cli_fallback: None },
        // Xcode auto-writes only when the agent dir exists; restricted shell
        // needs the absolute command + PATH. Verify note covers the residual
        // path-format uncertainty (medium-high confidence).
        Client { key: "xcode", name: "Xcode", wire: Wire::Stdio,
            path: xcode_agent_config(home),
            note: Some("Xcode 26.3+ — restart, then check `/context` in the Agent panel. If drengr isn't listed, add it in Settings ▸ Intelligence ▸ MCP.".into()),
            abs_cmd: true, cli_fallback: None },
    ]
}

/// Register via a host's own MCP-add CLI (argv incl. the binary). Returns true
/// only if the command runs and exits 0 — used as the fallback when a direct
/// config write fails (e.g. Claude Code → `claude mcp add --scope user …`).
pub fn run_cli_fallback(argv: &[String]) -> bool {
    let Some((bin, args)) = argv.split_first() else {
        return false;
    };
    std::process::Command::new(bin)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Merge the drengr server into an existing config file (preserving other
/// servers), or create it. Atomic (temp + rename) so concurrent readers never
/// see a partial file.
pub fn write_merge(path: &Path, config_json: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let existing: serde_json::Value = if path.exists() {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap_or_default())
            .unwrap_or(serde_json::json!({}))
    } else {
        serde_json::json!({})
    };
    let incoming: serde_json::Value =
        serde_json::from_str(config_json).unwrap_or(serde_json::json!({}));
    let mut merged = existing;
    // A non-object top level (corrupt / array / scalar) can't be merged into —
    // start clean so we never silently drop the drengr entry.
    if !merged.is_object() {
        merged = serde_json::json!({});
    }
    if let (Some(m), Some(n)) = (merged.as_object_mut(), incoming.as_object()) {
        for (key, val) in n {
            match m.get_mut(key) {
                // Both objects → merge our server in, preserving siblings.
                Some(section) if section.is_object() && val.is_object() => {
                    let es = section.as_object_mut().unwrap();
                    for (k, v) in val.as_object().unwrap() {
                        es.insert(k.clone(), v.clone());
                    }
                }
                // Missing, or an existing section of the wrong type (malformed
                // mcpServers) → set it outright rather than silently no-op.
                _ => {
                    m.insert(key.clone(), val.clone());
                }
            }
        }
    }
    // Atomic write: temp file + rename, so a concurrent reader (e.g. a running
    // Claude Code editing ~/.claude.json) never sees a partial/corrupt file.
    let pretty = serde_json::to_string_pretty(&merged).unwrap_or_default() + "\n";
    let tmp = match path.file_name() {
        Some(name) => path.with_file_name(format!(
            "{}.{}.drengr-tmp",
            name.to_string_lossy(),
            std::process::id()
        )),
        None => return std::fs::write(path, pretty),
    };
    std::fs::write(&tmp, pretty.as_bytes())?;
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrix_has_all_hosts_and_transports() {
        let home = PathBuf::from("/home/x"); // nonexistent → Xcode agent dir absent
        let clients = all(&home, 7878);
        assert_eq!(clients.len(), 8);
        let as_ = clients.iter().find(|c| c.key == "android-studio").unwrap();
        assert!(matches!(as_.wire, Wire::Http(7878)));
        // Cursor / Claude Code / VS Code are now auto-write (global config).
        for k in [
            "cursor",
            "claude-code",
            "vscode",
            "antigravity",
            "claude-desktop",
            "windsurf",
        ] {
            assert!(
                clients.iter().find(|c| c.key == k).unwrap().writable(),
                "{k} should be writable"
            );
        }
        // Claude Code carries a CLI fallback for write failures.
        assert!(clients
            .iter()
            .find(|c| c.key == "claude-code")
            .unwrap()
            .cli_fallback
            .is_some());
        // Xcode auto-writes only when its agent dir exists; absent here → print.
        let xcode = clients.iter().find(|c| c.key == "xcode").unwrap();
        assert!(!xcode.writable() && xcode.abs_cmd);
    }

    #[test]
    fn xcode_config_uses_absolute_command_and_path_env() {
        let xcode = Client {
            key: "xcode",
            name: "Xcode",
            wire: Wire::Stdio,
            path: None,
            note: None,
            abs_cmd: true,
            cli_fallback: None,
        };
        let json = xcode.config_json(None);
        assert!(json.contains("\"type\": \"stdio\""));
        assert!(json.contains("\"PATH\""), "abs_cmd must emit env.PATH");
        // command must be an absolute path (or at least not the bare "drengr").
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let cmd = v["mcpServers"]["drengr"]["command"].as_str().unwrap();
        assert!(cmd.contains('/') || cmd == "drengr"); // abs path, or bare if current_exe failed
    }

    #[test]
    fn vscode_uses_servers_key_with_type() {
        let c = all(&PathBuf::from("/h"), 7878)
            .into_iter()
            .find(|c| c.key == "vscode")
            .unwrap();
        let json = c.config_json(None);
        assert!(json.contains("\"servers\"") && json.contains("\"type\": \"stdio\""));
        assert!(!json.contains("\"mcpServers\""));
    }

    #[test]
    fn stdio_json_includes_android_home_when_present() {
        let c = all(&PathBuf::from("/h"), 7878)
            .into_iter()
            .find(|c| c.key == "claude-desktop")
            .unwrap();
        let json = c.config_json(Some("/sdk"));
        assert!(json.contains("\"command\": \"drengr\"") && json.contains("ANDROID_HOME"));
        assert!(c.config_json(None).contains("\"args\": [\"mcp\"]"));
        assert!(!c.config_json(None).contains("ANDROID_HOME"));
    }

    #[test]
    fn http_json_uses_httpurl() {
        let c = all(&PathBuf::from("/h"), 9000)
            .into_iter()
            .find(|c| c.key == "android-studio")
            .unwrap();
        let json = c.config_json(None);
        assert!(json.contains("\"httpUrl\": \"http://localhost:9000/mcp\""));
    }

    #[test]
    fn write_merge_preserves_other_servers() {
        let dir = std::env::temp_dir().join(format!("drengr-cltest-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mcp.json");
        std::fs::write(&path, r#"{"mcpServers":{"other":{"command":"x"}}}"#).unwrap();
        write_merge(
            &path,
            "{\"mcpServers\":{\"drengr\":{\"command\":\"drengr\",\"args\":[\"mcp\"]}}}",
        )
        .unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["mcpServers"]["other"]["command"], "x");
        assert_eq!(v["mcpServers"]["drengr"]["command"], "drengr");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_merge_recovers_from_malformed_section() {
        // An existing mcpServers of the wrong type must not silently drop drengr.
        let dir = std::env::temp_dir().join(format!("drengr-cltest-mal-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mcp.json");
        std::fs::write(&path, r#"{"mcpServers":"oops"}"#).unwrap();
        write_merge(
            &path,
            "{\"mcpServers\":{\"drengr\":{\"command\":\"drengr\"}}}",
        )
        .unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["mcpServers"]["drengr"]["command"], "drengr");
        // And a non-object top level is replaced rather than left to drop drengr.
        std::fs::write(&path, r#"["not","an","object"]"#).unwrap();
        write_merge(
            &path,
            "{\"mcpServers\":{\"drengr\":{\"command\":\"drengr\"}}}",
        )
        .unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["mcpServers"]["drengr"]["command"], "drengr");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
