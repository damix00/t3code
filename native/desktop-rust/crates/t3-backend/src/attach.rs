//! Attaching to a T3 Code server something else already runs (the T3 Code app, `t3 serve`, or
//! a background service), the way upstream's `t3 pair` finds it: through the runtime state file
//! the server writes into its state directory.
//!
//! The server holds an ownership lock on that directory, so a second server cannot start on it;
//! attaching is the only way to show the same projects and threads.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use t3_client::{AuthClient, ClientMetadata, Connection, Connector, EnvironmentDescriptor};
use tokio::process::Command;

use crate::connector::{BearerCache, open_socket};
use crate::launch::find_on_path;
use crate::token::now_ms;

/// The label the server shows for this client in its connections list.
pub const CLIENT_LABEL: &str = "T3 Code (Rust)";
/// Pairing tokens are one-shot and exchanged at once.
const PAIRING_TTL: &str = "5m";
const PAIR_TIMEOUT: Duration = Duration::from_secs(60);
/// Saved sessions this close to expiry are replaced instead of used; a session the server
/// refuses early is replaced anyway.
const EXPIRY_MARGIN_MS: u64 = 60 * 1000;

/// `server-runtime.json`, as `serverRuntimeState.ts` writes it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeState {
    pub pid: u32,
    pub port: u16,
    pub origin: String,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub server_version: Option<String>,
}

/// State directories a server may run from, in `t3 pair`'s order.
pub fn runtime_state_paths(t3_home: &Path) -> [PathBuf; 2] {
    ["userdata", "dev"].map(|variant| t3_home.join(variant).join("server-runtime.json"))
}

pub fn read_runtime_state(path: &Path) -> Option<RuntimeState> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Whether a process with this id exists; a dead server's port may belong to someone else now.
pub fn process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        // `kill -0` fails with EPERM for a live process we may not signal.
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(Stdio::null())
            .output()
            .is_ok_and(|output| {
                output.status.success()
                    || String::from_utf8_lossy(&output.stderr).contains("not permitted")
            })
    }
    #[cfg(not(unix))]
    {
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .is_ok_and(|output| {
                String::from_utf8_lossy(&output.stdout)
                    .split_whitespace()
                    .any(|word| word == pid.to_string())
            })
    }
}

/// A server that answered its descriptor at the address its runtime state names.
#[derive(Debug, Clone)]
pub struct RunningServer {
    pub state: RuntimeState,
    pub descriptor: EnvironmentDescriptor,
    /// `origin` with a trailing slash.
    pub http_base_url: String,
}

impl RunningServer {
    pub fn ws_base_url(&self) -> String {
        match self.http_base_url.strip_prefix("https://") {
            Some(rest) => format!("wss://{rest}"),
            None => self.http_base_url.replacen("http://", "ws://", 1),
        }
    }
}

/// Finds a live server for `t3_home`, as `t3 pair` does.
pub async fn discover(t3_home: &Path, auth: &AuthClient) -> Option<RunningServer> {
    for path in runtime_state_paths(t3_home) {
        let Some(state) = read_runtime_state(&path) else {
            continue;
        };
        if !process_alive(state.pid) {
            continue;
        }
        let http_base_url = format!("{}/", state.origin.trim_end_matches('/'));
        if let Ok(descriptor) = auth.environment_descriptor(&http_base_url).await {
            return Some(RunningServer {
                state,
                descriptor,
                http_base_url,
            });
        }
    }
    None
}

/// The token line from `t3 pair` output.
pub fn parse_pair_output(stdout: &str) -> Option<String> {
    stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix("Token:"))
        .map(|token| token.trim().to_owned())
        .filter(|token| !token.is_empty())
}

/// A `t3` CLI invocation: the program and the arguments before the subcommand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairCli {
    pub program: PathBuf,
    pub prefix: Vec<String>,
}

impl PairCli {
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(&self.program);
        command
            .args(&self.prefix)
            .args(args)
            .stdin(Stdio::null())
            .kill_on_drop(true);
        command
    }

    async fn version(&self) -> Option<String> {
        let output = tokio::time::timeout(PAIR_TIMEOUT, self.command(&["--version"]).output())
            .await
            .ok()?
            .ok()?;
        // `t3 v0.1.7`
        let text = String::from_utf8_lossy(&output.stdout);
        let word = text.split_whitespace().last()?;
        Some(word.strip_prefix('v').unwrap_or(word).to_owned())
    }
}

