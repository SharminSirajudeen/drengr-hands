//! Single-shot CLI bridge to the Drengr tools.
//!
//! `drengr look` / `drengr do` / `drengr query` give any tool-calling agent
//! Drengr's eyes and hands over plain shell — no MCP server to register, no
//! host restart, no LLM key. They call the exact same
//! [`McpHandlers::dispatch`] path the MCP server uses, so behaviour is
//! identical; the calling agent is the brain.
//!
//! This is the no-restart door into Drengr: an agent that can only shell out
//! (or hasn't restarted to pick up the MCP server) reaches `drengr look` /
//! `drengr do` instead of falling back to hand-rolled `adb`.

use super::handlers::McpHandlers;
use super::tools::ToolResult;
use base64::prelude::{Engine as _, BASE64_STANDARD};
use serde_json::{json, Value};
use std::path::PathBuf;

/// Where look/do screenshots land so an agent can `Read` a small file instead
/// of pulling base64 through its context.
fn frame_dir() -> PathBuf {
    let base = crate::paths::drengr_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("cli");
    let _ = std::fs::create_dir_all(&base);
    base
}

/// Run one Drengr tool from the CLI and print the result. Returns the process
/// exit code (0 = ok, 1 = tool error).
///
/// `auto_connect` runs `drengr_query(connect)` first so `look`/`do` work
/// against the detected device without a separate setup step — exactly what an
/// MCP agent does on its first call. `query` provisions itself, so it passes
/// `false`.
pub async fn run_tool(tool: &str, args: Value, auto_connect: bool) -> i32 {
    let handlers = McpHandlers::new();

    if auto_connect {
        // Attach to the requested device (or the default if none) so the
        // following look/do resolves a transport. Forward --device so connect
        // creates the same one resolve_transport will look up. A real failure
        // surfaces on the look/do call below with an actionable message.
        let mut connect_args = json!({ "question": "connect" });
        if let Some(d) = args.get("device") {
            connect_args["device"] = d.clone();
        }
        let _ = handlers.dispatch("drengr_query", connect_args).await;
    }

    let result = handlers.dispatch(tool, args).await;
    render(&result, tool);
    // main exits via std::process::exit, which skips the implicit flush. When
    // stdout is piped it is block-buffered, so flush or the output is lost.
    let _ = std::io::Write::flush(&mut std::io::stdout());
    if result.is_error.unwrap_or(false) {
        1
    } else {
        0
    }
}

/// Print text content to stdout; write any image to `~/.drengr/cli/<verb>.<ext>`
/// and print its path. Keeping the frame on disk (not base64 in stdout) is the
/// whole point — the agent `Read`s a small file rather than bloating context.
fn render(result: &ToolResult, tool: &str) {
    let verb = tool.trim_start_matches("drengr_");
    let mut frame_idx = 0usize;
    for c in &result.content {
        match c.content_type.as_str() {
            "text" => {
                if let Some(t) = &c.text {
                    println!("{}", t);
                }
            }
            "image" => {
                if let Some(b64) = &c.data {
                    if let Ok(bytes) = BASE64_STANDARD.decode(b64) {
                        let ext = if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
                            "png"
                        } else {
                            "jpg"
                        };
                        let name = if frame_idx == 0 {
                            format!("{verb}.{ext}")
                        } else {
                            format!("{verb}-{frame_idx}.{ext}")
                        };
                        let path = frame_dir().join(name);
                        if std::fs::write(&path, &bytes).is_ok() {
                            println!("[screenshot] {}", path.display());
                        }
                        frame_idx += 1;
                    }
                }
            }
            _ => {}
        }
    }
}
