fn main() {
    let host = hop_platform::install();
    for display in host.list_displays() {
        println!(
            "{} primary={} origin=({}, {}) size={}x{} scale={:.2}",
            display.name,
            display.primary,
            display.x,
            display.y,
            display.width,
            display.height,
            display.scale
        );
    }
    if let Some((x, y)) = host.cursor_pos() {
        println!("cursor {x},{y}");
        host.warp(x, y);
        println!("warp ok");
    }
}
