//! Client state for T3 Code, free of I/O: the reducers that keep the sidebar and an open thread
//! current from server streams, and the view models the desktop UI draws.
//!
//! The reducers are ports of `packages/client-runtime/src/state` in the upstream repository and
//! carry its tests, so behavior stays aligned with the web and mobile clients.

mod bodies;
mod collation;
pub mod projection;
pub mod shell;
pub mod stop;
pub mod thread;
pub mod title;
pub mod transcript;
mod visibility;

pub use collation::locale_compare;
pub use visibility::{TurnItemVisibility, is_superseded_interrupt, is_turn_item_visible};
