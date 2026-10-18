//! macOS displays. CoreGraphics reports bounds in **points**; cursor/warp/inject use the same space.

use core_graphics::display::CGDisplay;

use crate::units::display_info_from_points;
use crate::DisplayInfo;

pub fn list() -> Vec<DisplayInfo> {
    let ids = CGDisplay::active_displays().unwrap_or_default();
    let mut out = Vec::new();
    for id in ids {
        let display = CGDisplay { id };
        let bounds = display.bounds();
        out.push(display_info_from_points(
            id.to_string(),
            format!("Display {id}"),
            bounds.origin.x,
            bounds.origin.y,
            bounds.size.width,
            bounds.size.height,
            display.pixels_wide() as u32,
            display.pixels_high() as u32,
            display.is_main(),
        ));
    }
    out
}

/// Clamp a point to the display that contains `anchor`, in point coordinates.
/// Keeps at least one point inside the display so the cursor can reach the bottom edge / Dock.
pub fn clamp_point_to_display(anchor_x: i32, anchor_y: i32, x: i32, y: i32) -> (i32, i32) {
    let Some((ox, oy, w, h)) = display_bounds_containing(anchor_x, anchor_y) else {
        return (x, y);
    };
    let max_x = ox + w as i32 - 1;
    let max_y = oy + h as i32 - 1;
    (x.clamp(ox, max_x), y.clamp(oy, max_y))
}

fn display_bounds_containing(px: i32, py: i32) -> Option<(i32, i32, u32, u32)> {
    let ids = CGDisplay::active_displays().unwrap_or_default();
    for id in ids {
        let display = CGDisplay { id };
        let bounds = display.bounds();
        let ox = bounds.origin.x.round() as i32;
        let oy = bounds.origin.y.round() as i32;
        let w = bounds.size.width.round() as u32;
        let h = bounds.size.height.round() as u32;
        if px >= ox && py >= oy && px < ox + w as i32 && py < oy + h as i32 {
            return Some((ox, oy, w, h));
        }
    }
    None
}
