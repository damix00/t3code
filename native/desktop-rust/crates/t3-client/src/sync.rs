//! Keeps one environment's shell and the open thread current across reconnects.
//!
//! The engine runs on tokio. It owns the connection loop: connect, read the server config,
//! subscribe to the shell (resuming from the last sequence it saw) and to the open thread, and
//! on any disconnect retry with upstream's jittered exponential backoff. The UI reads
//! [`SyncState`] through a [`SyncHandle`] and wakes on its revision counter, so it can wait from
//! any executor.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures::StreamExt;
use t3_contracts::{ServerConfig, ShellStreamItem, ThreadId, ThreadStreamItem};
use t3_state::shell::ShellState;
use t3_state::thread::{ThreadState, ThreadStatus};
use tokio::sync::{mpsc, watch};

use crate::api::TypedStream;
use crate::transport::Connection;

const RETRY_BASE_DELAY: Duration = Duration::from_secs(1);
const RETRY_MAX_DELAY: Duration = Duration::from_secs(300);

/// `retryDelayMs` from upstream's connection supervisor: a ceiling that doubles per failure
/// from two seconds up to five minutes, and a delay drawn from its upper half.
/// `random` is in `[0, 1)`.
pub fn retry_delay(failure_count: u32, random: f64, base: Duration, max: Duration) -> Duration {
    let factor = 2f64.powi(failure_count.saturating_add(1).min(64) as i32);
    let ceiling = (base.as_secs_f64() * 1000.0 * factor).min(max.as_secs_f64() * 1000.0);
    let millis = (ceiling / 2.0 + ceiling / 2.0 * random.clamp(0.0, 1.0)).round();
    Duration::from_millis(millis as u64)
}

/// Connects to an environment: authorizes, mints a ticket, opens the socket.
pub trait Connector: Send + Sync + 'static {
    fn connect(&self) -> Pin<Box<dyn Future<Output = Result<Connection, String>> + Send + '_>>;
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConnectionStatus {
    Connecting {
        attempt: u32,
    },
    Connected,
    /// The last attempt failed; the next starts after `retry_in`.
    Waiting {
        attempt: u32,
        retry_in: Duration,
        error: String,
    },
    Stopped,
}

