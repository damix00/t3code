//! One WebSocket connection to a T3 Code server, multiplexing RPC requests over it.
//!
//! A background task owns the socket and an [`RpcSession`]. [`Connection`] handles send it
//! requests; replies come back on channels, so callers can await them from any executor.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use futures::{SinkExt, Stream, StreamExt};
use serde_json::Value;
use t3_protocol::{
    ClientMessage, RequestId, RequestKind, RpcError, RpcSession, SessionEvent, decode_frame,
    encode_message,
};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::auth::redact_url;

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// How often the client pings, and how long a silent socket lives, as in upstream's protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Keepalive {
    pub ping_interval: Duration,
    pub idle_timeout: Duration,
}

impl Default for Keepalive {
    fn default() -> Self {
        Self {
            ping_interval: Duration::from_secs(5),
            idle_timeout: Duration::from_secs(15),
        }
    }
}

/// Opening the socket failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConnectError {
    #[error("timed out opening {url}")]
    Timeout { url: String },
    #[error("server refused the WebSocket upgrade with status {status}")]
    Rejected { status: u16 },
    #[error("could not open {url}: {message}")]
    Io { url: String, message: String },
}

impl ConnectError {
    /// Whether the server refused the ticket, so a new one is needed before retrying.
    pub fn is_unauthorized(&self) -> bool {
        matches!(self, Self::Rejected { status: 401 | 403 })
    }

    /// The server speaks another orchestration protocol version.
    pub fn is_incompatible(&self) -> bool {
        matches!(self, Self::Rejected { status: 426 })
    }
}

enum Reply {
    Unary(oneshot::Sender<Result<Value, RpcError>>),
    Stream(mpsc::UnboundedSender<Result<Value, RpcError>>),
}

enum Command {
    Request {
        token: u64,
        tag: String,
        payload: Value,
        reply: Reply,
    },
    Cancel {
        token: u64,
    },
}

struct Shared {
    commands: mpsc::UnboundedSender<Command>,
    next_token: AtomicU64,
}

/// A live connection. Clones share the socket, which closes once every clone and every open
/// stream is dropped, or the server goes away.
#[derive(Clone)]
pub struct Connection {
    shared: Arc<Shared>,
    closed: watch::Receiver<Option<RpcError>>,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("closed", &self.is_closed())
            .finish()
    }
}

impl Connection {
    /// Opens `url` (a `ws:` or `wss:` URL, ticket included) within `timeout`.
    pub async fn connect(
        url: &str,
        timeout: Duration,
        keepalive: Keepalive,
    ) -> Result<Self, ConnectError> {
        let redacted = redact_url(url);
        let socket =
            match tokio::time::timeout(timeout, tokio_tungstenite::connect_async(url)).await {
                Err(_) => return Err(ConnectError::Timeout { url: redacted }),
                Ok(Err(tokio_tungstenite::tungstenite::Error::Http(response))) => {
                    return Err(ConnectError::Rejected {
                        status: response.status().as_u16(),
                    });
                }
                Ok(Err(error)) => {
                    return Err(ConnectError::Io {
                        url: redacted,
                        message: error.to_string(),
                    });
                }
                Ok(Ok((socket, _response))) => socket,
            };
        tracing::debug!(url = %redacted, "websocket connected");
        Ok(Self::spawn(socket, keepalive))
    }

    fn spawn(socket: Socket, keepalive: Keepalive) -> Self {
        let (commands, receiver) = mpsc::unbounded_channel();
        let (closed_tx, closed) = watch::channel(None);
        tokio::spawn(run(socket, receiver, closed_tx, keepalive));
        Self {
            shared: Arc::new(Shared {
                commands,
                next_token: AtomicU64::new(0),
            }),
            closed,
        }
    }

    fn token(&self) -> u64 {
        self.shared.next_token.fetch_add(1, Ordering::Relaxed)
    }