/// CLIs that might mint for this server, best first: the T3 Code app's own shim, `t3` on PATH,
/// then `npx` pinned to the server's version.
pub fn pair_cli_candidates(
    t3_home: &Path,
    path_env: Option<&std::ffi::OsStr>,
    server_version: Option<&str>,
) -> Vec<PairCli> {
    let mut candidates = Vec::new();
    let shim = t3_home.join("bin").join("t3");
    if shim.is_file() {
        candidates.push(PairCli {
            program: shim,
            prefix: Vec::new(),
        });
    }
    if let Some(t3) = find_on_path("t3", path_env) {
        candidates.push(PairCli {
            program: t3,
            prefix: Vec::new(),
        });
    }
    if let (Some(npx), Some(version)) = (find_on_path("npx", path_env), server_version) {
        candidates.push(PairCli {
            program: npx,
            prefix: vec!["--yes".into(), format!("t3@{version}")],
        });
    }
    candidates
}

/// Mints a one-time pairing token in the running server's database.
///
/// Minting opens that database with the CLI's own migrations, so only a CLI of exactly the
/// server's version is used.
pub async fn mint_pairing_token(
    t3_home: &Path,
    server_version: Option<&str>,
) -> Result<String, String> {
    let candidates =
        pair_cli_candidates(t3_home, std::env::var_os("PATH").as_deref(), server_version);
    if candidates.is_empty() {
        return Err("no `t3` CLI found to pair with the running server".into());
    }
    let home = t3_home.display().to_string();
    let mut last_error = String::from("no `t3` CLI matches the running server's version");
    for cli in candidates {
        if let Some(expected) = server_version {
            let found = cli.version().await;
            if found.as_deref() != Some(expected) {
                tracing::debug!(cli = %cli.program.display(), ?found, expected, "skipping t3 CLI");
                continue;
            }
        }
        let run = cli
            .command(&[
                "pair",
                "--base-dir",
                &home,
                "--label",
                CLIENT_LABEL,
                "--ttl",
                PAIRING_TTL,
            ])
            .output();
        let output = match tokio::time::timeout(PAIR_TIMEOUT, run).await {
            Ok(Ok(output)) => output,
            Ok(Err(error)) => {
                last_error = format!("could not run {}: {error}", cli.program.display());
                continue;
            }
            Err(_) => {
                last_error = "`t3 pair` timed out".into();
                continue;
            }
        };
        if let Some(token) = parse_pair_output(&String::from_utf8_lossy(&output.stdout)) {
            return Ok(token);
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        last_error = format!(
            "`t3 pair` failed: {}",
            stderr
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("no output")
        );
    }
    Err(last_error)
}

/// A bearer token this app obtained, saved per environment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedSession {
    pub bearer: String,
    /// Unix milliseconds; `None` when the server did not say.
    pub expires_at_ms: Option<u64>,
}

impl SavedSession {
    pub fn is_usable(&self, now_ms: u64) -> bool {
        self.expires_at_ms
            .is_none_or(|expires| expires > now_ms.saturating_add(EXPIRY_MARGIN_MS))
    }
}

/// Bearer tokens by environment id, in a file only this user can read.
#[derive(Debug, Clone)]
pub struct SessionStore {
    path: PathBuf,
}

impl SessionStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// `<data dir>/t3-code-rust/sessions.json`.
    pub fn default_location() -> Option<Self> {
        dirs::data_local_dir().map(|dir| Self::new(dir.join("t3-code-rust").join("sessions.json")))
    }

    fn read_all(&self) -> std::collections::BTreeMap<String, SavedSession> {
        std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    fn write_all(
        &self,
        sessions: &std::collections::BTreeMap<String, SavedSession>,
    ) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(sessions).map_err(std::io::Error::other)?;
        let temporary = self.path.with_extension("json.tmp");
        {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temporary)?;
            std::io::Write::write_all(&mut file, text.as_bytes())?;
            file.sync_all()?;
        }
        std::fs::rename(&temporary, &self.path)
    }

    pub fn load(&self, environment_id: &str, now_ms: u64) -> Option<SavedSession> {
        self.read_all()
            .remove(environment_id)
            .filter(|session| session.is_usable(now_ms))
    }

    pub fn save(&self, environment_id: &str, session: SavedSession) -> std::io::Result<()> {
        let mut sessions = self.read_all();
        sessions.insert(environment_id.to_owned(), session);
        self.write_all(&sessions)
    }

    pub fn remove(&self, environment_id: &str) -> std::io::Result<()> {
        let mut sessions = self.read_all();
        if sessions.remove(environment_id).is_some() {
            self.write_all(&sessions)?;
        }
        Ok(())
    }
}

