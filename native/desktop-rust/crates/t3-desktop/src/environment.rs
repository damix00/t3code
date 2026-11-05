//! The environments this window can show: the T3 Code server already running on this machine
//! (or one this app launched when none is), and paired remotes.
//! Everything here runs on the tokio runtime; the UI only holds handles.

use std::path::PathBuf;
use std::time::Duration;

use t3_backend::{
    AttachedConnector, Backend, BackendOptions, BackendStatus, LocalConnector, RemoteConnector,
    SessionStore,
};
use t3_client::{PairingInput, SyncHandle, SyncOptions, resolve_pairing_target};
use tokio::runtime::Handle;
use tokio::sync::watch;

use crate::cli::Cli;

/// How long startup waits to find a running server before launching one.
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(3);

pub enum EnvironmentKind {
    /// The server something else runs for this machine's T3 home, usually the T3 Code app.
    Attached {
        t3_home: PathBuf,
        http_base_url: String,
    },
    /// A server this app launched and supervises.
    Local {
        backend: Backend,
    },
    Remote {
        http_base_url: String,
    },
}

impl std::fmt::Debug for EnvironmentKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Attached { http_base_url, .. } => write!(f, "Attached({http_base_url})"),
            Self::Local { backend } => write!(f, "Local({})", backend.http_base_url()),
            Self::Remote { http_base_url } => write!(f, "Remote({http_base_url})"),
        }
    }
}

#[derive(Debug)]
pub struct Environment {
    pub kind: EnvironmentKind,
    pub sync: SyncHandle,
}

impl Environment {
    pub fn is_local(&self) -> bool {
        matches!(
            self.kind,
            EnvironmentKind::Local { .. } | EnvironmentKind::Attached { .. }
        )
    }

    pub fn backend(&self) -> Option<&Backend> {
        match &self.kind {
            EnvironmentKind::Local { backend } => Some(backend),
            EnvironmentKind::Attached { .. } | EnvironmentKind::Remote { .. } => None,
        }
    }

    pub fn backend_status(&self) -> Option<watch::Receiver<BackendStatus>> {
        self.backend().map(Backend::status)
    }

    pub fn address(&self) -> String {
        match &self.kind {
            EnvironmentKind::Local { backend } => backend.http_base_url().to_owned(),
            EnvironmentKind::Attached { http_base_url, .. }
            | EnvironmentKind::Remote { http_base_url } => http_base_url.clone(),
        }
    }

    /// Stops syncing and, for the local server, stops the process.
    pub fn shutdown(&self, runtime: &Handle) {
        self.sync.shutdown();
        if let EnvironmentKind::Local { backend } = &self.kind {
            // The server ends its provider sessions within its grace period.
            let _ = runtime.block_on(tokio::time::timeout(
                Duration::from_secs(8),
                backend.shutdown(),
            ));
        }
    }
}

#[derive(Debug)]
pub enum StartError {
    Launch(t3_backend::LaunchError),
    Port(std::io::Error),
    NoHome,
}

impl std::error::Error for StartError {}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Launch(error) => error.fmt(f),
            Self::Port(error) => write!(f, "could not pick a port for the local server: {error}"),
            Self::NoHome => f.write_str("could not find a home directory; pass --t3-home"),
        }
    }
}

/// Connects to this machine's T3 Code server: the one already running for the T3 home (the
/// T3 Code app's, normally), or else one this app launches there, as the Electron app does.
pub fn start_local(cli: &Cli, runtime: &Handle) -> Result<Environment, StartError> {
    let t3_home: PathBuf = match &cli.t3_home {
        Some(home) => home.clone(),
        None => t3_backend::launch::default_t3_home().ok_or(StartError::NoHome)?,
    };
    if !cli.launch_server {
        let auth = t3_client::AuthClient::default();
        let running = runtime.block_on(async {
            tokio::time::timeout(
                DISCOVERY_TIMEOUT,
                t3_backend::attach::discover(&t3_home, &auth),
            )
            .await
            .ok()
            .flatten()
        });
        if let Some(server) = running {
            tracing::info!(origin = %server.state.origin, "attaching to the running T3 Code server");
            return Ok(attach(t3_home, server.http_base_url, runtime));
        }
    }
    let launch = t3_backend::resolve_launch(
        cli.server_entry.clone(),
        None,
        std::env::var_os("PATH").as_deref(),
    )
    .map_err(StartError::Launch)?;
    let port = match cli.port {
        Some(port) => port,
        None => t3_backend::launch::pick_free_port().map_err(StartError::Port)?,
    };
    let _guard = runtime.enter();
    let mut options = BackendOptions::new(launch, t3_home, port);
    options.cwd = dirs_home();
    let backend = Backend::start(options);
    let sync = t3_client::sync::spawn(LocalConnector::new(backend.clone()), SyncOptions::default());
    Ok(Environment {
        kind: EnvironmentKind::Local { backend },
        sync,
    })
}

