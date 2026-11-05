//! Command-line options.

use std::path::PathBuf;

use clap::Parser;

#[derive(Debug, Clone, PartialEq, Eq, Parser)]
#[command(name = "t3-desktop", version, about = "T3 Code, native desktop client")]
pub struct Cli {
    /// Run this server entry script with `node` instead of an installed `t3`.
    #[arg(long, value_name = "PATH", env = "T3CODE_SERVER_ENTRY")]
    pub server_entry: Option<PathBuf>,

    /// Where the local server keeps its state.
    #[arg(long, value_name = "DIR", env = "T3CODE_HOME")]
    pub t3_home: Option<PathBuf>,

    /// Loopback port for the local server; a free one by default.
    #[arg(long, value_name = "PORT")]
    pub port: Option<u16>,

    /// Start a server even when one is already running for the T3 home. The running server
    /// holds the home's lock, so pair this with a separate `--t3-home`.
    #[arg(long)]
    pub launch_server: bool,

    /// Do not use this machine's server; connect only to `--remote`.
    #[arg(long, requires = "remote")]
    pub no_local: bool,

    /// Pair with a remote environment: a pairing URL or a `t3code://pair` link.
    #[arg(long, value_name = "URL")]
    pub remote: Option<String>,

    /// Open this thread once connected.
    #[arg(long, value_name = "ID")]
    pub thread: Option<String>,

    /// Log filter, e.g. `info` or `t3_client=debug`.
    #[arg(
        long,
        value_name = "FILTER",
        env = "T3CODE_LOG",
        default_value = "info"
    )]
    pub log: String,
}

impl Cli {
    /// The pairing URL to use, unwrapping a `t3code://pair` app link.
    pub fn remote_pairing_url(&self) -> Option<String> {
        let remote = self.remote.as_deref()?.trim();
        Some(t3_client::pairing::read_pairing_app_link(remote).unwrap_or_else(|| remote.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("t3-desktop").chain(args.iter().copied()))
    }

    #[test]
    fn defaults() {
        let cli = parse(&[]).unwrap();
        assert_eq!(cli.port, None);
        assert!(!cli.no_local);
        assert!(!cli.launch_server);
        assert_eq!(cli.thread, None);
        assert_eq!(cli.log, "info");
        assert_eq!(cli.remote_pairing_url(), None);
    }

    #[test]
    fn all_options() {
        let cli = parse(&[
            "--server-entry",
            "/srv/t3/bin.mjs",
            "--t3-home",
            "/tmp/home",
            "--port",
            "4000",
            "--remote",
            "https://box/pair#token=x",
            "--no-local",
            "--thread",
            "thread-1",
            "--log",
            "debug",
        ])
        .unwrap();
        assert_eq!(cli.thread.as_deref(), Some("thread-1"));
        assert_eq!(cli.server_entry, Some(PathBuf::from("/srv/t3/bin.mjs")));
        assert_eq!(cli.t3_home, Some(PathBuf::from("/tmp/home")));
        assert_eq!(cli.port, Some(4000));
        assert!(cli.no_local);
        assert_eq!(
            cli.remote_pairing_url().as_deref(),
            Some("https://box/pair#token=x")
        );
    }

    #[test]
    fn no_local_requires_a_remote() {
        assert!(parse(&["--no-local"]).is_err());
    }

    #[test]
    fn app_links_unwrap_to_their_pairing_url() {
        let link =
            t3_client::pairing::build_pairing_app_link("https://vps.example.com/pair#token=ABC");
        let cli = parse(&["--remote", &link]).unwrap();
        assert_eq!(
            cli.remote_pairing_url().as_deref(),
            Some("https://vps.example.com/pair#token=ABC")
        );
    }

    #[test]
    fn rejects_bad_ports() {
        assert!(parse(&["--port", "70000"]).is_err());
        assert!(parse(&["--port", "abc"]).is_err());
    }
}