/// How an [`AttachedConnector`] gets a pairing token; swapped out in tests.
pub type Minter = Arc<
    dyn Fn(PathBuf, Option<String>) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>>
        + Send
        + Sync,
>;

fn default_minter() -> Minter {
    Arc::new(|home, version| {
        Box::pin(async move { mint_pairing_token(&home, version.as_deref()).await })
    })
}

/// Connects to the server already running for a T3 home. Each attempt re-reads the runtime
/// state, so it follows the server across restarts and port changes.
#[derive(Clone)]
pub struct AttachedConnector {
    t3_home: PathBuf,
    auth: AuthClient,
    metadata: ClientMetadata,
    store: Option<SessionStore>,
    bearer: Arc<BearerCache>,
    minter: Minter,
}

impl std::fmt::Debug for AttachedConnector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AttachedConnector")
            .field("t3_home", &self.t3_home)
            .field("store", &self.store)
            .finish_non_exhaustive()
    }
}

impl AttachedConnector {
    pub fn new(t3_home: PathBuf, store: Option<SessionStore>) -> Self {
        Self {
            t3_home,
            auth: AuthClient::default(),
            metadata: ClientMetadata::desktop(),
            store,
            bearer: Arc::default(),
            minter: default_minter(),
        }
    }

    pub fn with_minter(mut self, minter: Minter) -> Self {
        self.minter = minter;
        self
    }

    pub fn t3_home(&self) -> &Path {
        &self.t3_home
    }

    async fn authorize(&self, server: &RunningServer) -> Result<String, String> {
        let environment = server.descriptor.environment_id.clone();
        if let (Some(store), Some(environment)) = (&self.store, &environment)
            && let Some(saved) = store.load(environment, now_ms())
        {
            return Ok(saved.bearer);
        }
        let version = server
            .descriptor
            .server_version
            .clone()
            .or_else(|| server.state.server_version.clone());
        let pairing = (self.minter)(self.t3_home.clone(), version).await?;
        let token = self
            .auth
            .exchange_bootstrap_credential(&server.http_base_url, &pairing, &self.metadata)
            .await
            .map_err(|error| error.to_string())?;
        if let (Some(store), Some(environment)) = (&self.store, &environment) {
            let session = SavedSession {
                bearer: token.access_token.clone(),
                expires_at_ms: token
                    .expires_in
                    .map(|seconds| now_ms().saturating_add((seconds * 1000.0) as u64)),
            };
            if let Err(error) = store.save(environment, session) {
                tracing::warn!(%error, "could not save the session");
            }
        }
        Ok(token.access_token)
    }
}

