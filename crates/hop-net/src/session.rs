//! Per-peer session phases and the link-selection rule both sides must agree on.
//! Pure state: `netloop` applies the decisions (spawn, close, emit).

use std::collections::{HashMap, HashSet};

use crate::conn::is_preferred;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    Discovered,
    Dialing,
    Handshaking {
        link_id: usize,
        dialed: bool,
    },
    Connected {
        paired: bool,
        link_id: usize,
        dialed: bool,
    },
    Closing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallAction {
    /// This link becomes (or replaces) the active link for the peer.
    Accept,
    /// Close the incoming link and keep what we already have.
    RejectNew,
    /// Close the existing link and install the new one.
    ReplaceExisting,
}

/// Whether an in-flight handshake may replace the current link.
pub fn decide_install(
    phase: &Phase,
    existing_link_open: bool,
    new_dialed: bool,
    local_id: &str,
    peer_id: &str,
) -> InstallAction {
    match phase {
        Phase::Discovered | Phase::Dialing => InstallAction::Accept,
        Phase::Closing => InstallAction::Accept,
        Phase::Handshaking { dialed, .. } | Phase::Connected { dialed, .. } => {
            if !existing_link_open {
                return InstallAction::Accept;
            }
            // Prefer the link the smaller device id opened. This is decidable
            // from the ids alone, so both sides always keep the *same* QUIC
            // connection, whether it is still handshaking or already in use.
            let new_preferred = is_preferred(new_dialed, local_id, peer_id);
            let old_preferred = is_preferred(*dialed, local_id, peer_id);
            if new_preferred && !old_preferred {
                InstallAction::ReplaceExisting
            } else {
                // Same preference class, or the existing link is preferred:
                // first installed wins.
                InstallAction::RejectNew
            }
        }
    }
}

impl Phase {
    pub fn on_dial_started(self) -> Self {
        match self {
            Phase::Discovered => Phase::Dialing,
            other => other,
        }
    }

    pub fn on_handshake_started(self, link_id: usize, dialed: bool) -> Self {
        match self {
            Phase::Discovered | Phase::Dialing => Phase::Handshaking { link_id, dialed },
            Phase::Handshaking { .. } | Phase::Connected { .. } => {
                Phase::Handshaking { link_id, dialed }
            }
            Phase::Closing => Phase::Handshaking { link_id, dialed },
        }
    }

    pub fn on_installed(self, link_id: usize, dialed: bool, paired: bool) -> Self {
        Phase::Connected {
            paired,
            link_id,
            dialed,
        }
    }

    pub fn on_link_closed(self) -> Self {
        Phase::Discovered
    }

    pub fn set_paired(self) -> Self {
        match self {
            Phase::Connected {
                link_id, dialed, ..
            } => Phase::Connected {
                paired: true,
                link_id,
                dialed,
            },
            other => other,
        }
    }

    /// The single user-visible status for a peer, derived from the state
    /// machine. `netloop` forwards this to the engine so the UI never has to
    /// guess from a log line.
    pub fn status(&self) -> &'static str {
        match self {
            Phase::Discovered => "discovered",
            Phase::Dialing | Phase::Handshaking { .. } => "connecting",
            Phase::Connected { paired: true, .. } => "paired",
            Phase::Connected { paired: false, .. } => "unpaired",
            Phase::Closing => "offline",
        }
    }
}

/// Dial dedupe key: device id when known, otherwise the address string.
pub fn dial_key(device_id: Option<&str>, addr: &str) -> String {
    device_id
        .filter(|id| !id.is_empty())
        .map(|id| id.to_string())
        .unwrap_or_else(|| addr.to_string())
}

/// Pure bookkeeping for "dial at most once per peer/address": what is in
/// flight, when a failed dial may be retried, and how many failures in a row.
/// `netloop` owns a `DialLedger` and applies its answers (`begin`, `waiting`,
/// `failed`, `succeeded`); the rules themselves live here, with tests.
#[derive(Clone, Debug, Default)]
pub struct DialLedger {
    in_flight: HashSet<String>,
    retry_after: HashMap<String, u64>,
    fails: HashMap<String, u32>,
}

