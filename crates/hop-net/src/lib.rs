//! QUIC (TLS 1.3) sessions, mDNS discovery, and TOFU pairing.
//! Unpaired peers can complete the handshake so both sides can show a code,
//! but `PairGate` refuses input until both machines confirm (R7).

pub mod conn;
pub mod discovery;
pub mod heartbeat;
pub mod identity;
pub mod pairing;

pub use conn::{read_msg, should_dial, write_msg, Endpoint, FrameReader, Link, NetError};
pub use discovery::{manual_peer, service_type, Discovery, PeerAdvert};
pub use heartbeat::{HeartbeatWatch, RttProbe, HEARTBEAT_MS, MISS_LIMIT};
pub use identity::Identity;
pub use pairing::{sas_code, PairGate};
pub use quinn::{RecvStream, SendStream};
