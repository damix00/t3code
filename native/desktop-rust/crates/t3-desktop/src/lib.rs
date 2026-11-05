//! T3 Code's desktop app on GPUI. The binary in `main.rs` wires these together; everything
//! that does not need a window is plain data and tested as such.

// GPUI's own idiom is `Context<Self>`, without the anonymous lifetime.
#![allow(elided_lifetimes_in_paths)]

pub mod actions;
pub mod cli;
pub mod editor;
pub mod environment;
pub mod markdown;
pub mod sidebar;
pub mod timeline;
pub mod ui;
pub mod view_model;