    pub fn is_closed(&self) -> bool {
        self.closed.borrow().is_some() || self.shared.commands.is_closed()
    }

    /// Resolves once the connection has closed, with the reason.
    pub async fn closed(&self) -> RpcError {
        let mut closed = self.closed.clone();
        match closed.wait_for(Option::is_some).await {
            Ok(reason) => reason.clone().unwrap_or(RpcError::Disconnected),
            Err(_) => RpcError::Disconnected,
        }
    }

    /// Sends a request that answers once.
    pub async fn request(&self, tag: &str, payload: Value) -> Result<Value, RpcError> {
        let (reply, response) = oneshot::channel();
        let command = Command::Request {
            token: self.token(),
            tag: tag.to_owned(),
            payload,
            reply: Reply::Unary(reply),
        };
        if self.shared.commands.send(command).is_err() {
            return Err(self.close_reason());
        }
        match response.await {
            Ok(result) => result,
            Err(_) => Err(self.close_reason()),
        }
    }

    /// Starts a streaming request. Dropping the stream interrupts it on the server.
    pub fn stream(&self, tag: &str, payload: Value) -> RpcStream {
        let (sink, values) = mpsc::unbounded_channel();
        let token = self.token();
        let command = Command::Request {
            token,
            tag: tag.to_owned(),
            payload,
            reply: Reply::Stream(sink),
        };
        let failed = self
            .shared
            .commands
            .send(command)
            .is_err()
            .then(|| self.close_reason());
        RpcStream {
            token,
            values,
            commands: self.shared.commands.clone(),
            failed,
        }
    }

    fn close_reason(&self) -> RpcError {
        self.closed
            .borrow()
            .clone()
            .unwrap_or(RpcError::Disconnected)
    }
}

/// Values of a streaming request. Ends after the server's exit; a failure arrives as a final
/// `Err` item.
#[derive(Debug)]
pub struct RpcStream {
    token: u64,
    values: mpsc::UnboundedReceiver<Result<Value, RpcError>>,
    commands: mpsc::UnboundedSender<Command>,
    failed: Option<RpcError>,
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Request { token, tag, .. } => write!(f, "Request({token}, {tag})"),
            Self::Cancel { token } => write!(f, "Cancel({token})"),
        }
    }
}

impl Stream for RpcStream {
    type Item = Result<Value, RpcError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if let Some(error) = self.failed.take() {
            self.values.close();
            return Poll::Ready(Some(Err(error)));
        }
        self.values.poll_recv(cx)
    }
}

impl Drop for RpcStream {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Cancel { token: self.token });
    }
}

struct Actor {
    session: RpcSession,
    replies: HashMap<RequestId, Reply>,
    tokens: HashMap<u64, RequestId>,
    ids: HashMap<RequestId, u64>,
}

impl Actor {
    fn finish(&mut self, id: &RequestId) -> Option<Reply> {
        if let Some(token) = self.ids.remove(id) {
            self.tokens.remove(&token);
        }
        self.replies.remove(id)
    }

    /// Applies session events. Returns messages to send back.
    fn dispatch(&mut self, events: Vec<SessionEvent>) -> Vec<ClientMessage> {
        let mut outgoing = Vec::new();
        for event in events {
            match event {
                SessionEvent::Chunk { id, values, ack } => {
                    let delivered = match self.replies.get(&id) {
                        Some(Reply::Stream(sink)) => {
                            values.into_iter().all(|value| sink.send(Ok(value)).is_ok())
                        }
                        _ => false,
                    };
                    if delivered {
                        outgoing.push(ack);
                    } else {
                        self.finish(&id);
                        outgoing.extend(self.session.cancel(&id));
                    }
                }
                SessionEvent::Completed { id, value } => match self.finish(&id) {
                    Some(Reply::Unary(reply)) => {
                        let _ = reply.send(Ok(value));
                    }
                    Some(Reply::Stream(_)) | None => {}
                },
                SessionEvent::Failed { id, error } => match self.finish(&id) {
                    Some(Reply::Unary(reply)) => {
                        let _ = reply.send(Err(error));
                    }
                    Some(Reply::Stream(sink)) => {
                        let _ = sink.send(Err(error));
                    }
                    None => {}
                },
                SessionEvent::Pong => {}
            }
        }
        outgoing
    }

