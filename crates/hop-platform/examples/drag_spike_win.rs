//! Throwaway check: can an invisible window call `DoDragDrop` with file paths?

fn main() {
    #[cfg(windows)]
    windows();
    #[cfg(not(windows))]
    println!("drag_spike_win is Windows-only");
}

#[cfg(windows)]
fn windows() {
    use hop_platform::Host;
    println!(
        "fallback folder: {}",
        hop_platform::windows::fallback_dir().display()
    );
    println!("DoDragDrop + CF_HDROP is implemented in hop_platform::windows::drag_target.");
    println!("Pass a path to drag it, or run with no args to only print the fallback.");
    let mut args = std::env::args().skip(1);
    if let Some(path) = args.next() {
        match hop_platform::windows::WindowsHost::new().begin_file_drag(&[path.into()]) {
            Ok(()) => println!("drop accepted"),
            Err(err) => println!("drop result: {err}"),
        }
    }
}
