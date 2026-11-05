use std::fmt;

use serde::de::{self, Deserializer, Visitor};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;

/// Identifies one request on a connection.
///
/// Effect accepts string or numeric ids and echoes back whatever the client sent. This client
/// always sends strings, but decodes either form so a server that renumbers still matches.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RequestId(String);

impl RequestId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for RequestId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for RequestId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct IdVisitor;

        impl Visitor<'_> for IdVisitor {
            type Value = RequestId;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a string or integer request id")
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<RequestId, E> {
                Ok(RequestId::new(v))
            }

            fn visit_u64<E: de::Error>(self, v: u64) -> Result<RequestId, E> {
                Ok(RequestId::new(v.to_string()))
            }

            fn visit_i64<E: de::Error>(self, v: i64) -> Result<RequestId, E> {
                Ok(RequestId::new(v.to_string()))
            }

            fn visit_f64<E: de::Error>(self, v: f64) -> Result<RequestId, E> {
                if v.fract() == 0.0 && v.is_finite() && v.abs() < 9.0e15 {
                    // Lossless: the guard keeps the value inside f64's exact integer range.
                    #[allow(clippy::cast_possible_truncation)]
                    Ok(RequestId::new((v as i64).to_string()))
                } else {
                    Err(E::custom(format!("non-integer request id {v}")))
                }
            }
        }

        deserializer.deserialize_any(IdVisitor)
    }
}

/// A message from client to server.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "_tag")]
pub enum ClientMessage {
    Request {
        id: RequestId,
        tag: String,
        payload: Value,
        headers: Vec<(String, String)>,
    },
    /// Lets the server send the next chunk of a stream.
    Ack {
        #[serde(rename = "requestId")]
        request_id: RequestId,
    },
    Interrupt {
        #[serde(rename = "requestId")]
        request_id: RequestId,
    },
    Ping,
    Eof,
}

