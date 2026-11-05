use std::collections::HashMap;

use serde_json::Value;

use crate::error::RpcError;
use crate::message::{ClientMessage, ExitEncoded, RequestId, ServerMessage};

/// Whether a request answers once or streams chunks until its exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestKind {
    Unary,
    Stream,
}

/// What the transport must do after the session handles a server message.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionEvent {
    /// Stream values for `id`. Deliver them, then send `ack` so the server sends more; if the
    /// consumer is gone, call [`RpcSession::cancel`] instead.
    Chunk {
        id: RequestId,
        values: Vec<Value>,
        ack: ClientMessage,
    },
    /// A unary request's result, or a stream's normal end (with a `null` value).
    Completed {
        id: RequestId,
        value: Value,
    },
    Failed {
        id: RequestId,
        error: RpcError,
    },
    Pong,
}

/// Tracks the requests in flight on one connection.
#[derive(Debug, Default)]
pub struct RpcSession {
    next_id: u64,
    pending: HashMap<RequestId, RequestKind>,
}

impl RpcSession {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a request and returns the message to send.
    pub fn start(
        &mut self,
        tag: impl Into<String>,
        payload: Value,
        kind: RequestKind,
    ) -> (RequestId, ClientMessage) {
        self.next_id += 1;
        let id = RequestId::new(self.next_id.to_string());
        self.pending.insert(id.clone(), kind);
        let message = ClientMessage::Request {
            id: id.clone(),
            tag: tag.into(),
            payload,
            headers: Vec::new(),
        };
        (id, message)
    }

    /// Abandons a request. Returns the interrupt to send when it was still in flight.
    pub fn cancel(&mut self, id: &RequestId) -> Option<ClientMessage> {
        self.pending.remove(id).map(|_| ClientMessage::Interrupt {
            request_id: id.clone(),
        })
    }

