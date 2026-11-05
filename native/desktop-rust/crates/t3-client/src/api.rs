//! Typed calls for the RPC methods the desktop app uses.

use std::marker::PhantomData;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::Stream;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use t3_contracts::{
    Command, LaunchThreadInput, ProjectCreate, ProjectShell, ServerConfig, ShellStreamItem,
    ThreadId, ThreadProjection, ThreadStreamItem, methods,
};
use t3_protocol::RpcError;

use crate::transport::{Connection, RpcStream};

/// `OrchestrationV2ThreadLaunchResult`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchThreadResult {
    pub thread_id: ThreadId,
    pub projection: Box<ThreadProjection>,
    pub resumed: bool,
    /// The initial message's commit sequence. Older servers omit it.
    #[serde(default)]
    pub sequence: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
struct DispatchResult {
    sequence: u64,
}

fn decode<T: DeserializeOwned>(method: &str, value: Value) -> Result<T, RpcError> {
    serde_json::from_value(value).map_err(|error| {
        RpcError::Protocol(format!("{method} returned an unexpected value: {error}"))
    })
}

/// The `orchestration.subscribeShell` payload. The completion marker makes the server send
/// `synchronized` once it has caught up.
pub fn subscribe_shell_payload(after_sequence: Option<u64>) -> Value {
    let mut payload = json!({"requestCompletionMarker": true});
    if let Some(sequence) = after_sequence {
        payload["afterSequence"] = json!(sequence);
    }
    payload
}

/// The `orchestration.subscribeThread` payload. The client asks for full snapshots: it sets none
/// of the `accept*` flags.
pub fn subscribe_thread_payload(thread_id: &ThreadId, after_sequence: Option<u64>) -> Value {
    let mut payload = json!({"threadId": thread_id, "requestCompletionMarker": true});
    if let Some(sequence) = after_sequence {
        payload["afterSequence"] = json!(sequence);
    }
    payload
}

impl Connection {
    pub async fn get_config(&self) -> Result<ServerConfig, RpcError> {
        let value = self.request(methods::SERVER_GET_CONFIG, json!({})).await?;
        decode(methods::SERVER_GET_CONFIG, value)
    }

    /// Dispatches a command and returns the sequence it committed at.
    pub async fn dispatch_command(&self, command: &Command) -> Result<u64, RpcError> {
        let payload = serde_json::to_value(command)
            .map_err(|error| RpcError::Protocol(format!("could not encode command: {error}")))?;
        let value = self.request(methods::DISPATCH_COMMAND, payload).await?;
        decode::<DispatchResult>(methods::DISPATCH_COMMAND, value).map(|result| result.sequence)
    }

    pub async fn launch_thread(
        &self,
        input: &LaunchThreadInput,
    ) -> Result<LaunchThreadResult, RpcError> {
        let payload = serde_json::to_value(input).map_err(|error| {
            RpcError::Protocol(format!("could not encode launch input: {error}"))
        })?;
        let value = self.request(methods::LAUNCH_THREAD, payload).await?;
        decode(methods::LAUNCH_THREAD, value)
    }

    pub async fn get_thread_projection(
        &self,
        thread_id: &ThreadId,
    ) -> Result<ThreadProjection, RpcError> {
        let value = self
            .request(
                methods::GET_THREAD_PROJECTION,
                json!({"threadId": thread_id}),
            )
            .await?;
        decode(methods::GET_THREAD_PROJECTION, value)
    }

    /// Adds a project for a folder on the server's machine, creating the folder if needed.
    pub async fn create_project(&self, command: &ProjectCreate) -> Result<ProjectShell, RpcError> {
        let payload = serde_json::to_value(command)
            .map_err(|error| RpcError::Protocol(format!("could not encode project: {error}")))?;
        let value = self.request(methods::PROJECTS_MUTATE, payload).await?;
        decode(methods::PROJECTS_MUTATE, value)
    }

