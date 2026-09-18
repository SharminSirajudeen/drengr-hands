use serde::{Deserialize, Serialize};

use crate::network::sink::BodyTruncation;

/// The wire protocol an in-app reporter speaks to drengr over localhost.
/// Drengr's own analytics SDK is one sender; any app that speaks this format
/// can stream what it sees, captured above TLS with no proxy and no CA.
/// Length-prefixed JSON protocol (4-byte u32 big-endian + JSON body).
/// Unknown fields are rejected so a hostile or version-skewed client can't
/// smuggle extra payload past us under a known tag.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum SdkMessage {
    /// SDK registration — app connects for the first time.
    #[serde(rename = "register")]
    Register {
        app_package: String,
        app_version: String,
        sdk_version: String,
        /// Authentication token (read from ~/.drengr/sdk_token).
        #[serde(default)]
        token: Option<String>,
    },

    /// SDK deregistration — app disconnects.
    #[serde(rename = "deregister")]
    Deregister { app_package: String },

    /// Network event — HTTP request/response captured.
    /// Headers and bodies are optional so an SDK built against the seven-field
    /// shape still parses and behaves exactly as it did.
    #[serde(rename = "event")]
    Event {
        url: String,
        method: String,
        status: Option<u16>,
        duration_ms: Option<u64>,
        request_size: Option<u64>,
        response_size: Option<u64>,
        timestamp_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_headers: Option<Vec<(String, String)>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        response_headers: Option<Vec<(String, String)>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_body: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        response_body: Option<String>,
        /// Which bodies the SDK cut short at its own capture cap. Absent means
        /// none. An SDK that truncates and stays silent would have the sink
        /// read a partial body as a complete one.
        #[serde(default, skip_serializing_if = "BodyTruncation::is_none")]
        truncated: BodyTruncation,
    },

    /// Screen change — activity transition detected.
    #[serde(rename = "screen_change")]
    ScreenChange {
        activity: String,
        package: String,
        timestamp_ms: u64,
    },

    /// Ping — keepalive from SDK.
    #[serde(rename = "ping")]
    Ping { timestamp_ms: u64 },

    /// Pong — server response to ping.
    #[serde(rename = "pong")]
    Pong { timestamp_ms: u64 },
}

impl SdkMessage {
    /// Encode a message as length-prefixed JSON bytes.
    pub fn encode(&self) -> anyhow::Result<Vec<u8>> {
        let json = serde_json::to_vec(self)?;
        let len = json.len() as u32;
        let mut buf = Vec::with_capacity(4 + json.len());
        buf.extend_from_slice(&len.to_be_bytes());
        buf.extend_from_slice(&json);
        Ok(buf)
    }