    pub fn is_pending(&self, id: &RequestId) -> bool {
        self.pending.contains_key(id)
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Ends every request, e.g. because the socket closed.
    pub fn fail_all(&mut self, error: &RpcError) -> Vec<SessionEvent> {
        let mut ids: Vec<RequestId> = self.pending.drain().map(|(id, _)| id).collect();
        ids.sort();
        ids.into_iter()
            .map(|id| SessionEvent::Failed {
                id,
                error: error.clone(),
            })
            .collect()
    }

    pub fn handle(&mut self, message: ServerMessage) -> Vec<SessionEvent> {
        match message {
            ServerMessage::Chunk { request_id, values } => {
                // Like Effect's client, chunks for unknown or unary requests are dropped unacked.
                match self.pending.get(&request_id) {
                    Some(RequestKind::Stream) if !values.is_empty() => vec![SessionEvent::Chunk {
                        ack: ClientMessage::Ack {
                            request_id: request_id.clone(),
                        },
                        id: request_id,
                        values,
                    }],
                    _ => Vec::new(),
                }
            }
            ServerMessage::Exit { request_id, exit } => {
                let Some(kind) = self.pending.remove(&request_id) else {
                    return Vec::new();
                };
                let event = match exit {
                    ExitEncoded::Success { value } => SessionEvent::Completed {
                        id: request_id,
                        value: match kind {
                            RequestKind::Unary => value,
                            RequestKind::Stream => Value::Null,
                        },
                    },
                    ExitEncoded::Failure { cause } => SessionEvent::Failed {
                        id: request_id,
                        error: RpcError::from_cause(&cause),
                    },
                };
                vec![event]
            }
            ServerMessage::Defect { defect } => self.fail_all(&RpcError::defect(defect)),
            ServerMessage::ClientProtocolError { error, request_ids } => {
                let error = RpcError::Protocol(
                    error
                        .get("message")
                        .and_then(Value::as_str)
                        .map_or_else(|| error.to_string(), str::to_owned),
                );
                match request_ids {
                    None => self.fail_all(&error),
                    Some(ids) => ids
                        .into_iter()
                        .filter(|id| self.pending.remove(id).is_some())
                        .map(|id| SessionEvent::Failed {
                            id,
                            error: error.clone(),
                        })
                        .collect(),
                }
            }
            ServerMessage::Pong => vec![SessionEvent::Pong],
            ServerMessage::Unknown => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{CauseEncoded, decode_frame};
    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn handle_frame(session: &mut RpcSession, frame: &str) -> Vec<SessionEvent> {
        decode_frame(frame)
            .unwrap()
            .into_iter()
            .flat_map(|message| session.handle(message))
            .collect()
    }

    #[test]
    fn allocates_increasing_string_ids() {
        let mut session = RpcSession::new();
        let (a, request) = session.start("a", json!({}), RequestKind::Unary);
        let (b, _) = session.start("b", json!({"x": 1}), RequestKind::Stream);
        assert_eq!(a.as_str(), "1");
        assert_eq!(b.as_str(), "2");
        assert_eq!(
            request,
            ClientMessage::Request {
                id: a,
                tag: "a".into(),
                payload: json!({}),
                headers: vec![]
            }
        );
        assert_eq!(session.pending_count(), 2);
    }

    #[test]
    fn unary_success_completes_with_value() {
        let mut session = RpcSession::new();
        let (id, _) = session.start("server.getConfig", json!({}), RequestKind::Unary);
        let events = handle_frame(
            &mut session,
            r#"{"_tag":"Exit","requestId":"1","exit":{"_tag":"Success","value":{"ok":true}}}"#,
        );
        assert_eq!(
            events,
            vec![SessionEvent::Completed {
                id,
                value: json!({"ok": true})
            }]
        );
        assert_eq!(session.pending_count(), 0);
    }

    #[test]
    fn stream_chunks_are_acked_and_exit_ends_the_stream() {
        let mut session = RpcSession::new();
        let (id, _) = session.start(
            "orchestration.subscribeShell",
            json!({}),
            RequestKind::Stream,
        );
        let events = handle_frame(
            &mut session,
            r#"{"_tag":"Chunk","requestId":"1","values":[{"kind":"synchronized"}]}"#,
        );
        assert_eq!(
            events,
            vec![SessionEvent::Chunk {
                id: id.clone(),
                values: vec![json!({"kind": "synchronized"})],
                ack: ClientMessage::Ack {
                    request_id: id.clone()
                },
            }]
        );
        assert!(session.is_pending(&id));
        let events = handle_frame(
            &mut session,
            r#"{"_tag":"Exit","requestId":"1","exit":{"_tag":"Success","value":"ignored"}}"#,
        );
        assert_eq!(
            events,
            vec![SessionEvent::Completed {
                id,
                value: Value::Null
            }]
        );
    }

    #[test]
    fn chunks_for_unknown_unary_or_empty_requests_are_dropped() {
        let mut session = RpcSession::new();
        session.start("unary", json!({}), RequestKind::Unary);
        session.start("stream", json!({}), RequestKind::Stream);
        assert!(
            handle_frame(
                &mut session,
                r#"{"_tag":"Chunk","requestId":"1","values":[1]}"#
            )
            .is_empty()
        );
        assert!(
            handle_frame(
                &mut session,
                r#"{"_tag":"Chunk","requestId":"99","values":[1]}"#
            )
            .is_empty()
        );
        assert!(
            handle_frame(
                &mut session,
                r#"{"_tag":"Chunk","requestId":"2","values":[]}"#
            )
            .is_empty()
        );
    }

    #[test]
    fn exit_for_unknown_request_is_ignored() {
        let mut session = RpcSession::new();
        assert!(
            handle_frame(
                &mut session,
                r#"{"_tag":"Exit","requestId":"5","exit":{"_tag":"Success"}}"#
            )
            .is_empty()
        );
    }

    #[test]
    fn exit_is_delivered_once() {
        let mut session = RpcSession::new();
        session.start("unary", json!({}), RequestKind::Unary);
        let frame = r#"{"_tag":"Exit","requestId":"1","exit":{"_tag":"Success","value":1}}"#;
        assert_eq!(handle_frame(&mut session, frame).len(), 1);
        assert!(handle_frame(&mut session, frame).is_empty());
    }

    #[test]
    fn failure_exit_carries_typed_error() {
        let mut session = RpcSession::new();
        let (id, _) = session.start("x", json!({}), RequestKind::Unary);
        let events = handle_frame(
            &mut session,
            r#"{"_tag":"Exit","requestId":"1","exit":{"_tag":"Failure","cause":[
                {"_tag":"Fail","error":{"_tag":"EnvironmentAuthorizationError","message":"denied"}}]}}"#,
        );
        assert_eq!(
            events,
            vec![SessionEvent::Failed {
                id,
                error: RpcError::from_cause(&[CauseEncoded::Fail {
                    error: json!({"_tag": "EnvironmentAuthorizationError", "message": "denied"})
                }]),
            }]
        );
    }