/// A message from server to client.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "_tag")]
pub enum ServerMessage {
    Chunk {
        #[serde(rename = "requestId")]
        request_id: RequestId,
        values: Vec<Value>,
    },
    Exit {
        #[serde(rename = "requestId")]
        request_id: RequestId,
        exit: ExitEncoded,
    },
    /// A failure not tied to one request; it ends every request on the connection.
    Defect {
        #[serde(default)]
        defect: Value,
    },
    Pong,
    ClientProtocolError {
        #[serde(default)]
        error: Value,
        #[serde(default, rename = "requestIds")]
        request_ids: Option<Vec<RequestId>>,
    },
    /// Server-initiated requests and message kinds newer than this client.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "_tag")]
pub enum ExitEncoded {
    Success {
        /// Absent for `void` results.
        #[serde(default)]
        value: Value,
    },
    Failure {
        #[serde(default)]
        cause: Vec<CauseEncoded>,
    },
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "_tag")]
pub enum CauseEncoded {
    /// An expected, typed failure declared by the RPC.
    Fail {
        #[serde(default)]
        error: Value,
    },
    /// An unexpected failure on the server.
    Die {
        #[serde(default)]
        defect: Value,
    },
    Interrupt {
        #[serde(default, rename = "fiberId")]
        fiber_id: Option<Value>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("frame is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
}

/// Decodes one WebSocket text frame, which holds a message or an array of messages.
pub fn decode_frame(text: &str) -> Result<Vec<ServerMessage>, FrameError> {
    let value: Value = serde_json::from_str(text)?;
    match value {
        Value::Array(items) => items
            .into_iter()
            .map(|item| serde_json::from_value(item).map_err(FrameError::from))
            .collect(),
        other => Ok(vec![serde_json::from_value(other)?]),
    }
}

/// Encodes one client message as a WebSocket text frame.
pub fn encode_message(message: &ClientMessage) -> String {
    // Serializing these types cannot fail: every key is a string and every value is JSON.
    serde_json::to_string(message).unwrap_or_else(|_| unreachable!("client messages are JSON"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    #[test]
    fn encodes_request_exactly_as_effect_expects() {
        let message = ClientMessage::Request {
            id: RequestId::new("7"),
            tag: "server.getConfig".into(),
            payload: json!({}),
            headers: vec![],
        };
        assert_eq!(
            encode_message(&message),
            r#"{"_tag":"Request","id":"7","tag":"server.getConfig","payload":{},"headers":[]}"#
        );
    }

    #[test]
    fn encodes_headers_as_pairs() {
        let message = ClientMessage::Request {
            id: RequestId::new("1"),
            tag: "x".into(),
            payload: Value::Null,
            headers: vec![("a".into(), "b".into())],
        };
        let encoded: Value = serde_json::from_str(&encode_message(&message)).unwrap();
        assert_eq!(encoded["headers"], json!([["a", "b"]]));
    }

    #[test]
    fn encodes_control_messages() {
        let id = RequestId::new("3");
        assert_eq!(
            encode_message(&ClientMessage::Ack {
                request_id: id.clone()
            }),
            r#"{"_tag":"Ack","requestId":"3"}"#
        );
        assert_eq!(
            encode_message(&ClientMessage::Interrupt { request_id: id }),
            r#"{"_tag":"Interrupt","requestId":"3"}"#
        );
        assert_eq!(encode_message(&ClientMessage::Ping), r#"{"_tag":"Ping"}"#);
        assert_eq!(encode_message(&ClientMessage::Eof), r#"{"_tag":"Eof"}"#);
    }

    #[test]
    fn decodes_chunk() {
        let frames = decode_frame(r#"{"_tag":"Chunk","requestId":"4","values":[1,{"a":2}]}"#);
        assert_eq!(
            frames.unwrap(),
            vec![ServerMessage::Chunk {
                request_id: RequestId::new("4"),
                values: vec![json!(1), json!({"a": 2})],
            }]
        );
    }

    #[test]
    fn decodes_numeric_request_ids() {
        let frames = decode_frame(r#"{"_tag":"Chunk","requestId":12,"values":[null]}"#).unwrap();
        let ServerMessage::Chunk { request_id, .. } = &frames[0] else {
            panic!("expected chunk");
        };
        assert_eq!(request_id.as_str(), "12");
    }

    #[test]
    fn decodes_float_encoded_integer_ids() {
        let frames = decode_frame(r#"{"_tag":"Pong"}"#).unwrap();
        assert_eq!(frames, vec![ServerMessage::Pong]);
        let id: RequestId = serde_json::from_str("5.0").unwrap();
        assert_eq!(id.as_str(), "5");
        assert!(serde_json::from_str::<RequestId>("5.5").is_err());
        assert!(serde_json::from_str::<RequestId>("true").is_err());
    }

    #[test]
    fn decodes_success_exit_with_and_without_value() {
        let frames = decode_frame(
            r#"[{"_tag":"Exit","requestId":"1","exit":{"_tag":"Success","value":{"sequence":3}}},
                {"_tag":"Exit","requestId":"2","exit":{"_tag":"Success"}}]"#,
        )
        .unwrap();
        assert_eq!(
            frames,
            vec![
                ServerMessage::Exit {
                    request_id: RequestId::new("1"),
                    exit: ExitEncoded::Success {
                        value: json!({"sequence": 3})
                    },
                },
                ServerMessage::Exit {
                    request_id: RequestId::new("2"),
                    exit: ExitEncoded::Success { value: Value::Null },
                },
            ]
        );
    }

    #[test]
    fn decodes_failure_causes() {
        let frames = decode_frame(
            r#"{"_tag":"Exit","requestId":"9","exit":{"_tag":"Failure","cause":[
                {"_tag":"Fail","error":{"_tag":"Nope","message":"no"}},
                {"_tag":"Die","defect":"boom"},
                {"_tag":"Interrupt","fiberId":4},
                {"_tag":"Sequential"}
            ]}}"#,
        )
        .unwrap();
        let ServerMessage::Exit {
            exit: ExitEncoded::Failure { cause },
            ..
        } = &frames[0]
        else {
            panic!("expected failure");
        };
        assert_eq!(
            cause,
            &vec![
                CauseEncoded::Fail {
                    error: json!({"_tag": "Nope", "message": "no"})
                },
                CauseEncoded::Die {
                    defect: json!("boom")
                },
                CauseEncoded::Interrupt {
                    fiber_id: Some(json!(4))
                },
                CauseEncoded::Unknown,
            ]
        );
    }

    #[test]
    fn decodes_defect_and_protocol_error() {
        let frames = decode_frame(
            r#"[{"_tag":"Defect","defect":{"message":"bad"}},
                {"_tag":"ClientProtocolError","error":{"_tag":"RpcClientError"},"requestIds":["1",2]},
                {"_tag":"ClientProtocolError"}]"#,
        )
        .unwrap();
        assert_eq!(
            frames[0],
            ServerMessage::Defect {
                defect: json!({"message": "bad"})
            }
        );
        assert_eq!(
            frames[1],
            ServerMessage::ClientProtocolError {
                error: json!({"_tag": "RpcClientError"}),
                request_ids: Some(vec![RequestId::new("1"), RequestId::new("2")]),
            }
        );
        assert_eq!(
            frames[2],
            ServerMessage::ClientProtocolError {
                error: Value::Null,
                request_ids: None
            }
        );
    }

    #[test]
    fn tolerates_unknown_and_server_initiated_messages() {
        let frames = decode_frame(
            r#"[{"_tag":"Request","id":"1","tag":"x","payload":{},"headers":[]},{"_tag":"Fancy"}]"#,
        )
        .unwrap();
        assert_eq!(frames, vec![ServerMessage::Unknown, ServerMessage::Unknown]);
    }

    #[test]
    fn rejects_garbage() {
        assert!(decode_frame("not json").is_err());
        assert!(decode_frame(r#"{"no":"tag"}"#).is_err());
        assert!(decode_frame(r#"{"_tag":"Chunk","requestId":"1"}"#).is_err());
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn string_ids_round_trip(id in ".{0,40}") {
                let encoded = serde_json::to_string(&RequestId::new(id.clone())).unwrap();
                let decoded: RequestId = serde_json::from_str(&encoded).unwrap();
                prop_assert_eq!(decoded.as_str(), id.as_str());
            }

            #[test]
            fn integer_ids_decode_to_their_decimal_form(id in any::<u64>()) {
                let decoded: RequestId = serde_json::from_str(&id.to_string()).unwrap();
                prop_assert_eq!(decoded.as_str(), id.to_string());
            }

            #[test]
            fn chunk_frames_preserve_values(values in proptest::collection::vec(any::<i64>(), 1..20)) {
                let frame = json!({"_tag": "Chunk", "requestId": "1", "values": values}).to_string();
                let decoded = decode_frame(&frame).unwrap();
                let expected: Vec<Value> = values.iter().map(|v| json!(v)).collect();
                prop_assert_eq!(
                    decoded,
                    vec![ServerMessage::Chunk { request_id: RequestId::new("1"), values: expected }]
                );
            }
        }
    }
}
