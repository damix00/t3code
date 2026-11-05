//! What to run, where, and with which bootstrap envelope.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::Serialize;

/// The `t3` npm package version this app is built against, used when no server is installed.
pub const DEFAULT_SERVER_PACKAGE: &str = "t3@0.1.7";
/// Overrides the server entry script, like `--server-entry`.
pub const SERVER_ENTRY_ENV: &str = "T3CODE_SERVER_ENTRY";

/// How to start the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerLaunch {
    /// `node <entry>`: a checkout or an unpacked package.
    NodeEntry { node: PathBuf, entry: PathBuf },
    /// An installed `t3` executable.
    Binary(PathBuf),
    /// `npx --yes <package>`: downloads the server on first run.
    Npx { npx: PathBuf, package: String },
}

impl ServerLaunch {
    /// Program and arguments, ending in `--bootstrap-fd 0` so the server reads its envelope from
    /// stdin.
    pub fn command_line(&self) -> (PathBuf, Vec<OsString>) {
        let (program, mut args) = match self {
            Self::NodeEntry { node, entry } => (node.clone(), vec![entry.clone().into_os_string()]),
            Self::Binary(binary) => (binary.clone(), Vec::new()),
            Self::Npx { npx, package } => (npx.clone(), vec!["--yes".into(), package.into()]),
        };
        args.push("--bootstrap-fd".into());
        args.push("0".into());
        (program, args)
    }

    pub fn describe(&self) -> String {
        let (program, args) = self.command_line();
        let mut text = program.display().to_string();
        for arg in args {
            text.push(' ');
            text.push_str(&arg.to_string_lossy());
        }
        text
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LaunchError {
    #[error("server entry {0} does not exist")]
    EntryMissing(PathBuf),
    #[error("could not find `node` on PATH to run {0}")]
    NodeMissing(PathBuf),
    #[error(
        "could not find a T3 Code server: install `t3` (npm i -g t3), install Node.js for npx, or pass --server-entry"
    )]
    NothingFound,
}

/// Finds `name` on `path` (a `PATH`-style list), trying Windows executable extensions.
pub fn find_on_path(name: &str, path: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    let path = path?;
    let extensions: &[&str] = if cfg!(windows) {
        &["exe", "cmd", "bat"]
    } else {
        &[]
    };
    std::env::split_paths(path).find_map(|dir| {
        let candidate = dir.join(name);
        if is_executable(&candidate) {
            return Some(candidate);
        }
        extensions
            .iter()
            .map(|extension| candidate.with_extension(extension))
            .find(|candidate| is_executable(candidate))
    })
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Picks how to start the server: an explicit entry (flag, then environment), then an installed
/// `t3`, then `npx`.
pub fn resolve_launch(
    explicit_entry: Option<PathBuf>,
    env_entry: Option<OsString>,
    path: Option<&std::ffi::OsStr>,
) -> Result<ServerLaunch, LaunchError> {
    let entry = explicit_entry.or_else(|| {
        env_entry
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    });
    if let Some(entry) = entry {
        if !entry.is_file() {
            return Err(LaunchError::EntryMissing(entry));
        }
        let node =
            find_on_path("node", path).ok_or_else(|| LaunchError::NodeMissing(entry.clone()))?;
        return Ok(ServerLaunch::NodeEntry { node, entry });
    }
    if let Some(binary) = find_on_path("t3", path) {
        return Ok(ServerLaunch::Binary(binary));
    }
    if let Some(npx) = find_on_path("npx", path) {
        return Ok(ServerLaunch::Npx {
            npx,
            package: DEFAULT_SERVER_PACKAGE.to_owned(),
        });
    }
    Err(LaunchError::NothingFound)
}

/// `DesktopBackendBootstrap`: the JSON line the server reads from its bootstrap descriptor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapEnvelope {
    pub mode: &'static str,
    pub no_browser: bool,
    pub port: u16,
    pub t3_home: String,
    pub host: String,
    pub desktop_bootstrap_token: String,
    pub desktop_bootstrap_secret: String,
    pub tailscale_serve_enabled: bool,
    pub tailscale_serve_port: u16,
}

impl BootstrapEnvelope {
    pub fn new(port: u16, t3_home: &Path, secret: &str, now_ms: u64) -> Self {
        Self {
            mode: "desktop",
            no_browser: true,
            port,
            t3_home: t3_home.display().to_string(),
            host: "127.0.0.1".to_owned(),
            desktop_bootstrap_token: crate::token::current_token(secret, now_ms),
            desktop_bootstrap_secret: secret.to_owned(),
            tailscale_serve_enabled: false,
            tailscale_serve_port: 443,
        }
    }

    /// One line of JSON, newline-terminated.
    pub fn to_line(&self) -> String {
        let mut line = serde_json::to_string(self).unwrap_or_default();
        line.push('\n');
        line
    }
}

/// `~/.t3`, where the server keeps its state.
pub fn default_t3_home() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".t3"))
}

