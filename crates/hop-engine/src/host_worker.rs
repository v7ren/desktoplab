//! Runs blocking [`Host`] calls off the engine thread and posts results as
//! [`InputEvent`]s.

use hop_platform::{ClipboardItem, Host, InputEvent};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

use crossbeam_channel::{Receiver, Sender};

#[derive(Debug)]
pub enum HostJob {
    BeginFileDrag(Vec<PathBuf>),
    ClipboardRead,
    ClipboardWrite(Vec<ClipboardItem>),
    PromptPermissions,
}

pub fn spawn(host: Arc<dyn Host>) -> (Sender<HostJob>, Receiver<InputEvent>) {
    let (job_tx, job_rx) = crossbeam_channel::unbounded();
    let (result_tx, result_rx) = crossbeam_channel::unbounded();
    thread::Builder::new()
        .name("devhop-host".into())
        .spawn(move || host_worker_main(host, job_rx, result_tx))
        .ok();
    (job_tx, result_rx)
}

fn host_worker_main(host: Arc<dyn Host>, jobs: Receiver<HostJob>, out: Sender<InputEvent>) {
    while let Ok(job) = jobs.recv() {
        match job {
            HostJob::BeginFileDrag(paths) => {
                let accepted = host.begin_file_drag(&paths).is_ok();
                let _ = out.send(InputEvent::DragFinished { accepted });
            }
            HostJob::ClipboardRead => {
                let items = host.clipboard_read();
                let _ = out.send(InputEvent::ClipboardRead { items });
            }
            HostJob::ClipboardWrite(items) => {
                let ok = host.clipboard_write(&items).is_ok();
                let _ = out.send(InputEvent::ClipboardWritten { ok });
            }
            HostJob::PromptPermissions => {
                host.prompt_permissions();
                let _ = out.send(InputEvent::PermissionsChanged(host.permissions()));
            }
        }
    }
}
