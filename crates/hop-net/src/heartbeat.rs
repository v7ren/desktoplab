//! Heartbeats are 100 ms apart. Three misses is 300 ms, which is the deadline
//! in R5 for releasing keys.

pub const HEARTBEAT_MS: u64 = 100;
pub const MISS_LIMIT: u8 = 3;

#[derive(Clone, Debug)]
pub struct HeartbeatWatch {
    last_ms: u64,
    misses: u8,
    interval_ms: u64,
    limit: u8,
}

impl HeartbeatWatch {
    pub fn new(now_ms: u64) -> Self {
        Self {
            last_ms: now_ms,
            misses: 0,
            interval_ms: HEARTBEAT_MS,
            limit: MISS_LIMIT,
        }
    }

    pub fn beat(&mut self, now_ms: u64) {
        self.last_ms = now_ms;
        self.misses = 0;
    }

    pub fn misses(&self) -> u8 {
        self.misses
    }

    /// Advance virtual time. `true` means the peer is gone.
    pub fn poll(&mut self, now_ms: u64) -> bool {
        while now_ms.saturating_sub(self.last_ms) >= self.interval_ms {
            self.misses = self.misses.saturating_add(1);
            self.last_ms = self.last_ms.saturating_add(self.interval_ms);
            if self.misses >= self.limit {
                return true;
            }
        }
        false
    }
}

/// Round-trip samples from echoed heartbeat sequence numbers.
#[derive(Clone, Debug, Default)]
pub struct RttProbe {
    sent_us: std::collections::BTreeMap<u64, u64>,
    samples_us: Vec<u64>,
}

impl RttProbe {
    pub fn sent(&mut self, seq: u64, now_us: u64) {
        self.sent_us.insert(seq, now_us);
        while self.sent_us.len() > 64 {
            if let Some(first) = self.sent_us.keys().next().copied() {
                self.sent_us.remove(&first);
            }
        }
    }

    pub fn echo(&mut self, seq: u64, now_us: u64) -> Option<u64> {
        let started = self.sent_us.remove(&seq)?;
        let rtt = now_us.saturating_sub(started);
        self.samples_us.push(rtt);
        if self.samples_us.len() > 256 {
            let drain = self.samples_us.len() - 256;
            self.samples_us.drain(0..drain);
        }
        Some(rtt)
    }

    pub fn p95_us(&self) -> Option<u64> {
        if self.samples_us.is_empty() {
            return None;
        }
        let mut ordered = self.samples_us.clone();
        ordered.sort_unstable();
        let idx = ((ordered.len() - 1) as f64 * 0.95).round() as usize;
        ordered.get(idx).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_silent_intervals_is_three_hundred_milliseconds() {
        let mut watch = HeartbeatWatch::new(0);
        assert!(!watch.poll(299));
        assert_eq!(watch.misses(), 2);
        assert!(watch.poll(300));
        assert_eq!(watch.misses(), 3);
    }

    #[test]
    fn a_beat_resets_the_window() {
        let mut watch = HeartbeatWatch::new(0);
        assert!(!watch.poll(200));
        watch.beat(250);
        assert!(!watch.poll(549));
        assert!(watch.poll(550));
    }

    #[test]
    fn rtt_p95_uses_the_echoed_sequence() {
        let mut probe = RttProbe::default();
        probe.sent(1, 1_000);
        probe.sent(2, 2_000);
        assert_eq!(probe.echo(1, 1_400), Some(400));
        assert_eq!(probe.echo(2, 2_900), Some(900));
        assert!(probe.echo(9, 0).is_none());
        let p95 = probe.p95_us().unwrap();
        assert!(p95 == 400 || p95 == 900);
    }
}
