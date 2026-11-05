//! Talking to a T3 Code server: pairing input, HTTP authorization, the WebSocket RPC
//! connection, typed calls, and a sync engine that keeps client state current across
//! reconnects.

pub mod api;
pub mod auth;
pub mod pairing;
pub mod sync;
pub mod transport;

pub use api::{LaunchThreadResult, TypedStream};
pub use auth::{AccessToken, AuthClient, AuthError, ClientMetadata, EnvironmentDescriptor};
pub use pairing::{PairingError, PairingInput, PairingTarget, resolve_pairing_target};
pub use sync::{ConnectionStatus, Connector, OpenThread, SyncHandle, SyncOptions, SyncState};
pub use transport::{ConnectError, Connection, Keepalive, RpcStream};
