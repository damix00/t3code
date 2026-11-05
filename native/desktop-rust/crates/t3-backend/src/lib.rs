//! The local T3 Code server, as the desktop app runs it: find the server, launch it with a
//! bootstrap envelope on stdin, keep it running, and connect to it.

pub mod attach;
pub mod connector;
pub mod launch;
pub mod supervisor;
pub mod token;

pub use attach::{AttachedConnector, RunningServer, SessionStore};
pub use connector::{LocalConnector, RemoteConnector};
pub use launch::{BootstrapEnvelope, LaunchError, ServerLaunch, resolve_launch};
pub use supervisor::{Backend, BackendError, BackendOptions, BackendStatus};
