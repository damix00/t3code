//! Runs the local server process: spawn, hand it the bootstrap envelope, wait for readiness,
//! restart it with backoff when it dies, and stop it gracefully.
//!
//! Timings follow upstream's `DesktopBackendManager`.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, watch};

use crate::launch::{BootstrapEnvelope, ServerLaunch};
use crate::token;

pub const READINESS_PATH: &str = "/.well-known/t3/environment";
const INITIAL_RESTART_DELAY: Duration = Duration::from_millis(500);
const MAX_RESTART_DELAY: Duration = Duration::from_secs(10);
/// `SERVER_EXIT_CODE_STATE_DIR_OWNED`: another server holds the state directory, so restarting
/// cannot help.
pub const EXIT_CODE_STATE_DIR_OWNED: i32 = 78;

/// `min(500ms * 2^attempt, 10s)`.
pub fn restart_delay(attempt: u32) -> Duration {
    let factor = 2u32.checked_pow(attempt).unwrap_or(u32::MAX);
    INITIAL_RESTART_DELAY
        .saturating_mul(factor)
        .min(MAX_RESTART_DELAY)
}

#[derive(Debug, Clone)]
pub struct BackendOptions {
    pub launch: ServerLaunch,
    pub t3_home: PathBuf,
    pub port: u16,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    /// Large state databases can take over a minute to become ready.
    pub readiness_timeout: Duration,
    pub readiness_interval: Duration,
    pub readiness_request_timeout: Duration,
    /// How long a stopping server may take to end its provider sessions.
    pub terminate_grace: Duration,
    /// Lines of server output kept for the diagnostics view.
    pub log_capacity: usize,
}

