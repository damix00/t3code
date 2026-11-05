//! Rust types for the parts of `@t3tools/contracts` the desktop client reads and writes.
//!
//! The server's schemas are large and grow often. These types name the fields the client uses
//! and keep every other field in an `extra` map, so decoding never fails on a newer server and
//! equality still compares whole records, as the TypeScript reducers do.

mod commands;
mod ids;
mod model;
mod projection;
mod server;
mod shell;
mod streams;

pub use commands::{
    ApprovalDecision, Command, DispatchMode, LaunchMessage, LaunchThreadInput, ProjectCreate,
    WorkspaceStrategy, infer_project_title,
};
pub use ids::{
    CommandId, MessageId, NodeId, ProjectId, RunAttemptId, RunId, RuntimeRequestId, ThreadId,
    TurnItemId,
};
pub use model::{
    CreationSource, InteractionMode, ModelSelection, RunStatus, RuntimeMode, UserMessageInputIntent,
};
pub use projection::{
    Checkpoint, ConversationMessage, Entity, ProjectedTurnItem, ProviderTurn, Run, RunAttempt,
    RuntimeRequest, Subagent, ThreadProjection, TurnItem, Visibility,
};
pub use server::{ProviderModel, ServerConfig, ServerProvider};
pub use shell::{
    LatestVisibleMessage, PendingRuntimeRequestSummary, ProjectShell, ShellSnapshot, ThreadLineage,
    ThreadShell, ThreadShellLocation,
};
pub use streams::{DomainEvent, ShellStreamItem, ThreadStreamItem};

/// JSON builders for tests in this and dependent crates.
#[cfg(any(test, feature = "fixtures"))]
pub mod fixtures {
    pub use crate::projection::fixtures::*;
    pub use crate::shell::fixtures::*;
}

/// Fields a record carries that this client does not name.
pub type Extra = serde_json::Map<String, serde_json::Value>;

/// RPC method tags, from `WS_METHODS` and `ORCHESTRATION_V2_WS_METHODS`.
pub mod methods {
    pub const SERVER_GET_CONFIG: &str = "server.getConfig";
    pub const DISPATCH_COMMAND: &str = "orchestration.dispatchCommand";
    pub const LAUNCH_THREAD: &str = "orchestration.launchThread";
    pub const GET_THREAD_PROJECTION: &str = "orchestration.getThreadProjection";
    pub const SUBSCRIBE_SHELL: &str = "orchestration.subscribeShell";
    pub const SUBSCRIBE_THREAD: &str = "orchestration.subscribeThread";
    pub const PROJECTS_MUTATE: &str = "projects.mutate";
}