/// The thread the person has open, and its stream state.
#[derive(Debug, Clone)]
pub struct OpenThread {
    pub id: ThreadId,
    pub state: ThreadState,
    /// Why the subscription ended, if it failed on its own.
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SyncState {
    pub status: ConnectionStatus,
    pub config: Option<ServerConfig>,
    pub shell: ShellState,
    pub thread: Option<OpenThread>,
}

impl Default for SyncState {
    fn default() -> Self {
        Self {
            status: ConnectionStatus::Connecting { attempt: 1 },
            config: None,
            shell: ShellState::default(),
            thread: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncOptions {
    pub retry_base_delay: Duration,
    pub retry_max_delay: Duration,
}

impl Default for SyncOptions {
    fn default() -> Self {
        Self {
            retry_base_delay: RETRY_BASE_DELAY,
            retry_max_delay: RETRY_MAX_DELAY,
        }
    }
}

#[derive(Debug)]
enum EngineCommand {
    ThreadChanged,
    ReconnectNow,
    Shutdown,
}

#[derive(Debug)]
struct Shared {
    state: Mutex<SyncState>,
    connection: Mutex<Option<Connection>>,
    revision: watch::Sender<u64>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, SyncState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn notify(&self) {
        self.revision.send_modify(|revision| *revision += 1);
    }

    /// Changes state and wakes readers when `change` reports a visible change.
    fn update(&self, change: impl FnOnce(&mut SyncState) -> bool) {
        let changed = change(&mut self.lock());
        if changed {
            self.notify();
        }
    }

    fn set_status(&self, status: ConnectionStatus) {
        self.update(|state| {
            let changed = state.status != status;
            state.status = status;
            changed
        });
    }

    fn set_connection(&self, connection: Option<Connection>) {
        *self
            .connection
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = connection;
    }
}

/// The UI's view of a running engine. Dropping every handle stops the engine.
#[derive(Debug, Clone)]
pub struct SyncHandle {
    shared: Arc<Shared>,
    commands: mpsc::UnboundedSender<EngineCommand>,
}

impl SyncHandle {
    /// Reads the current state. Keep the closure short: the engine waits on the same lock.
    pub fn read<R>(&self, read: impl FnOnce(&SyncState) -> R) -> R {
        read(&self.shared.lock())
    }

    /// A counter that increases on every visible change. `changed().await` works on any
    /// executor.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.shared.revision.subscribe()
    }

    /// The live connection, for one-off requests such as dispatching a command.
    pub fn connection(&self) -> Option<Connection> {
        self.shared
            .connection
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .filter(|connection| !connection.is_closed())
    }

    /// Opens a thread (or closes the open one with `None`). Reopening the open thread retries
    /// a failed subscription.
    pub fn open_thread(&self, thread_id: Option<ThreadId>) {
        self.shared.update(|state| {
            state.thread = thread_id.map(|id| OpenThread {
                id,
                state: ThreadState::default(),
                error: None,
            });
            true
        });
        let _ = self.commands.send(EngineCommand::ThreadChanged);
    }

    /// Skips the current backoff, or drops the connection and reconnects.
    pub fn reconnect_now(&self) {
        let _ = self.commands.send(EngineCommand::ReconnectNow);
    }

    pub fn shutdown(&self) {
        let _ = self.commands.send(EngineCommand::Shutdown);
    }
}

/// Starts the engine on the current tokio runtime.
pub fn spawn(connector: impl Connector, options: SyncOptions) -> SyncHandle {
    let (revision, _) = watch::channel(0);
    let shared = Arc::new(Shared {
        state: Mutex::new(SyncState::default()),
        connection: Mutex::new(None),
        revision,
    });
    let (commands, receiver) = mpsc::unbounded_channel();
    tokio::spawn(run(connector, options, Arc::clone(&shared), receiver));
    SyncHandle { shared, commands }
}

enum Next {
    Reconnect { error: Option<String> },
    Stop,
}

async fn run(
    connector: impl Connector,
    options: SyncOptions,
    shared: Arc<Shared>,
    mut commands: mpsc::UnboundedReceiver<EngineCommand>,
) {
    let mut failures: u32 = 0;
    loop {
        shared.set_status(ConnectionStatus::Connecting {
            attempt: failures + 1,
        });
        let attempt = {
            let connect = connector.connect();
            tokio::pin!(connect);
            loop {
                tokio::select! {
                    result = &mut connect => break Some(result),
                    command = commands.recv() => match command {
                        None | Some(EngineCommand::Shutdown) => break None,
                        Some(EngineCommand::ThreadChanged | EngineCommand::ReconnectNow) => {}
                    },
                }
            }
        };
        let next = match attempt {
            None => Next::Stop,
            Some(Err(error)) => Next::Reconnect { error: Some(error) },
            Some(Ok(connection)) => {
                shared.set_connection(Some(connection.clone()));
                let next = serve(&connection, &shared, &mut commands, &mut failures).await;
                shared.set_connection(None);
                next
            }
        };
        let error = match next {
            Next::Stop => break,
            Next::Reconnect { error: None } => continue,
            Next::Reconnect { error: Some(error) } => error,
        };
        let delay = retry_delay(
            failures,
            rand::random::<f64>(),
            options.retry_base_delay,
            options.retry_max_delay,
        );
        failures = failures.saturating_add(1);
        tracing::info!(%error, ?delay, "connection lost; retrying");
        shared.set_status(ConnectionStatus::Waiting {
            attempt: failures,
            retry_in: delay,
            error,
        });
        let sleep = tokio::time::sleep(delay);
        tokio::pin!(sleep);
        let stop = loop {
            tokio::select! {
                () = &mut sleep => break false,
                command = commands.recv() => match command {
                    None | Some(EngineCommand::Shutdown) => break true,
                    Some(EngineCommand::ReconnectNow) => break false,
                    Some(EngineCommand::ThreadChanged) => {}
                },
            }
        };
        if stop {
            break;
        }
    }
    shared.set_status(ConnectionStatus::Stopped);
}

/// Subscribes to the open thread, if any, resuming where its state left off.
fn subscribe_open_thread(
    connection: &Connection,
    shared: &Shared,
) -> Option<(ThreadId, TypedStream<ThreadStreamItem>)> {
    let mut state = shared.lock();
    let open = state.thread.as_mut()?;
    open.state.begin_resync();
    open.error = None;
    let stream = connection.subscribe_thread(&open.id, open.state.resume_sequence());
    Some((open.id.clone(), stream))
}

async fn next_thread_item(
    subscription: &mut Option<(ThreadId, TypedStream<ThreadStreamItem>)>,
) -> Option<Result<ThreadStreamItem, t3_protocol::RpcError>> {
    match subscription {
        Some((_, stream)) => stream.next().await,
        None => std::future::pending().await,
    }
}

async fn serve(
    connection: &Connection,
    shared: &Shared,
    commands: &mut mpsc::UnboundedReceiver<EngineCommand>,
    failures: &mut u32,
) -> Next {
    let config = tokio::select! {
        config = connection.get_config() => config,
        command = commands.recv() => match command {
            None | Some(EngineCommand::Shutdown) => return Next::Stop,
            // Connecting again would only repeat this; the thread subscribes below.
            Some(EngineCommand::ThreadChanged | EngineCommand::ReconnectNow) => connection.get_config().await,
        },
    };
    let config: ServerConfig = match config {
        Ok(config) => config,
        Err(error) => {
            return Next::Reconnect {
                error: Some(format!("could not read server config: {error}")),
            };
        }
    };
    shared.update(|state| {
        state.config = Some(config);
        state.shell.begin_resync();
        if let Some(open) = state.thread.as_mut() {
            open.state.begin_resync();
        }
        true
    });
    shared.set_status(ConnectionStatus::Connected);
    tracing::info!("connected");

    let resume = shared.lock().shell.resume_sequence();
    let mut shell = connection.subscribe_shell(resume);
    let mut thread = subscribe_open_thread(connection, shared);

    loop {
        tokio::select! {
            item = shell.next() => match item {
                Some(Ok(item)) => {
                    *failures = 0;
                    apply_shell(shared, &item);
                }
                Some(Err(error)) => return Next::Reconnect { error: Some(format!("shell subscription failed: {error}")) },
                None => return Next::Reconnect { error: Some("shell subscription ended".into()) },
            },
            item = next_thread_item(&mut thread) => {
                let Some((thread_id, _)) = thread.as_ref() else { continue };
                let thread_id = thread_id.clone();
                match item {
                    Some(Ok(item)) => {
                        if apply_thread(shared, &thread_id, &item) {
                            // The stream sent something unappliable: start over from a snapshot.
                            drop(thread.take());
                            thread = subscribe_open_thread(connection, shared);
                        }
                    }
                    Some(Err(error)) => {
                        if connection.is_closed() {
                            return Next::Reconnect { error: Some(error.to_string()) };
                        }
                        set_thread_error(shared, &thread_id, error.to_string());
                        thread = None;
                    }
                    None => thread = None,
                }
            },
            command = commands.recv() => match command {
                None | Some(EngineCommand::Shutdown) => return Next::Stop,
                Some(EngineCommand::ReconnectNow) => return Next::Reconnect { error: None },
                Some(EngineCommand::ThreadChanged) => {
                    // Dropping the old stream interrupts it on the server.
                    drop(thread.take());
                    thread = subscribe_open_thread(connection, shared);
                }
            },
            reason = connection.closed() => return Next::Reconnect { error: Some(reason.to_string()) },
        }
    }
}

fn apply_shell(shared: &Shared, item: &ShellStreamItem) {
    shared.update(|state| state.shell.apply(item));
}

/// Applies a thread item if that thread is still open. Returns whether it needs a resubscribe.
fn apply_thread(shared: &Shared, thread_id: &ThreadId, item: &ThreadStreamItem) -> bool {
    let mut needs_snapshot = false;
    shared.update(|state| match state.thread.as_mut() {
        Some(open) if &open.id == thread_id => {
            let changed = open.state.apply(item);
            needs_snapshot = open.state.status() == ThreadStatus::NeedsSnapshot;
            changed
        }
        _ => false,
    });
    needs_snapshot
}

fn set_thread_error(shared: &Shared, thread_id: &ThreadId, error: String) {
    shared.update(|state| match state.thread.as_mut() {
        Some(open) if &open.id == thread_id => {
            open.error = Some(error);
            true
        }
        _ => false,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::Keepalive;
    use crate::transport::tests::{ServerSide, TestServer};
    use pretty_assertions::assert_eq;
    use proptest::prelude::*;
    use serde_json::{Value, json};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use t3_contracts::fixtures::{projection_json, thread_shell_json};
    use t3_state::shell::ShellStatus;

    fn ms(millis: u64) -> Duration {
        Duration::from_millis(millis)
    }

    #[test]
    fn retry_delays_match_upstream() {
        let delay =
            |failures, random| retry_delay(failures, random, RETRY_BASE_DELAY, RETRY_MAX_DELAY);
        assert_eq!(delay(0, 0.0), ms(1_000));
        assert_eq!(delay(0, 0.999_999), ms(2_000));
        assert_eq!(delay(1, 0.5), ms(3_000));
        assert_eq!(delay(2, 0.0), ms(4_000));
        assert_eq!(delay(7, 0.0), ms(128_000));
        assert_eq!(delay(8, 0.0), ms(150_000));
        assert_eq!(delay(8, 1.0), ms(300_000));
        assert_eq!(delay(u32::MAX, 0.25), ms(187_500));
    }

    proptest! {
        #[test]
        fn retry_delays_stay_in_the_upper_half_of_the_ceiling(failures in 0u32..80, random in 0f64..1f64) {
            let delay = retry_delay(failures, random, RETRY_BASE_DELAY, RETRY_MAX_DELAY).as_millis() as f64;
            let ceiling = (1000.0 * 2f64.powi(failures as i32 + 1)).min(300_000.0);
            prop_assert!(delay >= (ceiling / 2.0).floor());
            prop_assert!(delay <= ceiling);
        }

        #[test]
        fn retry_delays_never_shrink_as_failures_grow(failures in 0u32..40, random in 0f64..1f64) {
            let now = retry_delay(failures, random, RETRY_BASE_DELAY, RETRY_MAX_DELAY);
            let next = retry_delay(failures + 1, random, RETRY_BASE_DELAY, RETRY_MAX_DELAY);
            prop_assert!(next >= now);
        }
    }

    /// Connects to a test server, or fails the first `fail_first` attempts.
    struct TestConnector {
        url: String,
        fail_first: usize,
        attempts: Arc<AtomicUsize>,
    }

    impl Connector for TestConnector {
        fn connect(&self) -> Pin<Box<dyn Future<Output = Result<Connection, String>> + Send + '_>> {
            Box::pin(async move {
                let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
                if attempt < self.fail_first {
                    return Err(format!("attempt {attempt} refused"));
                }
                Connection::connect(&self.url, Duration::from_secs(5), Keepalive::default())
                    .await
                    .map_err(|error| error.to_string())
            })
        }
    }

    fn fast() -> SyncOptions {
        SyncOptions {
            retry_base_delay: ms(10),
            retry_max_delay: ms(40),
        }
    }

    async fn start(fail_first: usize) -> (SyncHandle, TestServer, Arc<AtomicUsize>) {
        let server = TestServer::start().await;
        let attempts = Arc::new(AtomicUsize::new(0));
        let connector = TestConnector {
            url: server.url.clone(),
            fail_first,
            attempts: Arc::clone(&attempts),
        };
        (spawn(connector, fast()), server, attempts)
    }

    async fn wait_for(handle: &SyncHandle, what: &str, check: impl Fn(&SyncState) -> bool) {
        let mut revision = handle.subscribe();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if handle.read(&check) {
                return;
            }
            if tokio::time::timeout_at(deadline, revision.changed())
                .await
                .is_err()
            {
                panic!(
                    "timed out waiting for {what}: {:?}",
                    handle.read(|state| state.status.clone())
                );
            }
        }
    }

    fn shell_snapshot(sequence: u64, threads: Vec<Value>) -> Value {
        json!({"kind": "snapshot", "snapshot": {"schemaVersion": 1, "snapshotSequence": sequence,
            "threads": threads, "archivedThreads": [],
            "projects": [{"id": "project-1", "title": "P", "workspaceRoot": "/p"}]}})
    }

    /// Answers `server.getConfig` and returns the shell subscription request.
    async fn handshake(server: &mut ServerSide) -> Value {
        let config = server.recv().await;
        assert_eq!(config["tag"], "server.getConfig");
        server
            .exit(
                &config["id"],
                json!({"environment": {"label": "Test"}, "cwd": "/", "providers": []}),
            )
            .await;
        let shell = server.recv().await;
        assert_eq!(shell["tag"], "orchestration.subscribeShell");
        shell
    }

    #[tokio::test]
    async fn connects_reads_config_and_follows_the_shell() {
        let (handle, server, _) = start(0).await;
        let mut side = server.accept().await;
        let shell = handshake(&mut side).await;
        assert_eq!(shell["payload"], json!({"requestCompletionMarker": true}));
        side.chunk(
            &shell["id"],
            vec![shell_snapshot(
                5,
                vec![thread_shell_json("thread-1", "project-1")],
            )],
        )
        .await;
        wait_for(&handle, "the snapshot", |state| {
            state.shell.status() == ShellStatus::Live
        })
        .await;
        handle.read(|state| {
            assert_eq!(state.status, ConnectionStatus::Connected);
            assert_eq!(
                state
                    .config
                    .as_ref()
                    .and_then(ServerConfig::environment_label),
                Some("Test")
            );
            assert_eq!(
                state
                    .shell
                    .snapshot()
                    .map(|snapshot| snapshot.threads.len()),
                Some(1)
            );
        });
        assert!(handle.connection().is_some());
        let mut renamed = thread_shell_json("thread-1", "project-1");
        renamed["title"] = json!("Renamed");
        side.chunk(
            &shell["id"],
            vec![json!({"kind": "thread.updated", "sequence": 6,
            "location": "active", "thread": renamed})],
        )
        .await;
        wait_for(&handle, "the rename", |state| {
            state
                .shell
                .snapshot()
                .is_some_and(|snapshot| snapshot.threads[0].title == "Renamed")
        })
        .await;
    }

    #[tokio::test]
    async fn reconnects_and_resumes_the_shell_from_its_sequence() {
        let (handle, server, attempts) = start(0).await;
        let mut side = server.accept().await;
        let shell = handshake(&mut side).await;
        side.chunk(&shell["id"], vec![shell_snapshot(9, vec![])])
            .await;
        wait_for(&handle, "the snapshot", |state| {
            state.shell.status() == ShellStatus::Live
        })
        .await;
        side.socket.close(None).await.unwrap();
        drop(side);
        wait_for(&handle, "the disconnect", |state| {
            matches!(
                state.status,
                ConnectionStatus::Waiting { .. } | ConnectionStatus::Connecting { .. }
            )
        })
        .await;

        let mut side = server.accept().await;
        let shell = handshake(&mut side).await;
        assert_eq!(
            shell["payload"],
            json!({"requestCompletionMarker": true, "afterSequence": 9})
        );
        handle.read(|state| {
            assert_eq!(state.shell.status(), ShellStatus::Synchronizing);
            assert!(
                state.shell.snapshot().is_some(),
                "cached rows stay readable"
            );
        });
        side.chunk(&shell["id"], vec![json!({"kind": "synchronized"})])
            .await;
        wait_for(&handle, "the resync", |state| {
            state.shell.status() == ShellStatus::Live
        })
        .await;
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn backs_off_after_failed_attempts_and_reports_the_error() {
        let (handle, server, attempts) = start(2).await;
        let mut side = server.accept().await;
        assert!(attempts.load(Ordering::SeqCst) >= 3);
        handshake(&mut side).await;
        wait_for(&handle, "connected", |state| {
            state.status == ConnectionStatus::Connected
        })
        .await;
    }

    #[tokio::test]
    async fn waiting_status_names_the_failure() {
        let server = TestServer::start().await;
        let attempts = Arc::new(AtomicUsize::new(0));
        let connector = TestConnector {
            url: server.url.clone(),
            fail_first: usize::MAX,
            attempts,
        };
        let handle = spawn(
            connector,
            SyncOptions {
                retry_base_delay: Duration::from_secs(30),
                retry_max_delay: Duration::from_secs(60),
            },
        );
        wait_for(&handle, "waiting", |state| {
            matches!(state.status, ConnectionStatus::Waiting { .. })
        })
        .await;
        handle.read(|state| {
            let ConnectionStatus::Waiting {
                attempt,
                retry_in,
                error,
            } = &state.status
            else {
                unreachable!()
            };
            assert_eq!(*attempt, 1);
            assert!(*retry_in >= Duration::from_secs(30));
            assert_eq!(error, "attempt 0 refused");
        });
        assert!(handle.connection().is_none());
        handle.shutdown();
        wait_for(&handle, "stopped", |state| {
            state.status == ConnectionStatus::Stopped
        })
        .await;
    }

    #[tokio::test]
    async fn reconnect_now_skips_the_backoff() {
        let server = TestServer::start().await;
        let attempts = Arc::new(AtomicUsize::new(0));
        let connector = TestConnector {
            url: server.url.clone(),
            fail_first: 1,
            attempts: Arc::clone(&attempts),
        };
        let handle = spawn(
            connector,
            SyncOptions {
                retry_base_delay: Duration::from_secs(60),
                retry_max_delay: Duration::from_secs(60),
            },
        );
        wait_for(&handle, "waiting", |state| {
            matches!(state.status, ConnectionStatus::Waiting { .. })
        })
        .await;
        handle.reconnect_now();
        let mut side = tokio::time::timeout(Duration::from_secs(5), server.accept())
            .await
            .unwrap();
        handshake(&mut side).await;
        wait_for(&handle, "connected", |state| {
            state.status == ConnectionStatus::Connected
        })
        .await;
    }

    #[tokio::test]
    async fn opens_switches_and_closes_threads() {
        let (handle, server, _) = start(0).await;
        let mut side = server.accept().await;
        let shell = handshake(&mut side).await;
        side.chunk(&shell["id"], vec![shell_snapshot(1, vec![])])
            .await;
        wait_for(&handle, "live", |state| {
            state.shell.status() == ShellStatus::Live
        })
        .await;
        // The shell chunk's ack.
        assert_eq!(side.recv().await["_tag"], "Ack");

        handle.open_thread(Some(ThreadId::new("thread-1")));
        let subscribe = side.recv().await;
        assert_eq!(subscribe["tag"], "orchestration.subscribeThread");
        assert_eq!(
            subscribe["payload"],
            json!({"threadId": "thread-1", "requestCompletionMarker": true})
        );
        side.chunk(
            &subscribe["id"],
            vec![json!({"kind": "snapshot", "snapshotSequence": 4,
            "projection": projection_json("thread-1"), "hasMoreHistory": false})],
        )
        .await;
        wait_for(&handle, "the projection", |state| {
            state
                .thread
                .as_ref()
                .is_some_and(|open| open.state.status() == ThreadStatus::Live)
        })
        .await;
        assert_eq!(side.recv().await["_tag"], "Ack");

        handle.open_thread(Some(ThreadId::new("thread-2")));
        assert_eq!(
            side.recv().await,
            json!({"_tag": "Interrupt", "requestId": subscribe["id"]})
        );
        let second = side.recv().await;
        assert_eq!(second["payload"]["threadId"], "thread-2");
        handle.read(|state| {
            let open = state.thread.as_ref().unwrap();
            assert_eq!(open.id.as_str(), "thread-2");
            assert!(open.state.projection().is_none());
        });

        handle.open_thread(None);
        assert_eq!(
            side.recv().await,
            json!({"_tag": "Interrupt", "requestId": second["id"]})
        );
        handle.read(|state| assert!(state.thread.is_none()));
    }

    #[tokio::test]
    async fn resubscribes_without_a_cursor_after_an_unappliable_event() {
        let (handle, server, _) = start(0).await;
        handle.open_thread(Some(ThreadId::new("thread-1")));
        let mut side = server.accept().await;
        let shell = handshake(&mut side).await;
        let subscribe = side.recv().await;
        assert_eq!(subscribe["tag"], "orchestration.subscribeThread");
        side.chunk(
            &subscribe["id"],
            vec![json!({"kind": "snapshot", "snapshotSequence": 4,
            "projection": projection_json("thread-1"), "hasMoreHistory": false})],
        )
        .await;
        wait_for(&handle, "live", |state| {
            state
                .thread
                .as_ref()
                .is_some_and(|open| open.state.status() == ThreadStatus::Live)
        })
        .await;
        assert_eq!(side.recv().await["_tag"], "Ack");
        side.chunk(
            &subscribe["id"],
            vec![json!({"kind": "event", "sequence": 5, "event": {
            "id": "e5", "type": "run.created", "threadId": "thread-1",
            "occurredAt": "2026-11-01T00:00:00Z", "payload": "not a run"}})],
        )
        .await;
        // Delivered (so acked), then the stream is dropped and a fresh one starts without a cursor.
        assert_eq!(side.recv().await["_tag"], "Ack");
        assert_eq!(
            side.recv().await,
            json!({"_tag": "Interrupt", "requestId": subscribe["id"]})
        );
        let again = side.recv().await;
        assert_eq!(
            again["payload"],
            json!({"threadId": "thread-1", "requestCompletionMarker": true})
        );
        let _ = shell;
    }

    #[tokio::test]
    async fn thread_failures_stay_on_the_thread_and_keep_the_connection() {
        let (handle, server, attempts) = start(0).await;
        handle.open_thread(Some(ThreadId::new("missing")));
        let mut side = server.accept().await;
        handshake(&mut side).await;
        let subscribe = side.recv().await;
        side.send(json!({"_tag": "Exit", "requestId": subscribe["id"], "exit": {"_tag": "Failure",
            "cause": [{"_tag": "Fail", "error": {"_tag": "ThreadNotFound", "message": "no such thread"}}]}}))
        .await;
        wait_for(&handle, "the thread error", |state| {
            state
                .thread
                .as_ref()
                .is_some_and(|open| open.error.as_deref() == Some("no such thread"))
        })
        .await;
        assert_eq!(
            handle.read(|state| state.status.clone()),
            ConnectionStatus::Connected
        );
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn reconnect_resumes_the_open_thread_from_its_sequence() {
        let (handle, server, _) = start(0).await;
        handle.open_thread(Some(ThreadId::new("thread-1")));
        let mut side = server.accept().await;
        handshake(&mut side).await;
        let subscribe = side.recv().await;
        side.chunk(
            &subscribe["id"],
            vec![json!({"kind": "snapshot", "snapshotSequence": 12,
            "projection": projection_json("thread-1"), "hasMoreHistory": false})],
        )
        .await;
        wait_for(&handle, "live", |state| {
            state
                .thread
                .as_ref()
                .is_some_and(|open| open.state.status() == ThreadStatus::Live)
        })
        .await;
        side.socket.close(None).await.unwrap();
        drop(side);
        let mut side = server.accept().await;
        handshake(&mut side).await;
        let subscribe = side.recv().await;
        assert_eq!(
            subscribe["payload"],
            json!({"threadId": "thread-1", "requestCompletionMarker": true, "afterSequence": 12})
        );
        handle.read(|state| {
            assert_eq!(
                state.thread.as_ref().unwrap().state.status(),
                ThreadStatus::Loading
            );
        });
    }

    #[tokio::test]
    async fn config_failures_trigger_a_reconnect() {
        let (handle, server, attempts) = start(0).await;
        let mut side = server.accept().await;
        let config = side.recv().await;
        side.send(
            json!({"_tag": "Exit", "requestId": config["id"], "exit": {"_tag": "Failure",
            "cause": [{"_tag": "Die", "defect": "db locked"}]}}),
        )
        .await;
        wait_for(&handle, "waiting", |state| {
            matches!(&state.status, ConnectionStatus::Waiting { error, .. } if error.contains("db locked"))
        })
        .await;
        drop(side);
        let mut side = server.accept().await;
        handshake(&mut side).await;
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn dropping_every_handle_stops_the_engine() {
        let (handle, server, _) = start(0).await;
        let mut side = server.accept().await;
        handshake(&mut side).await;
        let mut revision = handle.subscribe();
        let probe = Arc::clone(&handle.shared);
        drop(handle);
        while probe.lock().status != ConnectionStatus::Stopped {
            tokio::time::timeout(Duration::from_secs(5), revision.changed())
                .await
                .unwrap()
                .unwrap();
        }
        // The shell subscription is interrupted, then the socket closes.
        assert_eq!(side.recv().await["_tag"], "Interrupt");
        assert_eq!(side.recv_raw().await, None);
    }
}