impl BackendOptions {
    pub fn new(launch: ServerLaunch, t3_home: PathBuf, port: u16) -> Self {
        Self {
            launch,
            t3_home,
            port,
            cwd: None,
            env: Vec::new(),
            readiness_timeout: Duration::from_secs(180),
            readiness_interval: Duration::from_millis(100),
            readiness_request_timeout: Duration::from_secs(1),
            terminate_grace: Duration::from_secs(5),
            log_capacity: 2_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendStatus {
    Starting {
        attempt: u32,
    },
    Ready,
    /// The last run ended; the next starts after `retry_in`.
    Restarting {
        attempt: u32,
        retry_in: Duration,
        reason: String,
    },
    Stopped,
    /// The server cannot run and is not restarted.
    Failed {
        reason: String,
    },
}

impl BackendStatus {
    /// No further runs will happen.
    pub fn is_final(&self) -> bool {
        matches!(self, Self::Stopped | Self::Failed { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BackendError {
    #[error("the local server stopped")]
    Stopped,
}

#[derive(Debug)]
enum Control {
    Shutdown,
}

/// Recent output from the server, oldest first.
#[derive(Debug, Clone, Default)]
pub struct LogBuffer {
    lines: Arc<Mutex<VecDeque<String>>>,
    capacity: usize,
}

impl LogBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            lines: Arc::default(),
            capacity,
        }
    }

    pub fn push(&self, line: String) {
        let mut lines = self.lines.lock().unwrap_or_else(PoisonError::into_inner);
        if self.capacity == 0 {
            return;
        }
        while lines.len() >= self.capacity {
            lines.pop_front();
        }
        lines.push_back(line);
    }

    pub fn snapshot(&self) -> Vec<String> {
        self.lines
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .cloned()
            .collect()
    }
}

/// A supervised local server.
#[derive(Debug, Clone)]
pub struct Backend {
    secret: Arc<str>,
    http_base_url: Arc<str>,
    status: watch::Receiver<BackendStatus>,
    control: mpsc::UnboundedSender<Control>,
    logs: LogBuffer,
    /// The running server's process id, or 0 between runs.
    pid: Arc<AtomicU32>,
}

impl Backend {
    /// Starts supervising on the current tokio runtime. Returns at once; watch
    /// [`Self::status`] or await [`Self::wait_ready`].
    pub fn start(options: BackendOptions) -> Self {
        let secret: Arc<str> = token::generate_secret().into();
        let http_base_url: Arc<str> = format!("http://127.0.0.1:{}/", options.port).into();
        let (status_tx, status) = watch::channel(BackendStatus::Starting { attempt: 0 });
        let (control, control_rx) = mpsc::unbounded_channel();
        let logs = LogBuffer::new(options.log_capacity);
        let pid = Arc::new(AtomicU32::new(0));
        tokio::spawn(supervise(
            options,
            Arc::clone(&secret),
            Arc::clone(&http_base_url),
            status_tx,
            control_rx,
            logs.clone(),
            Arc::clone(&pid),
        ));
        Self {
            secret,
            http_base_url,
            status,
            control,
            logs,
            pid,
        }
    }

    /// `http://127.0.0.1:<port>/`.
    pub fn http_base_url(&self) -> &str {
        &self.http_base_url
    }

    /// `ws://127.0.0.1:<port>/`.
    pub fn ws_base_url(&self) -> String {
        self.http_base_url.replacen("http://", "ws://", 1)
    }

    /// The bootstrap credential for right now; exchange it for a bearer token.
    pub fn credential(&self) -> String {
        token::current_token(&self.secret, token::now_ms())
    }

    pub fn status(&self) -> watch::Receiver<BackendStatus> {
        self.status.clone()
    }

    pub fn logs(&self) -> Vec<String> {
        self.logs.snapshot()
    }

    /// Resolves once the server answers its readiness probe.
    pub async fn wait_ready(&self) -> Result<(), BackendError> {
        let mut status = self.status.clone();
        let result = status
            .wait_for(|status| *status == BackendStatus::Ready || status.is_final())
            .await;
        match result.as_deref() {
            Ok(BackendStatus::Ready) => Ok(()),
            _ => Err(BackendError::Stopped),
        }
    }

    /// The running server's process id.
    pub fn pid(&self) -> Option<u32> {
        Some(self.pid.load(Ordering::SeqCst)).filter(|pid| *pid != 0)
    }

    /// Asks the server to stop without waiting or needing the runtime, for signal handlers
    /// and panic hooks where the supervisor may never get to run again.
    pub fn terminate_now(&self) {
        let Some(pid) = self.pid() else { return };
        #[cfg(unix)]
        let mut command = std::process::Command::new("kill");
        #[cfg(unix)]
        command.args(["-TERM", &pid.to_string()]);
        #[cfg(not(unix))]
        let mut command = std::process::Command::new("taskkill");
        #[cfg(not(unix))]
        command.args(["/PID", &pid.to_string(), "/T", "/F"]);
        let _ = command.stdout(Stdio::null()).stderr(Stdio::null()).status();
    }

    /// Stops the server (gracefully, then forcibly after the grace period) and waits for it.
    pub async fn shutdown(&self) {
        let _ = self.control.send(Control::Shutdown);
        let mut status = self.status.clone();
        let _ = status.wait_for(BackendStatus::is_final).await;
    }
}

fn spawn_child(options: &BackendOptions) -> std::io::Result<Child> {
    let (program, args) = options.launch.command_line();
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(cwd) = &options.cwd {
        command.current_dir(cwd);
    }
    for (key, value) in &options.env {
        command.env(key, value);
    }
    command.spawn()
}

fn capture(
    stream: Option<impl AsyncRead + Unpin + Send + 'static>,
    logs: LogBuffer,
    label: &'static str,
) {
    let Some(stream) = stream else { return };
    tokio::spawn(async move {
        let mut lines = BufReader::new(stream).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::debug!(target: "t3_backend::server", stream = label, "{line}");
            logs.push(line);
        }
    });
}

/// Polls the readiness endpoint until it answers 2xx.
async fn wait_until_ready(http: &reqwest::Client, url: &str, options: &BackendOptions) -> bool {
    let deadline = tokio::time::Instant::now() + options.readiness_timeout;
    while tokio::time::Instant::now() < deadline {
        let probe = http
            .get(url)
            .timeout(options.readiness_request_timeout)
            .send()
            .await;
        if probe.is_ok_and(|response| response.status().is_success()) {
            return true;
        }
        tokio::time::sleep(options.readiness_interval).await;
    }
    false
}

/// Asks the process to stop, then kills it after `grace`.
async fn terminate(child: &mut Child, grace: Duration) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        let signalled = Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .is_ok_and(|status| status.success());
        if signalled && tokio::time::timeout(grace, child.wait()).await.is_ok() {
            return;
        }
    }
    #[cfg(not(unix))]
    let _ = grace;
    let _ = child.kill().await;
}

enum RunEnd {
    Exited(String),
    /// Exited in a way restarting cannot fix.
    Fatal(String),
    Shutdown,
}

/// What every run of the server shares.
struct Supervisor {
    options: BackendOptions,
    secret: Arc<str>,
    readiness_url: String,
    http: reqwest::Client,
    status: watch::Sender<BackendStatus>,
    logs: LogBuffer,
    pid: Arc<AtomicU32>,
}

impl Supervisor {
    async fn run_once(
        &self,
        control: &mut mpsc::UnboundedReceiver<Control>,
        attempt: &mut u32,
    ) -> RunEnd {
        let mut child = match spawn_child(&self.options) {
            Ok(child) => child,
            Err(error) => {
                return RunEnd::Exited(format!(
                    "could not start {}: {error}",
                    self.options.launch.describe()
                ));
            }
        };
        self.pid.store(child.id().unwrap_or(0), Ordering::SeqCst);
        let end = self.drive(&mut child, control, attempt).await;
        self.pid.store(0, Ordering::SeqCst);
        end
    }