    pub fn subscribe_shell(&self, after_sequence: Option<u64>) -> TypedStream<ShellStreamItem> {
        TypedStream::new(
            methods::SUBSCRIBE_SHELL,
            self.stream(
                methods::SUBSCRIBE_SHELL,
                subscribe_shell_payload(after_sequence),
            ),
        )
    }

    pub fn subscribe_thread(
        &self,
        thread_id: &ThreadId,
        after_sequence: Option<u64>,
    ) -> TypedStream<ThreadStreamItem> {
        TypedStream::new(
            methods::SUBSCRIBE_THREAD,
            self.stream(
                methods::SUBSCRIBE_THREAD,
                subscribe_thread_payload(thread_id, after_sequence),
            ),
        )
    }
}

/// An [`RpcStream`] whose values decode as `T`. An undecodable value becomes an `Err` item.
#[derive(Debug)]
pub struct TypedStream<T> {
    method: &'static str,
    inner: RpcStream,
    _item: PhantomData<fn() -> T>,
}

impl<T> TypedStream<T> {
    fn new(method: &'static str, inner: RpcStream) -> Self {
        Self {
            method,
            inner,
            _item: PhantomData,
        }
    }
}

impl<T: DeserializeOwned> Stream for TypedStream<T> {
    type Item = Result<T, RpcError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let method = self.method;
        Pin::new(&mut self.inner)
            .poll_next(cx)
            .map(|item| item.map(|result| result.and_then(|value| decode(method, value))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::Keepalive;
    use crate::transport::tests::TestServer;
    use futures::StreamExt;
    use pretty_assertions::assert_eq;
    use std::time::Duration;
    use t3_contracts::ShellSnapshot;

    #[test]
    fn subscription_payloads() {
        assert_eq!(
            subscribe_shell_payload(None),
            json!({"requestCompletionMarker": true})
        );
        assert_eq!(
            subscribe_shell_payload(Some(7)),
            json!({"requestCompletionMarker": true, "afterSequence": 7})
        );
        let thread = ThreadId::new("t-1");
        assert_eq!(
            subscribe_thread_payload(&thread, None),
            json!({"threadId": "t-1", "requestCompletionMarker": true})
        );
        assert_eq!(
            subscribe_thread_payload(&thread, Some(0)),
            json!({"threadId": "t-1", "requestCompletionMarker": true, "afterSequence": 0})
        );
    }

    async fn connected() -> (Connection, crate::transport::tests::ServerSide) {
        let server = TestServer::start().await;
        let (connection, side) = tokio::join!(
            Connection::connect(&server.url, Duration::from_secs(5), Keepalive::default()),
            server.accept()
        );
        (connection.unwrap(), side)
    }

    fn shell_snapshot_json() -> Value {
        json!({"schemaVersion": 1, "snapshotSequence": 3, "threads": [], "archivedThreads": [],
            "projects": []})
    }

    #[tokio::test]
    async fn get_config_decodes_the_server_config() {
        let (connection, mut server) = connected().await;
        let call = tokio::spawn(async move { connection.get_config().await });
        let request = server.recv().await;
        assert_eq!(request["tag"], "server.getConfig");
        server
            .exit(
                &request["id"],
                json!({"environment": {"label": "Box"}, "cwd": "/home", "providers": []}),
            )
            .await;
        let config = call.await.unwrap().unwrap();
        assert_eq!(config.environment_label(), Some("Box"));
    }

    #[tokio::test]
    async fn undecodable_results_are_protocol_errors() {
        let (connection, mut server) = connected().await;
        let call = tokio::spawn(async move { connection.get_config().await });
        let request = server.recv().await;
        server.exit(&request["id"], json!("not a config")).await;
        let error = call.await.unwrap().unwrap_err();
        assert!(
            matches!(&error, RpcError::Protocol(message) if message.starts_with("server.getConfig"))
        );
    }

    #[tokio::test]
    async fn dispatch_sends_the_command_and_returns_its_sequence() {
        let (connection, mut server) = connected().await;
        let thread = ThreadId::new("thread-1");
        let command = Command::ThreadStop {
            command_id: t3_contracts::CommandId::new("c-1"),
            thread_id: thread.clone(),
        };
        let expected_payload = serde_json::to_value(&command).unwrap();
        let call = tokio::spawn(async move { connection.dispatch_command(&command).await });
        let request = server.recv().await;
        assert_eq!(request["tag"], "orchestration.dispatchCommand");
        assert_eq!(request["payload"], expected_payload);
        server.exit(&request["id"], json!({"sequence": 42})).await;
        assert_eq!(call.await.unwrap(), Ok(42));
    }

    #[tokio::test]
    async fn get_thread_projection_round_trips() {
        let (connection, mut server) = connected().await;
        let call = tokio::spawn(async move {
            connection
                .get_thread_projection(&ThreadId::new("thread-1"))
                .await
        });
        let request = server.recv().await;
        assert_eq!(request["payload"], json!({"threadId": "thread-1"}));
        server
            .exit(
                &request["id"],
                t3_contracts::fixtures::projection_json("thread-1"),
            )
            .await;
        let projection = call.await.unwrap().unwrap();
        assert_eq!(
            projection.thread_id().map(|id| id.as_str().to_owned()),
            Some("thread-1".to_owned())
        );
    }

    #[tokio::test]
    async fn launch_thread_decodes_the_result() {
        let (connection, mut server) = connected().await;
        let call = tokio::spawn(async move {
            let request = connection
                .request(methods::LAUNCH_THREAD, json!({}))
                .await?;
            decode::<LaunchThreadResult>(methods::LAUNCH_THREAD, request)
        });
        let request = server.recv().await;
        server
            .exit(&request["id"], json!({"threadId": "thread-1", "projection": t3_contracts::fixtures::projection_json("thread-1"),
                "resumed": false}))
            .await;
        let result = call.await.unwrap().unwrap();
        assert_eq!(result.thread_id.as_str(), "thread-1");
        assert_eq!(result.sequence, None);
        assert!(!result.resumed);
    }

    #[tokio::test]
    async fn shell_subscription_decodes_items() {
        let (connection, mut server) = connected().await;
        let mut stream = connection.subscribe_shell(Some(2));
        let request = server.recv().await;
        assert_eq!(
            request["payload"],
            json!({"requestCompletionMarker": true, "afterSequence": 2})
        );
        let id = request["id"].clone();
        server
            .chunk(
                &id,
                vec![
                    json!({"kind": "snapshot", "snapshot": shell_snapshot_json()}),
                    json!({"kind": "synchronized"}),
                    json!({"kind": "something.new"}),
                    json!({"kind": "snapshot"}),
                ],
            )
            .await;
        let Some(Ok(ShellStreamItem::Snapshot { snapshot, .. })) = stream.next().await else {
            panic!("expected a snapshot");
        };
        let expected: ShellSnapshot = serde_json::from_value(shell_snapshot_json()).unwrap();
        assert_eq!(*snapshot, expected);
        assert_eq!(stream.next().await, Some(Ok(ShellStreamItem::Synchronized)));
        assert_eq!(stream.next().await, Some(Ok(ShellStreamItem::Unknown)));
        assert!(matches!(
            stream.next().await,
            Some(Err(RpcError::Protocol(_)))
        ));
    }

    #[tokio::test]
    async fn thread_subscription_sends_the_thread_id() {
        let (connection, mut server) = connected().await;
        let mut stream = connection.subscribe_thread(&ThreadId::new("thread-9"), None);
        let request = server.recv().await;
        assert_eq!(request["tag"], "orchestration.subscribeThread");
        assert_eq!(
            request["payload"],
            json!({"threadId": "thread-9", "requestCompletionMarker": true})
        );
        server
            .chunk(&request["id"], vec![json!({"kind": "synchronized"})])
            .await;
        assert_eq!(
            stream.next().await,
            Some(Ok(ThreadStreamItem::Synchronized))
        );
    }
}