/// A loopback port nobody is listening on right now.
pub fn pick_free_port() -> std::io::Result<u16> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    Ok(listener.local_addr()?.port())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn executable(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    fn path_of(dirs: &[&Path]) -> OsString {
        std::env::join_paths(dirs).unwrap()
    }

    #[test]
    fn command_lines_end_with_the_bootstrap_fd() {
        let node = ServerLaunch::NodeEntry {
            node: "/usr/bin/node".into(),
            entry: "/srv/t3/bin.mjs".into(),
        };
        assert_eq!(
            node.describe(),
            "/usr/bin/node /srv/t3/bin.mjs --bootstrap-fd 0"
        );
        assert_eq!(
            ServerLaunch::Binary("/opt/t3".into()).describe(),
            "/opt/t3 --bootstrap-fd 0"
        );
        let npx = ServerLaunch::Npx {
            npx: "/usr/bin/npx".into(),
            package: "t3@1.0.0".into(),
        };
        assert_eq!(
            npx.describe(),
            "/usr/bin/npx --yes t3@1.0.0 --bootstrap-fd 0"
        );
    }

    #[test]
    fn prefers_an_explicit_entry_then_t3_then_npx() {
        let bin = tempfile::tempdir().unwrap();
        let node = executable(bin.path(), "node");
        let t3 = executable(bin.path(), "t3");
        let npx = executable(bin.path(), "npx");
        let entry = bin.path().join("server.mjs");
        std::fs::write(&entry, "").unwrap();
        let path = path_of(&[bin.path()]);

        assert_eq!(
            resolve_launch(Some(entry.clone()), None, Some(&path)),
            Ok(ServerLaunch::NodeEntry {
                node: node.clone(),
                entry: entry.clone()
            })
        );
        assert_eq!(
            resolve_launch(None, Some(entry.clone().into_os_string()), Some(&path)),
            Ok(ServerLaunch::NodeEntry {
                node,
                entry: entry.clone()
            })
        );
        assert_eq!(
            resolve_launch(None, Some(OsString::new()), Some(&path)),
            Ok(ServerLaunch::Binary(t3.clone()))
        );
        std::fs::remove_file(&t3).unwrap();
        assert_eq!(
            resolve_launch(None, None, Some(&path)),
            Ok(ServerLaunch::Npx {
                npx,
                package: DEFAULT_SERVER_PACKAGE.into()
            })
        );
    }

    #[test]
    fn reports_what_is_missing() {
        let empty = tempfile::tempdir().unwrap();
        let path = path_of(&[empty.path()]);
        let missing = empty.path().join("nope.mjs");
        assert_eq!(
            resolve_launch(Some(missing.clone()), None, Some(&path)),
            Err(LaunchError::EntryMissing(missing))
        );
        let entry = empty.path().join("server.mjs");
        std::fs::write(&entry, "").unwrap();
        assert_eq!(
            resolve_launch(Some(entry.clone()), None, Some(&path)),
            Err(LaunchError::NodeMissing(entry))
        );
        assert_eq!(
            resolve_launch(None, None, Some(&path)),
            Err(LaunchError::NothingFound)
        );
        assert_eq!(
            resolve_launch(None, None, None),
            Err(LaunchError::NothingFound)
        );
    }

    #[test]
    fn path_lookup_searches_in_order_and_skips_directories() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        std::fs::create_dir(first.path().join("t3")).unwrap();
        let found = executable(second.path(), "t3");
        assert_eq!(
            find_on_path("t3", Some(&path_of(&[first.path(), second.path()]))),
            Some(found)
        );
    }

    #[cfg(unix)]
    #[test]
    fn path_lookup_skips_non_executable_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("t3"), "").unwrap();
        assert_eq!(find_on_path("t3", Some(&path_of(&[dir.path()]))), None);
    }

    #[test]
    fn bootstrap_envelope_matches_the_desktop_schema() {
        let envelope = BootstrapEnvelope::new(3773, Path::new("/home/me/.t3"), "secret", 0);
        let line = envelope.to_line();
        assert!(line.ends_with('\n'));
        assert_eq!(line.matches('\n').count(), 1);
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(
            value,
            json!({
                "mode": "desktop",
                "noBrowser": true,
                "port": 3773,
                "t3Home": "/home/me/.t3",
                "host": "127.0.0.1",
                "desktopBootstrapToken": "c0bf962c57658da8944979b3a9b7b8233f78e4b156eb44690e30d2106eea0813",
                "desktopBootstrapSecret": "secret",
                "tailscaleServeEnabled": false,
                "tailscaleServePort": 443
            })
        );
    }

    #[test]
    fn free_ports_are_bindable() {
        // Parallel tests can grab a picked port before it is bound, so allow a few picks.
        let bound = (0..5).any(|_| {
            let port = pick_free_port().unwrap();
            assert_ne!(port, 0);
            std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
        });
        assert!(bound);
    }

    #[test]
    fn default_home_ends_in_dot_t3() {
        if let Some(home) = default_t3_home() {
            assert!(home.ends_with(".t3"));
        }
    }
}
