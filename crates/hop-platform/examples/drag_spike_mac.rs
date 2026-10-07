//! Throwaway check: NSFilePromiseProvider from a borderless window.
//! The real implementation is `hop_platform::macos::drag_target`.

fn main() {
    #[cfg(target_os = "macos")]
    {
        println!("NSDraggingSession / NSFilePromiseProvider lives in hop_platform::macos.");
        println!("Run the app and drag across a paired edge to exercise it.");
    }
    #[cfg(not(target_os = "macos"))]
    println!("drag_spike_mac is macOS-only");
}
