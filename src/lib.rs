//! rust-bbs: an SSH-accessible bulletin board system.
//!
//! The library holds everything; `main.rs` is the SSH server and
//! `bin/bbsadmin.rs` the command-line administration tool.

pub mod auth;
pub mod boards;
pub mod content;
pub mod db;
pub mod server;
pub mod state;
pub mod terminal;
pub mod ui;
