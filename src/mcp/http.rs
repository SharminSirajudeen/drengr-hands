//! Streamable HTTP transport — one POST endpoint in front of `handle_request`,
//! for MCP hosts that can't spawn stdio servers (e.g. Android Studio).
//!
//! Sessionless by design: this server has one device session per process, so
//! no Mcp-Session-Id is issued. GET (server-initiated stream) returns 405,
//! which the spec allows for servers that don't push.

use std::net::SocketAddr;
use std::sync::Arc;

use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;

use super::{handle_request, JsonRpcRequest, McpHandlers};

const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

/// Bind 127.0.0.1:`port` and serve forever. Port 0 picks an ephemeral port.
pub async fn run_http_server(handlers: Arc<McpHandlers>, port: u16) -> anyhow::Result<()> {
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?;
    eprintln!();
    eprintln!("  Drengr MCP — streamable HTTP on http://{bound}/mcp");
    eprintln!("  Keep this running while your MCP client is connected. Ctrl-C to stop.");
    eprintln!();
    serve(listener, handlers).await
}

/// Accept loop, split from the bind so tests can use an ephemeral port.
pub async fn serve(
    listener: tokio::net::TcpListener,
    handlers: Arc<McpHandlers>,
) -> anyhow::Result<()> {
    // Serialize dispatch — concurrent POSTs keep stdio's sequential semantics
    // (one device, one action at a time).
    let gate = Arc::new(tokio::sync::Mutex::new(()));
    loop {
        let (stream, _) = listener.accept().await?;
        let io = TokioIo::new(stream);
        let handlers = handlers.clone();
        let gate = gate.clone();
        tokio::spawn(async move {
            let svc = service_fn(move |req| route(req, handlers.clone(), gate.clone()));
            if let Err(e) = http1::Builder::new().serve_connection(io, svc).await {
                tracing::debug!("http connection ended: {e}");
            }
        });
    }
}

async fn route(
    req: Request<Incoming>,
    handlers: Arc<McpHandlers>,
    gate: Arc<tokio::sync::Mutex<()>>,
) -> Result<Response<Full<Bytes>>, std::convert::Infallible> {
    // DNS-rebinding guard: we bind loopback only, so any Origin a browser
    // attaches must itself be local.
    if let Some(origin) = req.headers().get("origin").and_then(|v| v.to_str().ok()) {
        if !origin_is_local(origin) {
            return Ok(plain(StatusCode::FORBIDDEN, "forbidden origin"));
        }
    }
    if req.uri().path() != "/mcp" {
        return Ok(plain(
            StatusCode::NOT_FOUND,
            "not found — MCP endpoint is /mcp",
        ));
    }
    if req.method() != Method::POST {
        // No server-initiated stream (GET) and no sessions to DELETE.
        return Ok(plain(StatusCode::METHOD_NOT_ALLOWED, "use POST"));
    }

    let body = match read_body_capped(req).await {
        Ok(b) => b,
        Err(resp) => return Ok(*resp),
    };
    let request: JsonRpcRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            let err = super::JsonRpcResponse::error(None, -32700, format!("Parse error: {e}"));
            let json = serde_json::to_vec(&err).unwrap_or_default();
            return Ok(json_response(StatusCode::BAD_REQUEST, json));
        }
    };

    let _serialized = gate.lock().await;
    match handle_request(request, &handlers).await {
        Some(response) => {
            let json = serde_json::to_vec(&response).unwrap_or_default();
            Ok(json_response(StatusCode::OK, json))
        }
        // Notification (e.g. notifications/initialized) — accepted, no body.
        None => Ok(plain(StatusCode::ACCEPTED, "")),
    }
}

// The error is a whole HTTP response, which makes every Ok carry its size too.
// Boxed so the common path stays small. Clippy 1.98 fails the build without it.
async fn read_body_capped(req: Request<Incoming>) -> Result<Bytes, Box<Response<Full<Bytes>>>> {
    if let Some(len) = req
        .headers()
        .get(hyper::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<usize>().ok())
    {
        if len > MAX_BODY_BYTES {
            return Err(Box::new(plain(
                StatusCode::PAYLOAD_TOO_LARGE,
                "body too large",
            )));
        }
    }
    // Limited enforces the cap DURING collection — a chunked body with no
    // Content-Length can't buffer past the limit before being rejected.
    let limited = http_body_util::Limited::new(req.into_body(), MAX_BODY_BYTES);
    match limited.collect().await {
        Ok(collected) => Ok(collected.to_bytes()),
        Err(e)
            if e.downcast_ref::<http_body_util::LengthLimitError>()
                .is_some() =>
        {
            Err(Box::new(plain(
                StatusCode::PAYLOAD_TOO_LARGE,
                "body too large",
            )))
        }
        Err(_) => Err(Box::new(plain(StatusCode::BAD_REQUEST, "body read failed"))),
    }
}

