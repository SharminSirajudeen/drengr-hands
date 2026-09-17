//! HTTP client for the drengr-runner XCTest target.

use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::driver::{DriverError, Result};

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    Tap {
        x: f64,
        y: f64,
    },
    Swipe {
        x1: f64,
        y1: f64,
        x2: f64,
        y2: f64,
        duration_ms: u32,
    },
    DrawPath {
        points: Vec<Point2D>,
        duration_ms: u32,
    },
    Type {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        bundle_id: Option<String>,
    },
    Button {
        button: ButtonKind,
    },
    Orientation {
        orientation: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Point2D {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ButtonKind {
    Home,
    Lock,
    Siri,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Observation {
    pub screenshot_b64: String,
    pub tree_hint: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RunnerStatus {
    pub ok: bool,
    pub product: String,
    pub version: String,
    pub ios_major: u8,
    pub screen: ScreenDimensions,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct ScreenDimensions {
    pub width: u32,
    pub height: u32,
    pub scale: f64,
}

#[derive(Debug, Deserialize)]
struct ActAck {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
}

pub struct DriverClient {
    base_url: String,
    http: reqwest::Client,
}

impl DriverClient {
    pub fn new(port: u16) -> Self {
        let http = reqwest::Client::builder()
            .pool_max_idle_per_host(0)
            .build()
            .expect("reqwest client build");
        Self {
            base_url: format!("http://127.0.0.1:{port}"),
            http,
        }
    }

    #[cfg(test)]
    fn with_base_url(mut self, url: String) -> Self {
        self.base_url = url;
        self
    }

    pub async fn status(&self) -> Result<RunnerStatus> {
        let resp = self
            .http
            .get(format!("{}/status", self.base_url))
            .timeout(Duration::from_secs(2))
            .send()
            .await?;
        decode_json(resp).await
    }

    /// Observe the screen. `bundle` names the foreground app so the runner can
    /// snapshot its real element tree (not just SpringBoard); pass `None` to
    /// fall back to SpringBoard-only.
    pub async fn observe(&self, bundle: Option<&str>) -> Result<Observation> {
        let url = match bundle {
            // Bundle ids are reverse-DNS (alphanumerics, '.', '-') — URL-safe.
            Some(b) if !b.is_empty() && b != "Unknown" => {
                format!("{}/observe?bundle={}", self.base_url, b)
            }
            _ => format!("{}/observe", self.base_url),
        };
        let resp = self
            .send_with_connect_retry(|| self.http.get(&url).timeout(Duration::from_secs(15)))
            .await?;
        decode_json(resp).await
    }

    pub async fn act(&self, action: Action) -> Result<()> {
        let url = format!("{}/act", self.base_url);
        let resp = self
            .send_with_connect_retry(|| {
                self.http
                    .post(&url)
                    .timeout(Duration::from_secs(10))
                    .json(&action)
            })
            .await?;
        let ack: ActAck = decode_json(resp).await?;
        if !ack.ok {
            return Err(DriverError::BadResponse {
                status: 200,
                body: ack.error.unwrap_or_else(|| "unknown runner error".into()),
            });
        }
        Ok(())
    }

    /// Send a request, retrying CONNECTION failures a few times. The runner
    /// tears down + re-opens its listening socket (~0.3s) when a SpringBoard
    /// overlay or rotation makes it suspension-eligible (iOS hangs up listening
    /// sockets). A request landing in that window is *connection-refused* — and
    /// since the connection was refused, the request never reached the runner,
    /// so retrying cannot double-execute the action.
    async fn send_with_connect_retry(
        &self,
        build: impl Fn() -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response> {
        let mut last: Option<reqwest::Error> = None;
        for attempt in 0..5 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(400)).await;
            }
            match build().send().await {
                Ok(resp) => return Ok(resp),
                Err(e) if e.is_connect() => {
                    last = Some(e);
                    continue;
                }
                Err(e) => return Err(e.into()),
            }
        }
        Err(last.expect("retry loop ran at least once").into())
    }
}

async fn decode_json<T: serde::de::DeserializeOwned>(resp: reqwest::Response) -> Result<T> {
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(DriverError::BadResponse {
            status: status.as_u16(),
            body,
        });
    }
    let body = resp.text().await.unwrap_or_default();
    match serde_json::from_str::<T>(&body) {
        Ok(v) => Ok(v),
        Err(_) => {
            let body_preview: String = body.chars().take(500).collect();
            Err(DriverError::BadResponse {
                status: 200,
                body: format!(
                    "expected {}, got: {}",
                    std::any::type_name::<T>(),
                    body_preview
                ),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn setup() -> (MockServer, DriverClient) {
        let server = MockServer::start().await;
        let client = DriverClient::new(0).with_base_url(server.uri());
        (server, client)
    }

    #[tokio::test]
    async fn status_parses_ok_response() {
        let (server, client) = setup().await;
        Mock::given(method("GET"))
            .and(path("/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "product": "drengr-runner",
                "version": "0.6.0",
                "ios_major": 17,
                "screen": { "width": 1170, "height": 2532, "scale": 3.0 }
            })))
            .mount(&server)
            .await;

        let s = client.status().await.expect("status ok");
        assert!(s.ok);
        assert_eq!(s.product, "drengr-runner");
        assert_eq!(s.version, "0.6.0");
        assert_eq!(s.ios_major, 17);
        assert_eq!(s.screen.width, 1170);
        assert_eq!(s.screen.height, 2532);
        assert!((s.screen.scale - 3.0).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn observe_handles_null_tree_hint() {
        let (server, client) = setup().await;
        Mock::given(method("GET"))
            .and(path("/observe"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "screenshot_b64": "AAAA",
                "tree_hint": null
            })))
            .mount(&server)
            .await;

        let o = client.observe(None).await.expect("observe ok");
        assert_eq!(o.screenshot_b64, "AAAA");
        assert!(o.tree_hint.is_none());
    }

    #[tokio::test]
    async fn act_ok_response() {
        let (server, client) = setup().await;
        Mock::given(method("POST"))
            .and(path("/act"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ok": true })),
            )
            .mount(&server)
            .await;

        client
            .act(Action::Tap { x: 10.0, y: 20.0 })
            .await
            .expect("act ok");
    }

    #[tokio::test]
    async fn act_application_error() {
        let (server, client) = setup().await;
        Mock::given(method("POST"))
            .and(path("/act"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": false,
                "error": "boom"
            })))
            .mount(&server)
            .await;

        let err = client
            .act(Action::Tap { x: 1.0, y: 1.0 })
            .await
            .expect_err("must error");
        match err {
            DriverError::BadResponse { status, body } => {
                assert_eq!(status, 200);
                assert!(body.contains("boom"), "body was {body}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn act_http_500() {
        let (server, client) = setup().await;
        Mock::given(method("POST"))
            .and(path("/act"))
            .respond_with(ResponseTemplate::new(500).set_body_string("kaboom"))
            .mount(&server)
            .await;

        let err = client
            .act(Action::Tap { x: 1.0, y: 1.0 })
            .await
            .expect_err("must error");
        match err {
            DriverError::BadResponse { status, .. } => assert_eq!(status, 500),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn act_tap_serializes_correctly() {
        let (server, client) = setup().await;
        Mock::given(method("POST"))
            .and(path("/act"))
            .and(body_json(
                serde_json::json!({ "kind": "tap", "x": 100.0, "y": 200.0 }),
            ))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ok": true })),
            )
            .mount(&server)
            .await;

        client
            .act(Action::Tap { x: 100.0, y: 200.0 })
            .await
            .expect("tap ok");
    }

    #[tokio::test]
    async fn act_type_with_bundle_id_serializes() {
        let (server, client) = setup().await;
        Mock::given(method("POST"))
            .and(path("/act"))
            .and(body_json(serde_json::json!({
                "kind": "type",
                "text": "hello",
                "bundle_id": "com.example.app"
            })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ok": true })),
            )
            .mount(&server)
            .await;

        client
            .act(Action::Type {
                text: "hello".into(),
                bundle_id: Some("com.example.app".into()),
            })
            .await
            .expect("type ok");
    }

    #[tokio::test]
    async fn act_malformed_json_response() {
        let (server, client) = setup().await;
        Mock::given(method("POST"))
            .and(path("/act"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;

        let err = client
            .act(Action::Tap { x: 1.0, y: 1.0 })
            .await
            .expect_err("must error");
        match err {
            DriverError::BadResponse { status, body } => {
                assert_eq!(status, 200);
                assert!(body.contains("not json"), "body was {body}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn status_decode_failure_preserves_body() {
        let (server, client) = setup().await;
        Mock::given(method("GET"))
            .and(path("/status"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;

        let err = client.status().await.expect_err("must error");
        match err {
            DriverError::BadResponse { status, body } => {
                assert_eq!(status, 200);
                assert!(body.contains("not json"), "body was {body}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn act_type_without_bundle_id_omits_field() {
        let (server, client) = setup().await;
        Mock::given(method("POST"))
            .and(path("/act"))
            .and(body_json(
                serde_json::json!({ "kind": "type", "text": "hi" }),
            ))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ok": true })),
            )
            .mount(&server)
            .await;

        client
            .act(Action::Type {
                text: "hi".into(),
                bundle_id: None,
            })
            .await
            .expect("type ok");
    }
}