fn attach(t3_home: PathBuf, http_base_url: String, runtime: &Handle) -> Environment {
    let _guard = runtime.enter();
    let connector = AttachedConnector::new(t3_home.clone(), SessionStore::default_location());
    let sync = t3_client::sync::spawn(connector, SyncOptions::default());
    Environment {
        kind: EnvironmentKind::Attached {
            t3_home,
            http_base_url,
        },
        sync,
    }
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Pairs with a remote environment from a pairing URL or `t3code://pair` link.
pub fn start_remote(pairing: &str, runtime: &Handle) -> Result<Environment, String> {
    let pairing_url = t3_client::pairing::read_pairing_app_link(pairing.trim())
        .unwrap_or_else(|| pairing.trim().to_owned());
    let target = resolve_pairing_target(&PairingInput {
        pairing_url: Some(pairing_url),
        ..Default::default()
    })
    .map_err(|error| error.to_string())?;
    let http_base_url = target.http_base_url.clone();
    let _guard = runtime.enter();
    let sync = t3_client::sync::spawn(RemoteConnector::new(target), SyncOptions::default());
    Ok(Environment {
        kind: EnvironmentKind::Remote { http_base_url },
        sync,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap()
    }

    #[test]
    fn remote_environments_need_a_valid_pairing_url() {
        let runtime = runtime();
        assert_eq!(
            start_remote("", runtime.handle()).unwrap_err(),
            "Enter a backend URL."
        );
        assert_eq!(
            start_remote("https://box.example/pair", runtime.handle()).unwrap_err(),
            "Pairing URL is missing its token."
        );
        let environment =
            start_remote("https://box.example/pair#token=abc", runtime.handle()).unwrap();
        assert!(!environment.is_local());
        assert_eq!(environment.address(), "https://box.example/");
        assert!(environment.backend_status().is_none());
        environment.shutdown(runtime.handle());
    }

    #[test]
    fn remote_environments_accept_app_links() {
        let runtime = runtime();
        let link =
            t3_client::pairing::build_pairing_app_link("https://box.example:8443/pair#token=abc");
        let environment = start_remote(&link, runtime.handle()).unwrap();
        assert_eq!(environment.address(), "https://box.example:8443/");
        environment.shutdown(runtime.handle());
    }

    #[test]
    fn local_start_reports_a_missing_entry() {
        let runtime = runtime();
        let cli = Cli {
            server_entry: Some("/definitely/not/here.mjs".into()),
            t3_home: Some("/tmp".into()),
            port: Some(1),
            no_local: false,
            launch_server: true,
            remote: None,
            thread: None,
            log: "info".into(),
        };
        let error = start_local(&cli, runtime.handle()).unwrap_err();
        assert_eq!(
            error.to_string(),
            "server entry /definitely/not/here.mjs does not exist"
        );
    }

    #[test]
    fn launches_when_no_server_is_running_for_the_home() {
        let runtime = runtime();
        let home = tempfile::tempdir().unwrap();
        let cli = Cli {
            server_entry: Some("/definitely/not/here.mjs".into()),
            t3_home: Some(home.path().to_path_buf()),
            port: Some(1),
            no_local: false,
            launch_server: false,
            remote: None,
            thread: None,
            log: "info".into(),
        };
        // Nothing to attach to, so it goes on to resolve a launch, which fails here.
        let error = start_local(&cli, runtime.handle()).unwrap_err();
        assert!(matches!(error, StartError::Launch(_)), "{error}");
    }
}
