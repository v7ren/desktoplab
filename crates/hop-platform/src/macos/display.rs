//! macOS displays. Bounds from CoreGraphics are points; we store physical pixels.

use core_graphics::display::CGDisplay;

use crate::DisplayInfo;

pub fn list() -> Vec<DisplayInfo> {
    let ids = CGDisplay::active_displays().unwrap_or_default();
    let mut out = Vec::new();
    for id in ids {
        let display = CGDisplay { id };
        let bounds = display.bounds();
        let pixels_w = display.pixels_wide();
        let pixels_h = display.pixels_high();
        let scale = if bounds.size.width > 0.5 {
            pixels_w as f64 / bounds.size.width
        } else {
            1.0
        };
        out.push(DisplayInfo {
            id: id.to_string(),
            name: format!("Display {id}"),
            x: (bounds.origin.x * scale).round() as i32,
            y: (bounds.origin.y * scale).round() as i32,
            width: pixels_w as u32,
            height: pixels_h as u32,
            scale,
            primary: display.is_main(),
        });
    }
    out
}
