//! The wire protocol T3 Code servers speak on `/ws`: Effect RPC with JSON serialization.
//!
//! Every WebSocket text frame carries one JSON message (or an array of them). The client sends
//! `Request`, `Ack`, `Interrupt`, `Ping` and `Eof`; the server answers with `Chunk` (stream
//! values), `Exit` (the terminal result), `Defect`, `Pong` and `ClientProtocolError`.
//!
//! [`RpcSession`] is a sans-I/O multiplexer over those messages: it allocates request ids, tracks
//! what is in flight and turns server frames into [`SessionEvent`]s. Transports own the socket.

mod error;
mod message;
mod session;

pub use error::RpcError;
pub use message::{
    CauseEncoded, ClientMessage, ExitEncoded, FrameError, RequestId, ServerMessage, decode_frame,
    encode_message,
};
pub use session::{RequestKind, RpcSession, SessionEvent};
