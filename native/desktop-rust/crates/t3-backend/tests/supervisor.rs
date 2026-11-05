//! Runs the supervisor against `fixtures/fake-server.mjs`. Skipped when `node` is not on PATH.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use pretty_assertions::assert_eq;
use serde_json::Value;
use t3_backend::launch::{find_on_path, pick_free_port};
use t3_backend::token::is_valid_token;
use t3_backend::{
    AttachedConnector, Backend, BackendOptions, BackendStatus, LocalConnector, ServerLaunch,
    SessionStore,
};
use t3_client::Connector;
use tempfile::TempDir;

struct Fixture {
    dir: TempDir,
    options: BackendOptions,
}

impl Fixture {
    fn record(&self) -> PathBuf {
        self.dir.path().join("bootstrap.jsonl")
    }

    fn envelopes(&self) -> Vec<Value> {
        std::fs::read_to_string(self.record())
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

fn fixture(mode: &str) -> Option<Fixture> {
    let Some(node) = find_on_path("node", std::env::var_os("PATH").as_deref()) else {
        eprintln!("skipping: node is not on PATH");
        return None;
    };
    let entry = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-server.mjs");
    let dir = tempfile::tempdir().unwrap();
    let mut options = BackendOptions::new(
        ServerLaunch::NodeEntry { node, entry },
        dir.path().join("home"),
        pick_free_port().unwrap(),
    );
    options.env = vec![
        ("FAKE_MODE".into(), mode.into()),
        (
            "FAKE_RECORD".into(),
            dir.path().join("bootstrap.jsonl").display().to_string(),
        ),
        (
            "FAKE_STATE".into(),
            dir.path().join("state").display().to_string(),
        ),
    ];
    options.terminate_grace = Duration::from_millis(500);
    Some(Fixture { dir, options })
}

async fn within<T>(what: &str, future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(20), future)
        .await
        .unwrap_or_else(|_| panic!("timed out: {what}"))
}

#[tokio::test]
async fn starts_the_server_with_a_valid_envelope_and_waits_for_readiness() {
    let Some(fixture) = fixture("ok") else { return };
    let backend = Backend::start(fixture.options.clone());
    within("ready", backend.wait_ready()).await.unwrap();
    assert_eq!(*backend.status().borrow(), BackendStatus::Ready);

    let envelopes = fixture.envelopes();
    assert_eq!(envelopes.len(), 1);
    let envelope = &envelopes[0];
    assert_eq!(envelope["mode"], "desktop");
    assert_eq!(envelope["host"], "127.0.0.1");
    assert_eq!(envelope["port"], fixture.options.port);
    assert_eq!(
        envelope["t3Home"],
        fixture.options.t3_home.display().to_string()
    );
    let secret = envelope["desktopBootstrapSecret"].as_str().unwrap();
    let token = envelope["desktopBootstrapToken"].as_str().unwrap();
    assert!(is_valid_token(secret, token, t3_backend::token::now_ms()));
    assert!(is_valid_token(
        secret,
        &backend.credential(),
        t3_backend::token::now_ms()
    ));
    assert_eq!(
        backend.http_base_url(),
        format!("http://127.0.0.1:{}/", fixture.options.port)
    );
    assert_eq!(
        backend.ws_base_url(),
        format!("ws://127.0.0.1:{}/", fixture.options.port)
    );

    // Output is captured.
    within("logs", async {
        while !backend.logs().iter().any(|line| line == "stderr works too") {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(
        backend
            .logs()
            .iter()
            .any(|line| line.starts_with("listening on 127.0.0.1:"))
    );

    within("shutdown", backend.shutdown()).await;
    assert_eq!(*backend.status().borrow(), BackendStatus::Stopped);
    assert!(
        reqwest::get(format!(
            "{}.well-known/t3/environment",
            backend.http_base_url()
        ))
        .await
        .is_err()
    );
}

#[tokio::test]
async fn restarts_a_server_that_crashes_during_startup() {
    let Some(fixture) = fixture("crash-first") else {
        return;
    };
    let backend = Backend::start(fixture.options.clone());
    let mut status = backend.status();
    let restarting = within(
        "restart",
        status.wait_for(|status| matches!(status, BackendStatus::Restarting { .. })),
    )
    .await
    .unwrap()
    .clone();
    let BackendStatus::Restarting {
        attempt,
        retry_in,
        reason,
    } = restarting
    else {
        unreachable!()
    };
    assert_eq!(attempt, 0);
    assert_eq!(retry_in, Duration::from_millis(500));
    assert!(reason.contains("exit"), "{reason}");
    within("ready", backend.wait_ready()).await.unwrap();
    // Only the second run got far enough to read its envelope.
    assert_eq!(fixture.envelopes().len(), 1);
    assert!(
        backend
            .logs()
            .iter()
            .any(|line| line == "crashing on purpose")
    );
    within("shutdown", backend.shutdown()).await;
}

#[tokio::test]
async fn restarts_a_server_that_exits_after_becoming_ready() {
    let Some(fixture) = fixture("exit-after-ready") else {
        return;
    };
    let backend = Backend::start(fixture.options.clone());
    within("ready", backend.wait_ready()).await.unwrap();
    let mut status = backend.status();
    within(
        "restart",
        status.wait_for(|status| matches!(status, BackendStatus::Restarting { .. })),
    )
    .await
    .unwrap();
    within(
        "ready again",
        status.wait_for(|status| *status == BackendStatus::Ready),
    )
    .await
    .unwrap();
    assert!(fixture.envelopes().len() >= 2);
    within("shutdown", backend.shutdown()).await;
}

#[tokio::test]
async fn gives_up_on_a_server_that_never_becomes_ready() {
    let Some(mut fixture) = fixture("never-ready") else {
        return;
    };
    fixture.options.readiness_timeout = Duration::from_millis(800);
    let backend = Backend::start(fixture.options.clone());
    let mut status = backend.status();
    let restarting = within(
        "restart",
        status.wait_for(|status| matches!(status, BackendStatus::Restarting { .. })),
    )
    .await
    .unwrap()
    .clone();
    let BackendStatus::Restarting { reason, .. } = restarting else {
        unreachable!()
    };
    assert_eq!(reason, "server was not ready after 0s");
    within("shutdown", backend.shutdown()).await;
    assert_eq!(
        backend.wait_ready().await,
        Err(t3_backend::BackendError::Stopped)
    );
}

#[cfg(unix)]
#[tokio::test]
async fn kills_a_server_that_ignores_sigterm_after_the_grace_period() {
    let Some(fixture) = fixture("ignore-term") else {
        return;
    };
    let backend = Backend::start(fixture.options.clone());
    within("ready", backend.wait_ready()).await.unwrap();
    let started = std::time::Instant::now();
    within("shutdown", backend.shutdown()).await;
    assert!(
        started.elapsed() >= Duration::from_millis(400),
        "{:?}",
        started.elapsed()
    );
    within("ignored log", async {
        while !backend.logs().iter().any(|line| line == "ignoring SIGTERM") {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
}

#[tokio::test]
async fn reports_a_server_that_cannot_be_spawned() {
    let dir = tempfile::tempdir().unwrap();
    let options = BackendOptions::new(
        ServerLaunch::Binary(dir.path().join("missing-t3")),
        dir.path().to_path_buf(),
        pick_free_port().unwrap(),
    );
    let backend = Backend::start(options);
    let mut status = backend.status();
    let restarting = within(
        "restart",
        status.wait_for(|status| matches!(status, BackendStatus::Restarting { .. })),
    )
    .await
    .unwrap()
    .clone();
    let BackendStatus::Restarting { reason, .. } = restarting else {
        unreachable!()
    };
    assert!(reason.starts_with("could not start"), "{reason}");
    within("shutdown", backend.shutdown()).await;
}

#[tokio::test]
async fn local_connector_authorizes_with_the_rotating_credential() {
    let Some(fixture) = fixture("ok") else { return };
    let backend = Backend::start(fixture.options.clone());
    let connector = LocalConnector::new(backend.clone());
    // The fake has no WebSocket endpoint, so the token exchange and ticket succeed and the
    // upgrade is what fails.
    let error = within("connect", connector.connect()).await.unwrap_err();
    assert!(
        error.contains("status 404") || error.contains("404"),
        "{error}"
    );
    within("shutdown", backend.shutdown()).await;
}

#[tokio::test]
async fn local_connector_refuses_an_incompatible_server() {
    let Some(mut fixture) = fixture("ok") else {
        return;
    };
    fixture
        .options
        .env
        .push(("FAKE_PROTOCOL".into(), "1".into()));
    let backend = Backend::start(fixture.options.clone());
    let error = within("connect", LocalConnector::new(backend.clone()).connect())
        .await
        .unwrap_err();
    assert_eq!(
        error,
        "This client requires a newer server. Update T3 Code on Fake to connect."
    );
    within("shutdown", backend.shutdown()).await;
}

#[tokio::test]
async fn tracks_the_server_pid_and_terminates_it_synchronously() {
    let Some(fixture) = fixture("ok") else { return };
    let backend = Backend::start(fixture.options.clone());
    within("ready", backend.wait_ready()).await.unwrap();
    let pid = backend.pid().expect("a running server has a pid");

    // What a signal handler does: no awaiting, no runtime.
    backend.terminate_now();
    let mut status = backend.status();
    within(
        "restart after the signal",
        status.wait_for(|status| matches!(status, BackendStatus::Restarting { .. })),
    )
    .await
    .unwrap();
    assert_ne!(backend.pid(), Some(pid));
    within("shutdown", backend.shutdown()).await;
    assert_eq!(backend.pid(), None);
}

#[tokio::test]
async fn terminate_now_without_a_server_is_harmless() {
    let dir = tempfile::tempdir().unwrap();
    let options = BackendOptions::new(
        ServerLaunch::Binary(dir.path().join("missing-t3")),
        dir.path().to_path_buf(),
        pick_free_port().unwrap(),
    );
    let backend = Backend::start(options);
    assert_eq!(backend.pid(), None);
    backend.terminate_now();
    within("shutdown", backend.shutdown()).await;
    assert_eq!(backend.pid(), None);
}

/// Writes the runtime state a running server leaves for `t3 pair` and attaching clients.
fn advertise(home: &Path, backend: &Backend) {
    let dir = home.join("userdata");
    std::fs::create_dir_all(&dir).unwrap();
    let origin = backend.http_base_url().trim_end_matches('/').to_owned();
    let port: u16 = origin.rsplit(':').next().unwrap().parse().unwrap();
    std::fs::write(
        dir.join("server-runtime.json"),
        serde_json::json!({
            "version": 1, "pid": backend.pid().unwrap(), "port": port,
            "origin": origin, "startedAt": "2026-11-05T00:00:00Z", "serverVersion": "0.1.7"
        })
        .to_string(),
    )
    .unwrap();
}

fn counting_minter(backend: &Backend, calls: &Arc<AtomicUsize>) -> t3_backend::attach::Minter {
    let backend = backend.clone();
    let calls = Arc::clone(calls);
    Arc::new(move |_, version| {
        assert_eq!(version.as_deref(), Some("0.1.7"));
        calls.fetch_add(1, Ordering::SeqCst);
        let credential = backend.credential();
        Box::pin(async move { Ok(credential) })
    })
}

#[tokio::test]
async fn attaches_to_a_running_server_and_reuses_the_saved_session() {
    let Some(fixture) = fixture("ok") else { return };
    let backend = Backend::start(fixture.options.clone());
    within("ready", backend.wait_ready()).await.unwrap();
    let home = fixture.dir.path().join("attach-home");
    let store = SessionStore::new(fixture.dir.path().join("sessions.json"));

    let calls = Arc::new(AtomicUsize::new(0));
    let first = AttachedConnector::new(home.clone(), Some(store.clone()))
        .with_minter(counting_minter(&backend, &calls));
    let error = within("connect", first.connect()).await.unwrap_err();
    assert!(error.contains("no T3 Code server is running"), "{error}");
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    advertise(&home, &backend);
    // The fake has no WebSocket endpoint: auth succeeds and the upgrade fails.
    let error = within("connect", first.connect()).await.unwrap_err();
    assert!(error.contains("404"), "{error}");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let saved = store.load("local", 0).expect("the session is saved");
    assert!(saved.expires_at_ms.is_some());

    // A later launch uses the saved session instead of pairing again.
    let second = AttachedConnector::new(home.clone(), Some(store.clone()))
        .with_minter(counting_minter(&backend, &calls));
    within("connect", second.connect()).await.unwrap_err();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    within("shutdown", backend.shutdown()).await;
}

#[tokio::test]
async fn ignores_runtime_state_left_by_a_dead_server() {
    let Some(fixture) = fixture("ok") else { return };
    let backend = Backend::start(fixture.options.clone());
    within("ready", backend.wait_ready()).await.unwrap();
    let home = fixture.dir.path().join("attach-home");
    advertise(&home, &backend);
    let discovered = t3_backend::attach::discover(&home, &t3_client::AuthClient::default()).await;
    assert_eq!(
        discovered.map(|server| server.descriptor.environment_id),
        Some(Some("local".into()))
    );
    within("shutdown", backend.shutdown()).await;
    assert!(
        t3_backend::attach::discover(&home, &t3_client::AuthClient::default())
            .await
            .is_none()
    );
}

#[tokio::test]
async fn stops_for_good_when_another_server_owns_the_state_directory() {
    let Some(fixture) = fixture("owned") else {
        return;
    };
    let backend = Backend::start(fixture.options.clone());
    assert!(within("ready", backend.wait_ready()).await.is_err());
    let BackendStatus::Failed { reason } = backend.status().borrow().clone() else {
        panic!("expected a failure, got {:?}", backend.status().borrow());
    };
    assert!(
        reason.starts_with("another T3 Code server already uses"),
        "{reason}"
    );
    // Give a restart loop time to show itself; there must be exactly one run.
    tokio::time::sleep(Duration::from_millis(1_200)).await;
    let runs = backend
        .logs()
        .iter()
        .filter(|line| line.contains("already owns"))
        .count();
    assert_eq!(runs, 1);
    within("shutdown", backend.shutdown()).await;
}
