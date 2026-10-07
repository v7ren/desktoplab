//! Latency samples and the bookkeeping for one remote session.

use std::collections::VecDeque;

use hop_core::DeviceId;

#[derive(Clone, Debug)]
pub struct RemoteSession {
    pub peer: DeviceId,
    pub samples_us: VecDeque<u64>,
}

impl RemoteSession {
    pub fn new(peer: DeviceId) -> Self {
        Self {
            peer,
            samples_us: VecDeque::with_capacity(256),
        }
    }

    pub fn push_us(&mut self, sample: u64) {
        if self.samples_us.len() == 256 {
            self.samples_us.pop_front();
        }
        self.samples_us.push_back(sample);
    }

    pub fn p95_us(&self) -> Option<u64> {
        if self.samples_us.is_empty() {
            return None;
        }
        let mut ordered: Vec<u64> = self.samples_us.iter().copied().collect();
        ordered.sort_unstable();
        let idx = ((ordered.len() - 1) as f64 * 0.95).round() as usize;
        ordered.get(idx).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p95_of_a_short_window() {
        let mut session = RemoteSession::new(DeviceId("p".into()));
        for n in 1..=20 {
            session.push_us(n * 100);
        }
        let p95 = session.p95_us().unwrap();
        assert!((1800..=2000).contains(&p95), "{p95}");
    }
}