    fn fail_all(&mut self, error: &RpcError) {
        let events = self.session.fail_all(error);
        self.dispatch(events);
        // Requests the session no longer tracked still hold replies; drop them.
        self.replies.clear();
        self.tokens.clear();
        self.ids.clear();
    }
}

async fn send(socket: &mut Socket, message: &ClientMessage) -> Result<(), RpcError> {
    socket
        .send(Message::text(encode_message(message)))
        .await
        .map_err(|_| RpcError::Disconnected)
}

async fn run(
    mut socket: Socket,
    mut commands: mpsc::UnboundedReceiver<Command>,
    closed: watch::Sender<Option<RpcError>>,
    keepalive: Keepalive,
) {
    let mut actor = Actor {
        session: RpcSession::new(),
        replies: HashMap::new(),
        tokens: HashMap::new(),
        ids: HashMap::new(),
    };
    let mut ping = tokio::time::interval_at(
        Instant::now() + keepalive.ping_interval,
        keepalive.ping_interval,
    );
    let mut last_frame = Instant::now();

    let reason = loop {
        let idle_deadline = last_frame + keepalive.idle_timeout;
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else {
                    // Every handle and stream is gone.
                    let _ = socket.close(None).await;
                    break RpcError::Disconnected;
                };
                let outgoing = match command {
                    Command::Request { token, tag, payload, reply } => {
                        let kind = match reply {
                            Reply::Unary(_) => RequestKind::Unary,
                            Reply::Stream(_) => RequestKind::Stream,
                        };
                        let (id, message) = actor.session.start(tag, payload, kind);
                        actor.replies.insert(id.clone(), reply);
                        actor.tokens.insert(token, id.clone());
                        actor.ids.insert(id, token);
                        Some(message)
                    }
                    Command::Cancel { token } => match actor.tokens.remove(&token) {
                        Some(id) => {
                            actor.ids.remove(&id);
                            actor.replies.remove(&id);
                            actor.session.cancel(&id)
                        }
                        None => None,
                    },
                };
                if let Some(message) = outgoing
                    && let Err(error) = send(&mut socket, &message).await
                {
                    break error;
                }
            }
            frame = socket.next() => {
                last_frame = Instant::now();
                let text = match frame {
                    None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break RpcError::Disconnected,
                    Some(Ok(Message::Text(text))) => text.as_str().to_owned(),
                    Some(Ok(Message::Binary(bytes))) => match String::from_utf8(bytes.to_vec()) {
                        Ok(text) => text,
                        Err(_) => break RpcError::Protocol("binary frame is not UTF-8".into()),
                    },
                    Some(Ok(_)) => continue,
                };
                let messages = match decode_frame(&text) {
                    Ok(messages) => messages,
                    Err(error) => break RpcError::Protocol(error.to_string()),
                };
                let mut outgoing = Vec::new();
                for message in messages {
                    let events = actor.session.handle(message);
                    outgoing.extend(actor.dispatch(events));
                }
                let mut failed = None;
                for message in &outgoing {
                    if let Err(error) = send(&mut socket, message).await {
                        failed = Some(error);
                        break;
                    }
                }
                if let Some(error) = failed {
                    break error;
                }
            }
            _ = ping.tick() => {
                if let Err(error) = send(&mut socket, &ClientMessage::Ping).await {
                    break error;
                }
            }
            () = tokio::time::sleep_until(idle_deadline) => {
                tracing::warn!("websocket silent for {:?}; dropping it", keepalive.idle_timeout);
                break RpcError::Disconnected;
            }
        }
    };
    tracing::debug!(%reason, "websocket closed");
    actor.fail_all(&reason);
    let _ = closed.send(Some(reason));
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use tokio::net::TcpListener;

    /// A scripted server: each test drives the socket by hand.
    pub(crate) struct TestServer {
        pub url: String,
        listener: TcpListener,
    }

    impl TestServer {
        pub(crate) async fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("ws://{}/ws", listener.local_addr().unwrap());
            Self { url, listener }
        }

        pub(crate) async fn accept(&self) -> ServerSide {
            let (stream, _) = self.listener.accept().await.unwrap();
            ServerSide {
                socket: tokio_tungstenite::accept_async(stream).await.unwrap(),
            }
        }
    }

    pub(crate) struct ServerSide {
        pub socket: WebSocketStream<TcpStream>,
    }

    impl ServerSide {
        /// The next client message, skipping pings unless asked for.
        pub(crate) async fn recv(&mut self) -> Value {
            loop {
                match self.socket.next().await {
                    Some(Ok(Message::Text(text))) => {
                        let value: Value = serde_json::from_str(text.as_str()).unwrap();
                        if value["_tag"] != "Ping" {
                            return value;
                        }
                    }
                    Some(Ok(_)) => {}
                    other => panic!("socket ended: {other:?}"),
                }
            }
        }

        pub(crate) async fn recv_raw(&mut self) -> Option<Value> {
            loop {
                match self.socket.next().await {
                    Some(Ok(Message::Text(text))) => {
                        return Some(serde_json::from_str(text.as_str()).unwrap());
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return None,
                    Some(Ok(_)) => {}
                }
            }
        }

        pub(crate) async fn send(&mut self, value: Value) {
            self.socket
                .send(Message::text(value.to_string()))
                .await
                .unwrap();
        }

        pub(crate) async fn exit(&mut self, id: &Value, value: Value) {
            self.send(json!({"_tag": "Exit", "requestId": id,
                "exit": {"_tag": "Success", "value": value}}))
                .await;
        }

        pub(crate) async fn chunk(&mut self, id: &Value, values: Vec<Value>) {
            self.send(json!({"_tag": "Chunk", "requestId": id, "values": values}))
                .await;
        }
    }

    async fn pair(keepalive: Keepalive) -> (Connection, ServerSide) {
        let server = TestServer::start().await;
        let (connection, side) = tokio::join!(
            Connection::connect(&server.url, Duration::from_secs(5), keepalive),
            server.accept()
        );
        (connection.unwrap(), side)
    }

    async fn connected() -> (Connection, ServerSide) {
        pair(Keepalive {
            ping_interval: Duration::from_secs(60),
            idle_timeout: Duration::from_secs(120),
        })
        .await
    }

    #[tokio::test]
    async fn unary_requests_resolve_with_the_exit_value() {
        let (connection, mut server) = connected().await;
        let call = tokio::spawn({
            let connection = connection.clone();
            async move { connection.request("server.getConfig", json!({})).await }
        });
        let request = server.recv().await;
        assert_eq!(
            request,
            json!({"_tag": "Request", "id": "1", "tag": "server.getConfig", "payload": {}, "headers": []})
        );
        server.exit(&request["id"], json!({"cwd": "/"})).await;
        assert_eq!(call.await.unwrap(), Ok(json!({"cwd": "/"})));
    }

    #[tokio::test]
    async fn concurrent_requests_are_matched_by_id() {
        let (connection, mut server) = connected().await;
        let first = tokio::spawn({
            let connection = connection.clone();
            async move { connection.request("a", json!(1)).await }
        });
        let a = server.recv().await;
        let second = tokio::spawn({
            let connection = connection.clone();
            async move { connection.request("b", json!(2)).await }
        });
        let b = server.recv().await;
        // Answer out of order, both in one array frame.
        server
            .send(json!([
                {"_tag": "Exit", "requestId": b["id"], "exit": {"_tag": "Success", "value": "B"}},
                {"_tag": "Exit", "requestId": a["id"], "exit": {"_tag": "Success", "value": "A"}}
            ]))
            .await;
        assert_eq!(first.await.unwrap(), Ok(json!("A")));
        assert_eq!(second.await.unwrap(), Ok(json!("B")));
    }

    #[tokio::test]
    async fn typed_failures_reach_the_caller() {
        let (connection, mut server) = connected().await;
        let call = tokio::spawn(async move { connection.request("x", json!({})).await });
        let request = server.recv().await;
        server
            .send(
                json!({"_tag": "Exit", "requestId": request["id"], "exit": {"_tag": "Failure",
                "cause": [{"_tag": "Fail", "error": {"_tag": "NotAllowed", "message": "nope"}}]}}),
            )
            .await;
        let error = call.await.unwrap().unwrap_err();
        assert_eq!(error.tag(), Some("NotAllowed"));
        assert_eq!(error.to_string(), "nope");
    }

    #[tokio::test]
    async fn streams_deliver_chunks_ack_each_and_end_on_exit() {
        let (connection, mut server) = connected().await;
        let mut stream = connection.stream("orchestration.subscribeShell", json!({}));
        let request = server.recv().await;
        let id = request["id"].clone();
        server.chunk(&id, vec![json!(1), json!(2)]).await;
        assert_eq!(stream.next().await, Some(Ok(json!(1))));
        assert_eq!(stream.next().await, Some(Ok(json!(2))));
        assert_eq!(server.recv().await, json!({"_tag": "Ack", "requestId": id}));
        server.chunk(&id, vec![json!(3)]).await;
        assert_eq!(stream.next().await, Some(Ok(json!(3))));
        assert_eq!(server.recv().await, json!({"_tag": "Ack", "requestId": id}));
        server.exit(&id, Value::Null).await;
        assert_eq!(stream.next().await, None);
    }

    #[tokio::test]
    async fn stream_failures_arrive_as_a_final_error() {
        let (connection, mut server) = connected().await;
        let mut stream = connection.stream("s", json!({}));
        let id = server.recv().await["id"].clone();
        server
            .send(
                json!({"_tag": "Exit", "requestId": id, "exit": {"_tag": "Failure",
                "cause": [{"_tag": "Die", "defect": "boom"}]}}),
            )
            .await;
        assert!(matches!(
            stream.next().await,
            Some(Err(RpcError::Defect { .. }))
        ));
        assert_eq!(stream.next().await, None);
    }

    #[tokio::test]
    async fn dropping_a_stream_interrupts_it() {
        let (connection, mut server) = connected().await;
        let stream = connection.stream("s", json!({}));
        let id = server.recv().await["id"].clone();
        drop(stream);
        assert_eq!(
            server.recv().await,
            json!({"_tag": "Interrupt", "requestId": id})
        );
        // Late chunks for it are ignored and not acked.
        server.chunk(&id, vec![json!(1)]).await;
        let call = tokio::spawn(async move { connection.request("after", json!({})).await });
        let next = server.recv().await;
        assert_eq!(next["tag"], "after");
        server.exit(&next["id"], json!(true)).await;
        assert_eq!(call.await.unwrap(), Ok(json!(true)));
    }

    #[tokio::test]
    async fn sends_pings_and_drops_a_silent_server() {
        let (connection, mut server) = pair(Keepalive {
            ping_interval: Duration::from_millis(50),
            idle_timeout: Duration::from_millis(300),
        })
        .await;
        assert_eq!(server.recv_raw().await, Some(json!({"_tag": "Ping"})));
        // Pongs keep it alive past the idle timeout.
        for _ in 0..8 {
            server.send(json!({"_tag": "Pong"})).await;
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(!connection.is_closed());
        // Silence ends it.
        let started = std::time::Instant::now();
        assert_eq!(connection.closed().await, RpcError::Disconnected);
        assert!(started.elapsed() >= Duration::from_millis(150));
    }

    #[tokio::test]
    async fn server_close_fails_pending_requests() {
        let (connection, mut server) = connected().await;
        let call = tokio::spawn({
            let connection = connection.clone();
            async move { connection.request("x", json!({})).await }
        });
        let mut stream = connection.stream("s", json!({}));
        server.recv().await;
        server.recv().await;
        server.socket.close(None).await.unwrap();
        assert_eq!(call.await.unwrap(), Err(RpcError::Disconnected));
        assert_eq!(stream.next().await, Some(Err(RpcError::Disconnected)));
        assert_eq!(connection.closed().await, RpcError::Disconnected);
        assert!(connection.is_closed());
        // New requests fail fast.
        assert_eq!(
            connection.request("y", json!({})).await,
            Err(RpcError::Disconnected)
        );
        let mut late = connection.stream("z", json!({}));
        assert_eq!(late.next().await, Some(Err(RpcError::Disconnected)));
        assert_eq!(late.next().await, None);
    }

    #[tokio::test]
    async fn malformed_frames_close_with_a_protocol_error() {
        let (connection, mut server) = connected().await;
        server.socket.send(Message::text("{nope")).await.unwrap();
        assert!(matches!(connection.closed().await, RpcError::Protocol(_)));
    }

    #[tokio::test]
    async fn defects_fail_everything_in_flight() {
        let (connection, mut server) = connected().await;
        let call = tokio::spawn({
            let connection = connection.clone();
            async move { connection.request("x", json!({})).await }
        });
        server.recv().await;
        server
            .send(json!({"_tag": "Defect", "defect": "server exploded"}))
            .await;
        assert_eq!(
            call.await.unwrap(),
            Err(RpcError::defect(json!("server exploded")))
        );
        assert!(!connection.is_closed());
    }

    #[tokio::test]
    async fn binary_frames_with_json_are_accepted() {
        let (connection, mut server) = connected().await;
        let call = tokio::spawn(async move { connection.request("x", json!({})).await });
        let id = server.recv().await["id"].clone();
        let frame =
            json!({"_tag": "Exit", "requestId": id, "exit": {"_tag": "Success", "value": 5}});
        server
            .socket
            .send(Message::binary(frame.to_string().into_bytes()))
            .await
            .unwrap();
        assert_eq!(call.await.unwrap(), Ok(json!(5)));
    }

    #[tokio::test]
    async fn dropping_every_handle_closes_the_socket() {
        let (connection, mut server) = connected().await;
        drop(connection);
        assert_eq!(server.recv_raw().await, None);
    }

    #[tokio::test]
    async fn rejected_upgrades_report_the_status() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/ws", listener.local_addr().unwrap());
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = [0u8; 1024];
            let _ = stream.read(&mut buffer).await;
            let _ = stream
                .write_all(b"HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\n\r\n")
                .await;
        });
        let error = Connection::connect(&url, Duration::from_secs(5), Keepalive::default())
            .await
            .unwrap_err();
        assert_eq!(error, ConnectError::Rejected { status: 401 });
        assert!(error.is_unauthorized());
    }

    #[tokio::test]
    async fn connect_times_out_when_the_server_never_answers() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/ws?wsTicket=secret", listener.local_addr().unwrap());
        let error = Connection::connect(&url, Duration::from_millis(100), Keepalive::default())
            .await
            .unwrap_err();
        assert!(matches!(&error, ConnectError::Timeout { url } if !url.contains("secret")));
        drop(listener);
    }

    #[tokio::test]
    async fn refused_connections_are_io_errors() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/ws", listener.local_addr().unwrap());
        drop(listener);
        let error = Connection::connect(&url, Duration::from_secs(5), Keepalive::default())
            .await
            .unwrap_err();
        assert!(matches!(error, ConnectError::Io { .. }));
    }
}
