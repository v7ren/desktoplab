//! Wires geometry, the platform, and the network into one process.

mod clipboard;
mod config;
mod dnd;
mod host_worker;
mod hotkeys;
mod local;
mod log;
mod netloop;
mod peers;
mod remote;
mod runtime;
mod transfer;

pub use clipboard::{cap, prepare};
pub use config::{Config, HotkeyConfig};
pub use local::order_of;
pub use log::{conn_span, init, Conn, Log};
pub use remote::RemoteSession;
pub use runtime::{config_dir, spawn, start_in, Cmd, EngineHandle, PeerView, Snapshot, UiEvent};
