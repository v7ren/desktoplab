//! Convert between host (physical) pixel coordinates and layout (virtual) space.

use hop_core::{DeviceId, Layout, MonitorId};

pub fn phys_to_layout(
    layout: &Layout,
    device: &DeviceId,
    x: i32,
    y: i32,
) -> Option<(MonitorId, i32, i32)> {
    for monitor in layout.monitors.iter().filter(|m| &m.device_id == device) {
        let phys = monitor.physical_bounds();
        if phys.contains(x, y) {
            let lx = monitor.bounds.x + (x - phys.x);
            let ly = monitor.bounds.y + (y - phys.y);
            return Some((monitor.id.clone(), lx, ly));
        }
    }
    None
}

pub fn phys_point_to_layout(
    layout: &Layout,
    device: &DeviceId,
    x: i32,
    y: i32,
    hint: &MonitorId,
) -> (i32, i32) {
    if let Some((_, lx, ly)) = phys_to_layout(layout, device, x, y) {
        return (lx, ly);
    }
    if let Some(monitor) = layout.monitor(hint) {
        let phys = monitor.physical_bounds();
        return (
            monitor.bounds.x + (x - phys.x),
            monitor.bounds.y + (y - phys.y),
        );
    }
    (x, y)
}

pub fn layout_to_phys(
    layout: &Layout,
    monitor_id: &MonitorId,
    lx: i32,
    ly: i32,
) -> Option<(i32, i32)> {
    let monitor = layout.monitor(monitor_id)?;
    let phys = monitor.physical_bounds();
    Some((
        phys.x + (lx - monitor.bounds.x),
        phys.y + (ly - monitor.bounds.y),
    ))
}

pub fn layout_point_to_phys(
    layout: &Layout,
    monitor_id: &MonitorId,
    lx: i32,
    ly: i32,
) -> (i32, i32) {
    layout_to_phys(layout, monitor_id, lx, ly).unwrap_or((lx, ly))
}

pub fn layout_center_phys(layout: &Layout, monitor_id: &MonitorId) -> Option<(i32, i32)> {
    let monitor = layout.monitor(monitor_id)?;
    let lx = monitor.bounds.x + (monitor.bounds.width as i32) / 2;
    let ly = monitor.bounds.y + (monitor.bounds.height as i32) / 2;
    layout_to_phys(layout, monitor_id, lx, ly)
}