    #[test]
    fn defect_fails_every_pending_request_in_id_order() {
        let mut session = RpcSession::new();
        for _ in 0..3 {
            session.start("x", json!({}), RequestKind::Stream);
        }
        let events = handle_frame(&mut session, r#"{"_tag":"Defect","defect":"bad frame"}"#);
        let ids: Vec<&str> = events
            .iter()
            .map(|event| match event {
                SessionEvent::Failed { id, error } => {
                    assert_eq!(error.to_string(), "server defect: bad frame");
                    id.as_str()
                }
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(ids, vec!["1", "2", "3"]);
        assert_eq!(session.pending_count(), 0);
    }

    #[test]
    fn protocol_error_fails_listed_requests_only() {
        let mut session = RpcSession::new();
        session.start("x", json!({}), RequestKind::Unary);
        session.start("y", json!({}), RequestKind::Unary);
        let events = handle_frame(
            &mut session,
            r#"{"_tag":"ClientProtocolError","error":{"message":"bad payload"},"requestIds":["2","77"]}"#,
        );
        assert_eq!(
            events,
            vec![SessionEvent::Failed {
                id: RequestId::new("2"),
                error: RpcError::Protocol("bad payload".into())
            }]
        );
        assert_eq!(session.pending_count(), 1);
    }

    #[test]
    fn protocol_error_without_ids_fails_everything() {
        let mut session = RpcSession::new();
        session.start("x", json!({}), RequestKind::Unary);
        session.start("y", json!({}), RequestKind::Stream);
        let events = handle_frame(
            &mut session,
            r#"{"_tag":"ClientProtocolError","error":"x"}"#,
        );
        assert_eq!(events.len(), 2);
        assert_eq!(session.pending_count(), 0);
    }

    #[test]
    fn cancel_interrupts_only_pending_requests() {
        let mut session = RpcSession::new();
        let (id, _) = session.start("x", json!({}), RequestKind::Stream);
        assert_eq!(
            session.cancel(&id),
            Some(ClientMessage::Interrupt {
                request_id: id.clone()
            })
        );
        assert_eq!(session.cancel(&id), None);
        // A late chunk for a cancelled stream is not acked.
        assert!(
            handle_frame(
                &mut session,
                r#"{"_tag":"Chunk","requestId":"1","values":[1]}"#
            )
            .is_empty()
        );
    }

    #[test]
    fn fail_all_on_disconnect() {
        let mut session = RpcSession::new();
        session.start("x", json!({}), RequestKind::Unary);
        let events = session.fail_all(&RpcError::Disconnected);
        assert_eq!(
            events,
            vec![SessionEvent::Failed {
                id: RequestId::new("1"),
                error: RpcError::Disconnected
            }]
        );
        assert!(session.fail_all(&RpcError::Disconnected).is_empty());
    }

    #[test]
    fn pong_and_unknown() {
        let mut session = RpcSession::new();
        assert_eq!(
            handle_frame(&mut session, r#"{"_tag":"Pong"}"#),
            vec![SessionEvent::Pong]
        );
        assert!(handle_frame(&mut session, r#"{"_tag":"Whatever"}"#).is_empty());
    }

    #[test]
    fn interleaved_streams_stay_separate() {
        let mut session = RpcSession::new();
        let (a, _) = session.start("a", json!({}), RequestKind::Stream);
        let (b, _) = session.start("b", json!({}), RequestKind::Stream);
        let events = handle_frame(
            &mut session,
            r#"[{"_tag":"Chunk","requestId":"2","values":["b1"]},
                {"_tag":"Chunk","requestId":"1","values":["a1","a2"]},
                {"_tag":"Exit","requestId":"2","exit":{"_tag":"Success"}}]"#,
        );
        assert_eq!(
            events,
            vec![
                SessionEvent::Chunk {
                    id: b.clone(),
                    values: vec![json!("b1")],
                    ack: ClientMessage::Ack {
                        request_id: b.clone()
                    },
                },
                SessionEvent::Chunk {
                    id: a.clone(),
                    values: vec![json!("a1"), json!("a2")],
                    ack: ClientMessage::Ack {
                        request_id: a.clone()
                    },
                },
                SessionEvent::Completed {
                    id: b,
                    value: Value::Null
                },
            ]
        );
        assert!(session.is_pending(&a));
    }
}
