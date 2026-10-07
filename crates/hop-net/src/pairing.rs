//! Six-digit short authentication string and the gate that blocks input
//! until both people confirm it.

use sha2::{Digest, Sha256};

/// Code both machines display. It depends only on the two fingerprints, so
/// neither side has to send the code (a network attacker would still have to
/// show the same number on both screens).
pub fn sas_code(fingerprint_a: &str, fingerprint_b: &str) -> String {
    let (left, right) = if fingerprint_a <= fingerprint_b {
        (fingerprint_a, fingerprint_b)
    } else {
        (fingerprint_b, fingerprint_a)
    };
    let mut hasher = Sha256::new();
    hasher.update(left.as_bytes());
    hasher.update(b"|");
    hasher.update(right.as_bytes());
    let dig = hasher.finalize();
    let n = u32::from_be_bytes([dig[0], dig[1], dig[2], dig[3]]) % 1_000_000;
    format!("{n:06}")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairGate {
    pinned: bool,
    local_ok: bool,
    remote_ok: bool,
    rejected: bool,
    code: String,
}

impl PairGate {
    pub fn new(pinned: bool, local_fp: &str, remote_fp: &str) -> Self {
        Self {
            pinned,
            local_ok: pinned,
            remote_ok: pinned,
            rejected: false,
            code: sas_code(local_fp, remote_fp),
        }
    }

    pub fn code(&self) -> &str {
        &self.code
    }

    pub fn confirm_local(&mut self) {
        if !self.rejected {
            self.local_ok = true;
        }
    }

    pub fn confirm_remote(&mut self) {
        if !self.rejected {
            self.remote_ok = true;
        }
    }

    pub fn reject(&mut self) {
        self.rejected = true;
        self.local_ok = false;
        self.remote_ok = false;
    }

    /// Input messages are delivered only after pairing (R7).
    pub fn accepts_input(&self) -> bool {
        !self.rejected && self.local_ok && self.remote_ok
    }

    pub fn ready_to_pin(&self) -> bool {
        !self.pinned && self.accepts_input()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpaired_input_is_refused_until_both_confirm() {
        let mut gate = PairGate::new(false, "alpha", "beta");
        assert!(!gate.accepts_input());
        assert_eq!(gate.code().len(), 6);
        gate.confirm_local();
        assert!(!gate.accepts_input(), "one side is not enough");
        gate.confirm_remote();
        assert!(gate.accepts_input());
        assert!(gate.ready_to_pin());
    }

    #[test]
    fn a_pinned_peer_accepts_immediately() {
        let gate = PairGate::new(true, "alpha", "beta");
        assert!(gate.accepts_input());
        assert!(!gate.ready_to_pin());
    }

    #[test]
    fn reject_closes_the_gate_again() {
        let mut gate = PairGate::new(false, "alpha", "beta");
        gate.confirm_local();
        gate.confirm_remote();
        gate.reject();
        assert!(!gate.accepts_input());
        gate.confirm_local();
        gate.confirm_remote();
        assert!(!gate.accepts_input());
    }

    #[test]
    fn the_code_does_not_depend_on_argument_order() {
        assert_eq!(sas_code("a", "b"), sas_code("b", "a"));
        assert_ne!(sas_code("a", "b"), sas_code("a", "c"));
        assert!(sas_code("a", "b").chars().all(|c| c.is_ascii_digit()));
    }
}