    async fn drive(
        &self,
        child: &mut Child,
        control: &mut mpsc::UnboundedReceiver<Control>,
        attempt: &mut u32,
    ) -> RunEnd {
        let options = &self.options;
        let envelope = BootstrapEnvelope::new(
            options.port,
            &options.t3_home,
            &self.secret,
            token::now_ms(),
        );
        if let Some(mut stdin) = child.stdin.take() {
            // The server waits about a second for this line, so write it right away.
            if let Err(error) = stdin.write_all(envelope.to_line().as_bytes()).await {
                tracing::warn!(%error, "could not write the bootstrap envelope");
            }
            let _ = stdin.flush().await;
        }
        capture(child.stdout.take(), self.logs.clone(), "stdout");
        capture(child.stderr.take(), self.logs.clone(), "stderr");

        let mut ready = false;
        let readiness = wait_until_ready(&self.http, &self.readiness_url, options);
        tokio::pin!(readiness);
        loop {
            tokio::select! {
                exit = child.wait() => {
                    if let Ok(exit) = &exit
                        && exit.code() == Some(EXIT_CODE_STATE_DIR_OWNED)
                    {
                        return RunEnd::Fatal(format!(
                            "another T3 Code server already uses {}",
                            options.t3_home.display()
                        ));
                    }
                    let reason = match exit {
                        Ok(exit) => format!("server exited with {exit}"),
                        Err(error) => format!("lost the server process: {error}"),
                    };
                    return RunEnd::Exited(reason);
                }
                became_ready = &mut readiness, if !ready => {
                    if became_ready {
                        ready = true;
                        *attempt = 0;
                        let _ = self.status.send(BackendStatus::Ready);
                    } else {
                        terminate(child, options.terminate_grace).await;
                        return RunEnd::Exited(format!(
                            "server was not ready after {}s",
                            options.readiness_timeout.as_secs()
                        ));
                    }
                }
                command = control.recv() => match command {
                    None | Some(Control::Shutdown) => {
                        terminate(child, options.terminate_grace).await;
                        return RunEnd::Shutdown;
                    }
                },
            }
        }
    }
}

async fn supervise(
    options: BackendOptions,
    secret: Arc<str>,
    http_base_url: Arc<str>,
    status: watch::Sender<BackendStatus>,
    mut control: mpsc::UnboundedReceiver<Control>,
    logs: LogBuffer,
    pid: Arc<AtomicU32>,
) {
    let supervisor = Supervisor {
        readiness_url: format!("{}{}", http_base_url.trim_end_matches('/'), READINESS_PATH),
        http: reqwest::Client::new(),
        options,
        secret,
        status,
        logs,
        pid,
    };
    let Supervisor {
        options,
        status,
        logs,
        ..
    } = &supervisor;
    let mut attempt: u32 = 0;
    loop {
        let _ = status.send(BackendStatus::Starting { attempt });
        tracing::info!(command = %options.launch.describe(), attempt, "starting local server");
        let reason = match supervisor.run_once(&mut control, &mut attempt).await {
            RunEnd::Shutdown => break,
            RunEnd::Fatal(reason) => {
                tracing::error!(%reason, "local server cannot run");
                logs.push(format!("[t3-desktop] {reason}"));
                let _ = status.send(BackendStatus::Failed { reason });
                return;
            }
            RunEnd::Exited(reason) => reason,
        };
        let retry_in = restart_delay(attempt);
        tracing::warn!(%reason, ?retry_in, "local server ended; restarting");
        logs.push(format!(
            "[t3-desktop] {reason}; restarting in {}ms",
            retry_in.as_millis()
        ));
        let _ = status.send(BackendStatus::Restarting {
            attempt,
            retry_in,
            reason,
        });
        attempt = attempt.saturating_add(1);
        tokio::select! {
            () = tokio::time::sleep(retry_in) => {}
            _ = control.recv() => break,
        }
    }
    let _ = status.send(BackendStatus::Stopped);
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn restart_delays_double_up_to_ten_seconds() {
        let delays: Vec<u128> = (0..8)
            .map(|attempt| restart_delay(attempt).as_millis())
            .collect();
        assert_eq!(
            delays,
            vec![500, 1_000, 2_000, 4_000, 8_000, 10_000, 10_000, 10_000]
        );
        assert_eq!(restart_delay(u32::MAX), MAX_RESTART_DELAY);
    }

    #[test]
    fn log_buffer_keeps_the_newest_lines() {
        let logs = LogBuffer::new(2);
        logs.push("a".into());
        logs.push("b".into());
        logs.push("c".into());
        assert_eq!(logs.snapshot(), vec!["b", "c"]);
        let none = LogBuffer::new(0);
        none.push("a".into());
        assert!(none.snapshot().is_empty());
    }
}
