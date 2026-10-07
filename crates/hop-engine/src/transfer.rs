//! Active and recent file transfers.

use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct TransferSnap {
    pub id: u64,
    pub label: String,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub bytes_per_sec: u64,
    pub state: String,
    pub path: String,
}

#[derive(Clone, Debug)]
pub struct TransferBook {
    pub items: Vec<TransferSnap>,
    next_id: u64,
}

impl TransferBook {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            next_id: 1,
        }
    }

    pub fn start(&mut self, label: String, total: u64) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.items.insert(
            0,
            TransferSnap {
                id,
                label,
                bytes_done: 0,
                bytes_total: total,
                bytes_per_sec: 0,
                state: "active".into(),
                path: String::new(),
            },
        );
        id
    }

    pub fn progress(&mut self, id: u64, done: u64, total: u64, per_sec: u64) {
        if let Some(item) = self.items.iter_mut().find(|item| item.id == id) {
            item.bytes_done = done;
            item.bytes_total = total;
            item.bytes_per_sec = per_sec;
            item.state = "active".into();
        }
    }

    pub fn finish(&mut self, id: u64, path: String, state: &str) {
        if let Some(item) = self.items.iter_mut().find(|item| item.id == id) {
            item.state = state.into();
            item.path = path;
            item.bytes_done = item.bytes_total;
        }
    }

    pub fn cancel_flag(&mut self, id: u64) -> bool {
        if let Some(item) = self.items.iter_mut().find(|item| item.id == id) {
            if item.state == "active" {
                item.state = "cancel".into();
                return true;
            }
        }
        false
    }
}

impl Default for TransferBook {
    fn default() -> Self {
        Self::new()
    }
}
