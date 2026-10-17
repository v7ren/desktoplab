//! QUIC (TLS 1.3) sessions, mDNS discovery, and TOFU pairing.
//! Unpaired peers can complete the handshake so both sides can show a code,
//! but `PairGate` refuses input until both machines confirm (R7).

pub mod conn;
pub mod devname;
pub mod discovery;
pub mod heartbeat;
pub mod identity;
pub mod pairing;
pub mod session;

pub use conn::{
    is_preferred, read_msg, should_dial, write_msg, Endpoint, FrameReader, Link, NetError,
    CLIPBOARD_CHUNK_BYTES, STREAM_CLIPBOARD, STREAM_CONTROL, STREAM_TRANSFER,
};
pub use devname::device_name;
pub use discovery::{
    advertised_ip, is_own_addr, manual_peer, service_type, Discovery, PeerAdvert, BEACON_PORT,
};
pub use heartbeat::{HeartbeatWatch, RttProbe, HEARTBEAT_MS, MISS_LIMIT};
pub use identity::Identity;
pub use pairing::{local_code, sas_code, PairGate};
pub use quinn::{RecvStream, SendStream};
pub use session::{decide_install, dial_key, DialLedger, InstallAction, Phase};