    /// Decode a message from JSON bytes (without the length prefix).
    pub fn decode(data: &[u8]) -> anyhow::Result<Self> {
        Ok(serde_json::from_slice(data)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_roundtrip() {
        let msg = SdkMessage::Register {
            app_package: "com.app".to_string(),
            app_version: "1.0".to_string(),
            sdk_version: "0.1.0".to_string(),
            token: None,
        };
        let encoded = msg.encode().unwrap();

        // First 4 bytes are length
        let len = u32::from_be_bytes([encoded[0], encoded[1], encoded[2], encoded[3]]);
        assert_eq!(len as usize, encoded.len() - 4);

        let decoded = SdkMessage::decode(&encoded[4..]).unwrap();
        match decoded {
            SdkMessage::Register { app_package, .. } => assert_eq!(app_package, "com.app"),
            _ => panic!("Expected Register"),
        }
    }

    fn bare_event() -> SdkMessage {
        SdkMessage::Event {
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
        }
    }

    #[test]
    fn test_event_serialize() {
        let json = serde_json::to_string(&bare_event()).unwrap();
        assert!(json.contains("\"type\":\"event\""));
        assert!(json.contains("/api/login"));
    }

    #[test]
    fn an_event_carrying_nothing_encodes_the_seven_field_shape_verbatim() {
        let json = serde_json::to_string(&bare_event()).unwrap();
        for absent in [
            "request_headers",
            "response_headers",
            "request_body",
            "response_body",
            "truncated",
        ] {
            assert!(
                !json.contains(absent),
                "{absent} must not appear on the wire when unset"
            );
        }
    }

    #[test]
    fn a_seven_field_event_from_an_older_sdk_still_decodes() {
        let json = r#"{"type":"event","url":"/api/login","method":"POST","status":200,"duration_ms":150,"request_size":100,"response_size":500,"timestamp_ms":1709500000000}"#;
        match SdkMessage::decode(json.as_bytes()).unwrap() {
            SdkMessage::Event {
                url,
                request_body,
                response_body,
                truncated,
                ..
            } => {
                assert_eq!(url, "/api/login");
                assert!(request_body.is_none() && response_body.is_none());
                assert_eq!(truncated, BodyTruncation::None);
            }
            _ => panic!("expected Event"),
        }
    }

    #[test]
    fn an_event_carrying_headers_and_bodies_survives_the_wire() {
        let json = r#"{"type":"event","url":"/api/login","method":"POST","status":200,"duration_ms":150,"request_size":100,"response_size":500,"timestamp_ms":1709500000000,"request_headers":[["content-type","application/json"]],"response_headers":[["x-req-id","7"]],"request_body":"{\"email\":\"[REDACTED]\"}","response_body":"{\"ok\":true}","truncated":"response"}"#;
        match SdkMessage::decode(json.as_bytes()).unwrap() {
            SdkMessage::Event {
                request_headers,
                response_headers,
                request_body,
                response_body,
                truncated,
                ..
            } => {
                assert_eq!(request_headers.unwrap()[0].0, "content-type");
                assert_eq!(response_headers.unwrap()[0].1, "7");
                assert_eq!(request_body.unwrap(), r#"{"email":"[REDACTED]"}"#);
                assert_eq!(response_body.unwrap(), r#"{"ok":true}"#);
                assert_eq!(truncated, BodyTruncation::Response);
            }
            _ => panic!("expected Event"),
        }
    }

    #[test]
    fn an_unknown_field_is_still_rejected_now_that_the_event_has_optional_ones() {
        let json = r#"{"type":"event","url":"/a","method":"GET","timestamp_ms":1,"status":null,"duration_ms":null,"request_size":null,"response_size":null,"smuggled":"x"}"#;
        assert!(SdkMessage::decode(json.as_bytes()).is_err());
    }

    #[test]
    fn test_screen_change_serialize() {
        let msg = SdkMessage::ScreenChange {
            activity: "com.app/.LoginActivity".to_string(),
            package: "com.app".to_string(),
            timestamp_ms: 1709500000000,
        };

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"screen_change\""));
        assert!(json.contains("LoginActivity"));
    }

    #[test]
    fn test_ping_pong() {
        let ping = SdkMessage::Ping {
            timestamp_ms: 12345,
        };
        let json = serde_json::to_string(&ping).unwrap();
        assert!(json.contains("\"type\":\"ping\""));

        let pong = SdkMessage::Pong {
            timestamp_ms: 12345,
        };
        let json = serde_json::to_string(&pong).unwrap();
        assert!(json.contains("\"type\":\"pong\""));
    }

    #[test]
    fn test_decode_register() {
        let json = r#"{"type":"register","app_package":"com.test","app_version":"2.0","sdk_version":"0.1"}"#;
        let msg = SdkMessage::decode(json.as_bytes()).unwrap();
        match msg {
            SdkMessage::Register {
                app_package,
                app_version,
                ..
            } => {
                assert_eq!(app_package, "com.test");
                assert_eq!(app_version, "2.0");
            }
            _ => panic!("Expected Register"),
        }
    }

    #[test]
    fn test_decode_invalid_json() {
        let result = SdkMessage::decode(b"not json");
        assert!(result.is_err());
    }

    #[test]
    fn test_encode_length_prefix() {
        let msg = SdkMessage::Ping { timestamp_ms: 0 };
        let encoded = msg.encode().unwrap();

        assert!(encoded.len() > 4);
        let declared_len = u32::from_be_bytes([encoded[0], encoded[1], encoded[2], encoded[3]]);
        assert_eq!(declared_len as usize, encoded.len() - 4);
    }
}