fn origin_is_local(origin: &str) -> bool {
    let host = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
        .unwrap_or("");
    let host = host.split('/').next().unwrap_or("");
    let host = if host.starts_with('[') {
        host.split(']')
            .next()
            .map(|h| format!("{h}]"))
            .unwrap_or_default()
    } else {
        host.rsplit_once(':')
            .map(|(h, _)| h)
            .unwrap_or(host)
            .to_string()
    };
    matches!(host.as_str(), "localhost" | "127.0.0.1" | "[::1]")
}

fn json_response(status: StatusCode, body: Vec<u8>) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header(hyper::header::CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body)))
        .unwrap_or_default()
}

fn plain(status: StatusCode, msg: &str) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header(hyper::header::CONTENT_TYPE, "text/plain")
        .body(Full::new(Bytes::from(msg.to_string())))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_handlers() -> Arc<McpHandlers> {
        Arc::new(McpHandlers::new())
    }

    async fn spawn_server() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(serve(listener, test_handlers()));
        format!("http://{addr}/mcp")
    }

    #[tokio::test]
    async fn initialize_over_http() {
        let url = spawn_server().await;
        let resp = crate::http::client()
            .post(&url)
            .json(&serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize"}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["result"]["serverInfo"]["name"], "drengr");
        assert!(body["result"]["protocolVersion"].is_string());
    }

    #[tokio::test]
    async fn notification_returns_202() {
        let url = spawn_server().await;
        let resp = crate::http::client()
            .post(&url)
            .json(&serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 202);
    }

    #[tokio::test]
    async fn get_is_405_and_wrong_path_404() {
        let url = spawn_server().await;
        let client = crate::http::client();
        assert_eq!(client.get(&url).send().await.unwrap().status(), 405);
        let root = url.trim_end_matches("/mcp").to_string();
        let resp = client
            .post(format!("{root}/other"))
            .body("{}")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);
    }

    #[tokio::test]
    async fn foreign_origin_rejected() {
        let url = spawn_server().await;
        let resp = crate::http::client()
            .post(&url)
            .header("Origin", "https://evil.example.com")
            .json(&serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize"}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 403);
    }

    #[tokio::test]
    async fn parse_error_is_400_with_jsonrpc_body() {
        let url = spawn_server().await;
        let resp = crate::http::client()
            .post(&url)
            .body("not json")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 400);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["error"]["code"], -32700);
    }

    #[tokio::test]
    async fn oversized_chunked_body_rejected_during_read() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(serve(listener, test_handlers()));

        // Chunked, no Content-Length — bypasses the early header check, so the
        // cap must trip during collection (Limited), not after.
        let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
        s.write_all(
            b"POST /mcp HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked\r\nContent-Type: application/json\r\n\r\n",
        )
        .await
        .unwrap();
        let chunk = vec![b'a'; 1024 * 1024];
        let header = format!("{:x}\r\n", chunk.len());
        for _ in 0..(MAX_BODY_BYTES / chunk.len() + 1) {
            if s.write_all(header.as_bytes()).await.is_err() {
                break;
            }
            if s.write_all(&chunk).await.is_err() {
                break;
            }
            if s.write_all(b"\r\n").await.is_err() {
                break;
            }
        }
        // Don't terminate the chunk stream — the server must answer mid-body.
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        loop {
            match s.read(&mut tmp).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    buf.extend_from_slice(&tmp[..n]);
                    if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
            }
        }
        let resp = String::from_utf8_lossy(&buf);
        assert!(
            resp.starts_with("HTTP/1.1 413"),
            "expected 413, got: {:.120}",
            resp
        );
    }

    #[test]
    fn origin_check_accepts_local_variants() {
        assert!(origin_is_local("http://localhost:63342"));
        assert!(origin_is_local("http://127.0.0.1"));
        assert!(origin_is_local("https://localhost"));
        assert!(!origin_is_local("https://evil.example.com"));
        assert!(!origin_is_local("http://localhost.evil.com"));
    }
}