impl Connector for AttachedConnector {
    fn connect(&self) -> Pin<Box<dyn Future<Output = Result<Connection, String>> + Send + '_>> {
        Box::pin(async move {
            let server = discover(&self.t3_home, &self.auth).await.ok_or_else(|| {
                format!(
                    "no T3 Code server is running for {}; open T3 Code",
                    self.t3_home.display()
                )
            })?;
            if let Some(reason) = server.descriptor.incompatibility() {
                return Err(reason);
            }
            let bearer = self
                .bearer
                .get_or_exchange(|| self.authorize(&server))
                .await?;
            let result = open_socket(
                &self.auth,
                &self.bearer,
                &server.http_base_url,
                &server.ws_base_url(),
                bearer,
                &self.metadata,
            )
            .await;
            // A refused session is forgotten so the next attempt pairs again.
            if result.is_err()
                && !self.bearer.is_set().await
                && let (Some(store), Some(environment)) =
                    (&self.store, &server.descriptor.environment_id)
            {
                let _ = store.remove(environment);
            }
            result
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn reads_the_runtime_state_the_server_writes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server-runtime.json");
        std::fs::write(
            &path,
            r#"{"version":1,"pid":99619,"host":"127.0.0.1","port":3773,"origin":"http://127.0.0.1:3773","startedAt":"2026-11-04T23:20:28.854Z","serverVersion":"0.1.7","managedPolicy":null,"ownerId":"b7"}"#,
        )
        .unwrap();
        assert_eq!(
            read_runtime_state(&path),
            Some(RuntimeState {
                pid: 99619,
                port: 3773,
                origin: "http://127.0.0.1:3773".into(),
                host: Some("127.0.0.1".into()),
                server_version: Some("0.1.7".into()),
            })
        );
        assert_eq!(read_runtime_state(&dir.path().join("missing.json")), None);
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(read_runtime_state(&path), None);
    }

    #[test]
    fn state_paths_follow_t3_pair() {
        let [userdata, dev] = runtime_state_paths(Path::new("/home/me/.t3"));
        assert_eq!(
            userdata,
            Path::new("/home/me/.t3/userdata/server-runtime.json")
        );
        assert_eq!(dev, Path::new("/home/me/.t3/dev/server-runtime.json"));
    }

    #[test]
    fn this_process_is_alive_and_pid_zero_is_not() {
        assert!(process_alive(std::process::id()));
        assert!(!process_alive(0));
    }

    #[test]
    fn parses_the_token_from_t3_pair_output() {
        let output = "Pairing with Mac (http://127.0.0.1:3773).\n\n█▀▀▀▀▀█\n\n\
            Pairing URL: http://127.0.0.1:3773/pair#token=ABCDEF123456\n\
            App link: t3code://pair?url=x\nToken: ABCDEF123456\n\
            Expires: 2026-11-05T15:02:04.849Z\n";
        assert_eq!(parse_pair_output(output), Some("ABCDEF123456".into()));
        assert_eq!(parse_pair_output("Token:   \n"), None);
        assert_eq!(parse_pair_output("No running T3 Code server found."), None);
    }

    #[test]
    fn prefers_the_app_shim_then_path_then_pinned_npx() {
        let home = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        for name in ["t3", "npx"] {
            let path = bin.path().join(name);
            std::fs::write(&path, "#!/bin/sh\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        let path_env = std::env::join_paths([bin.path()]).unwrap();
        let without_shim = pair_cli_candidates(home.path(), Some(&path_env), Some("0.1.7"));
        assert_eq!(without_shim.len(), 2);
        assert_eq!(without_shim[1].prefix, vec!["--yes", "t3@0.1.7"]);

        std::fs::create_dir_all(home.path().join("bin")).unwrap();
        std::fs::write(home.path().join("bin/t3"), "#!/bin/sh\n").unwrap();
        let with_shim = pair_cli_candidates(home.path(), Some(&path_env), None);
        assert_eq!(with_shim[0].program, home.path().join("bin/t3"));
        // Without a known version, npx cannot be pinned and is left out.
        assert_eq!(with_shim.len(), 2);
    }

    #[test]
    fn sessions_round_trip_and_expire() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().join("nested/sessions.json"));
        assert_eq!(store.load("env", 0), None);
        let session = SavedSession {
            bearer: "b".into(),
            expires_at_ms: Some(10 * EXPIRY_MARGIN_MS),
        };
        store.save("env", session.clone()).unwrap();
        store
            .save(
                "other",
                SavedSession {
                    bearer: "o".into(),
                    expires_at_ms: None,
                },
            )
            .unwrap();
        assert_eq!(store.load("env", 0), Some(session));
        // Within the margin of expiry it is no longer offered.
        assert_eq!(store.load("env", 9 * EXPIRY_MARGIN_MS + 1), None);
        assert_eq!(
            store.load("other", u64::MAX / 2).map(|s| s.bearer),
            Some("o".into())
        );
        store.remove("env").unwrap();
        assert_eq!(store.load("env", 0), None);
        assert!(store.load("other", 0).is_some());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join("nested/sessions.json"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