impl DialLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Marks `key` in flight. Returns `false` when a dial for it is already
    /// running, in which case the caller must not start another.
    pub fn begin(&mut self, key: &str) -> bool {
        self.in_flight.insert(key.to_string())
    }

    pub fn in_flight(&self, key: &str) -> bool {
        self.in_flight.contains(key)
    }

    /// True while an earlier failure says to wait before dialing `key` again.
    pub fn waiting(&self, key: &str, now_ms: u64) -> bool {
        self.retry_after
            .get(key)
            .is_some_and(|after| now_ms < *after)
    }

    /// A dial succeeded: clear every trace of the key.
    pub fn succeeded(&mut self, key: &str) {
        self.in_flight.remove(key);
        self.retry_after.remove(key);
        self.fails.remove(key);
    }

    /// A dial failed: schedule the next attempt and count the failure. Returns
    /// `true` once the key has failed `forget_after` times in a row.
    pub fn failed(&mut self, key: &str, now_ms: u64, retry_ms: u64, forget_after: u32) -> bool {
        self.in_flight.remove(key);
        self.retry_after
            .insert(key.to_string(), now_ms.saturating_add(retry_ms));
        let fails = self.fails.entry(key.to_string()).or_insert(0);
        *fails += 1;
        *fails >= forget_after
    }

    /// Drops the failure count for a key that has been forgotten from discovery.
    pub fn forget(&mut self, key: &str) {
        self.fails.remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "aaa";
    const B: &str = "bbb";

    #[test]
    fn both_dial_at_once_tie_break_agrees() {
        // Smaller id (aaa) dialed; both sides reject the acceptor link.
        let phase = Phase::Handshaking {
            link_id: 1,
            dialed: true,
        };
        assert_eq!(
            decide_install(&phase, true, false, A, B),
            InstallAction::RejectNew
        );
        assert_eq!(
            decide_install(&phase, true, true, B, A),
            InstallAction::RejectNew
        );
        // Smaller id's inbound link replaces the larger id's outbound attempt.
        let phase = Phase::Handshaking {
            link_id: 1,
            dialed: false,
        };
        assert_eq!(
            decide_install(&phase, true, true, A, B),
            InstallAction::ReplaceExisting
        );
    }

    #[test]
    fn connected_session_keeps_the_preferred_link() {
        // Same preference class (both outbound): first installed wins.
        let phase = Phase::Connected {
            paired: false,
            link_id: 10,
            dialed: true,
        };
        assert_eq!(
            decide_install(&phase, true, true, A, B),
            InstallAction::RejectNew
        );
        // The existing link was the larger id's outbound (not preferred for
        // B); the peer's smaller-id dial is preferred, so it must replace it.
        // Otherwise B keeps `b->a` while A keeps `a->b`: different connections,
        // and both sides drop the session (the flap in PLAN.md N1).
        let phase = Phase::Connected {
            paired: false,
            link_id: 10,
            dialed: true,
        };
        assert_eq!(
            decide_install(&phase, true, false, B, A),
            InstallAction::ReplaceExisting
        );
    }

    #[test]
    fn late_handshake_after_pairing_is_rejected() {
        let phase = Phase::Connected {
            paired: true,
            link_id: 7,
            dialed: false,
        };
        assert_eq!(
            decide_install(&phase, true, true, B, A),
            InstallAction::RejectNew
        );
    }

    #[test]
    fn peer_restart_with_same_id_accepts_when_old_link_is_gone() {
        let phase = Phase::Connected {
            paired: true,
            link_id: 7,
            dialed: false,
        };
        assert_eq!(
            decide_install(&phase, false, true, A, B),
            InstallAction::Accept
        );
    }

    #[test]
    fn same_side_dial_twice_keeps_first_handshake_winner() {
        let phase = Phase::Handshaking {
            link_id: 1,
            dialed: true,
        };
        assert_eq!(
            decide_install(&phase, true, true, A, B),
            InstallAction::RejectNew
        );
    }

    #[test]
    fn dial_key_uses_device_id_when_known() {
        assert_eq!(dial_key(Some("dev"), "1.2.3.4:5"), "dev");
        assert_eq!(dial_key(None, "1.2.3.4:5"), "1.2.3.4:5");
    }

    #[test]
    fn dial_ledger_dedupes_and_clears_on_success() {
        let mut ledger = DialLedger::new();
        assert!(ledger.begin("peer-a"));
        assert!(!ledger.begin("peer-a"), "a second dial must be a no-op");
        assert!(ledger.in_flight("peer-a"));
        ledger.succeeded("peer-a");
        assert!(!ledger.in_flight("peer-a"));
        assert!(!ledger.waiting("peer-a", 0));
        assert!(
            ledger.begin("peer-a"),
            "a dial may start again after success"
        );
    }

    #[test]
    fn dial_ledger_waits_then_forgets_after_repeated_failures() {
        let mut ledger = DialLedger::new();
        ledger.begin("10.0.0.9:42424");
        assert!(!ledger.failed("10.0.0.9:42424", 1_000, 3_000, 3));
        assert!(ledger.waiting("10.0.0.9:42424", 3_999));
        assert!(!ledger.waiting("10.0.0.9:42424", 4_000));
        assert!(!ledger.failed("10.0.0.9:42424", 5_000, 3_000, 3));
        assert!(ledger.failed("10.0.0.9:42424", 9_000, 3_000, 3));
        ledger.forget("10.0.0.9:42424");
        assert!(!ledger.failed("10.0.0.9:42424", 10_000, 3_000, 3));
    }

    #[test]
    fn every_phase_maps_to_a_ui_status() {
        assert_eq!(Phase::Discovered.status(), "discovered");
        assert_eq!(Phase::Dialing.status(), "connecting");
        assert_eq!(
            Phase::Handshaking {
                link_id: 1,
                dialed: true
            }
            .status(),
            "connecting"
        );
        assert_eq!(
            Phase::Connected {
                paired: false,
                link_id: 1,
                dialed: false
            }
            .status(),
            "unpaired"
        );
        assert_eq!(
            Phase::Connected {
                paired: true,
                link_id: 1,
                dialed: true
            }
            .status(),
            "paired"
        );
        assert_eq!(Phase::Closing.status(), "offline");
    }
}
