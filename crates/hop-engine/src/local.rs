//! Local hops and DPI-proportional crossing. Pure: the engine applies the effect.

use std::collections::HashSet;

use hop_core::{
    cross, denormalize, hop_monitors, normalize, reading_order, DeviceId, HopAction, HopMode,
    Layout, MonitorId, NormPos, PhysRect, PositionMemory,
};

#[derive(Clone, Debug)]
pub struct LocalSession {
    pub device: DeviceId,
    pub monitor: MonitorId,
    pub nx: f64,
    pub ny: f64,
    pub memory: PositionMemory,
    pub locked: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MotionEffect {
    Stay,
    Warp {
        x: i32,
        y: i32,
        monitor: MonitorId,
    },
    Handoff {
        peer: DeviceId,
        x: i32,
        y: i32,
        monitor: MonitorId,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum HotEffect {
    Warp { x: i32, y: i32, monitor: MonitorId },
    FindCursor { x: i32, y: i32 },
    Lock(bool),
    Panic,
    ReturnHome,
    JumpDevice(u8),
}

/// A cursor stuck on the last pixel with outward motion has left, even though
/// the OS has not moved it yet. That is the case DPI smoothing has to catch.
pub fn pushed_outside(bounds: PhysRect, x: i32, y: i32, dx: i32, dy: i32) -> Option<(i32, i32)> {
    if !bounds.contains(x, y) {
        return Some((x, y));
    }
    if dx > 0 && x >= bounds.right().saturating_sub(1) {
        return Some((bounds.right(), y));
    }
    if dx < 0 && x <= bounds.x {
        return Some((bounds.x.saturating_sub(1), y));
    }
    if dy > 0 && y >= bounds.bottom().saturating_sub(1) {
        return Some((x, bounds.bottom()));
    }
    if dy < 0 && y <= bounds.y {
        return Some((x, bounds.y.saturating_sub(1)));
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub fn on_motion(
    layout: &Layout,
    session: &mut LocalSession,
    x: i32,
    y: i32,
    dx: i32,
    dy: i32,
    paused: bool,
    online: &HashSet<DeviceId>,
) -> MotionEffect {
    let Some(current) = layout.monitor(&session.monitor).cloned() else {
        return MotionEffect::Stay;
    };
    if let Some(here) = layout.monitor_at(x, y) {
        if here.device_id == session.device && !session.locked && !paused {
            let (nx, ny) = normalize(here.bounds, x, y);
            session.monitor = here.id.clone();
            session.nx = nx;
            session.ny = ny;
        }
    }
    if paused || session.locked {
        return MotionEffect::Stay;
    }
    let Some((ox, oy)) = pushed_outside(current.bounds, x, y, dx, dy) else {
        return MotionEffect::Stay;
    };
    let Some(arrival) = cross(layout, &current.id, ox, oy) else {
        return MotionEffect::Stay;
    };
    if arrival.device_id == session.device {
        session
            .memory
            .remember(&NormPos::new(current.id.clone(), session.nx, session.ny));
        let (nx, ny) = normalize(
            layout
                .monitor(&arrival.monitor)
                .map(|m| m.bounds)
                .unwrap_or(current.bounds),
            arrival.x,
            arrival.y,
        );
        session.monitor = arrival.monitor.clone();
        session.nx = nx;
        session.ny = ny;
        MotionEffect::Warp {
            x: arrival.x,
            y: arrival.y,
            monitor: arrival.monitor,
        }
    } else if online.contains(&arrival.device_id) {
        MotionEffect::Handoff {
            peer: arrival.device_id,
            x: arrival.x,
            y: arrival.y,
            monitor: arrival.monitor,
        }
    } else {
        MotionEffect::Stay
    }
}

pub fn on_hotkey(
    layout: &Layout,
    session: &mut LocalSession,
    mode: HopMode,
    action: hop_core::HotAction,
) -> Option<HotEffect> {
    let current = NormPos::new(session.monitor.clone(), session.nx, session.ny);
    session.memory.remember(&current);
    match action {
        hop_core::HotAction::FindCursor => {
            let bounds = layout.monitor(&session.monitor)?.bounds;
            let (x, y) = denormalize(bounds, session.nx, session.ny);
            Some(HotEffect::FindCursor { x, y })
        }
        hop_core::HotAction::LockMonitor => {
            session.locked = !session.locked;
            Some(HotEffect::Lock(session.locked))
        }
        hop_core::HotAction::Panic => Some(HotEffect::Panic),
        hop_core::HotAction::ReturnHome => Some(HotEffect::ReturnHome),
        hop_core::HotAction::JumpDevice(n) => Some(HotEffect::JumpDevice(n)),
        hop_core::HotAction::NextMonitor => hop_to(layout, session, mode, HopAction::Next),
        hop_core::HotAction::PrevMonitor => hop_to(layout, session, mode, HopAction::Prev),
        hop_core::HotAction::Monitor(n) => hop_to(layout, session, mode, HopAction::Number(n)),
        hop_core::HotAction::Center => hop_to(layout, session, mode, HopAction::Center),
    }
}

fn hop_to(
    layout: &Layout,
    session: &mut LocalSession,
    mode: HopMode,
    action: HopAction,
) -> Option<HotEffect> {
    let current = NormPos::new(session.monitor.clone(), session.nx, session.ny);
    let dest = hop_monitors(
        layout,
        &session.device,
        mode,
        &session.memory,
        &current,
        action,
    )?;
    let bounds = layout.monitor(&dest.monitor)?.bounds;
    let (x, y) = denormalize(bounds, dest.nx, dest.ny);
    session.monitor = dest.monitor.clone();
    session.nx = dest.nx;
    session.ny = dest.ny;
    Some(HotEffect::Warp {
        x,
        y,
        monitor: dest.monitor,
    })
}

pub fn order_of(layout: &Layout, device: &DeviceId) -> Vec<MonitorId> {
    reading_order(layout, device)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hop_core::{Device, DeviceId, Layout, Monitor, MonitorId, PhysRect};

    fn mon(id: &str, dev: &str, x: i32, y: i32, w: u32, h: u32) -> Monitor {
        Monitor {
            id: MonitorId(id.into()),
            device_id: DeviceId(dev.into()),
            name: id.into(),
            bounds: PhysRect::new(x, y, w, h),
            scale: 1.0,
        }
    }

    fn desk(monitors: Vec<Monitor>, wrap: bool) -> Layout {
        Layout {
            devices: vec![
                Device {
                    id: DeviceId("local".into()),
                    name: "local".into(),
                },
                Device {
                    id: DeviceId("laptop".into()),
                    name: "laptop".into(),
                },
            ],
            monitors,
            wrap,
        }
    }

    fn session() -> LocalSession {
        LocalSession {
            device: DeviceId("local".into()),
            monitor: MonitorId("a".into()),
            nx: 0.9,
            ny: 0.5,
            memory: PositionMemory::default(),
            locked: false,
        }
    }

    #[test]
    fn pushing_off_a_shorter_neighbor_warps_proportionally() {
        let layout = desk(
            vec![
                mon("a", "local", 0, 0, 100, 200),
                mon("b", "local", 100, 0, 100, 100),
            ],
            false,
        );
        let mut session = session();
        session.ny = 0.75;
        let effect = on_motion(&layout, &mut session, 99, 150, 4, 0, false, &HashSet::new());
        match effect {
            MotionEffect::Warp { monitor, y, .. } => {
                assert_eq!(monitor.0, "b");
                assert!(y < 100, "y={y}");
            }
            other => panic!("expected warp, got {other:?}"),
        }
    }

    #[test]
    fn an_online_peer_takes_the_handoff() {
        let layout = desk(
            vec![
                mon("a", "local", 0, 0, 100, 100),
                mon("p", "laptop", 100, 0, 80, 80),
            ],
            false,
        );
        let mut online = HashSet::new();
        online.insert(DeviceId("laptop".into()));
        let effect = on_motion(&layout, &mut session(), 99, 40, 3, 0, false, &online);
        match effect {
            MotionEffect::Handoff { peer, .. } => assert_eq!(peer.0, "laptop"),
            other => panic!("expected handoff, got {other:?}"),
        }
    }

    #[test]
    fn lock_and_pause_suppress_crossing() {
        let layout = desk(
            vec![
                mon("a", "local", 0, 0, 100, 100),
                mon("b", "local", 100, 0, 100, 100),
            ],
            false,
        );
        let mut locked = session();
        locked.locked = true;
        assert_eq!(
            on_motion(&layout, &mut locked, 99, 40, 5, 0, false, &HashSet::new()),
            MotionEffect::Stay
        );
        assert_eq!(
            on_motion(&layout, &mut session(), 99, 40, 5, 0, true, &HashSet::new()),
            MotionEffect::Stay
        );
    }

    #[test]
    fn next_hotkey_keeps_relative_position() {
        let layout = desk(
            vec![
                mon("a", "local", 0, 0, 100, 100),
                mon("b", "local", 200, 0, 100, 100),
            ],
            false,
        );
        let mut session = session();
        session.nx = 0.3;
        session.ny = 0.3;
        let effect = on_hotkey(
            &layout,
            &mut session,
            HopMode::Relative,
            hop_core::HotAction::NextMonitor,
        )
        .unwrap();
        match effect {
            HotEffect::Warp { monitor, .. } => assert_eq!(monitor.0, "b"),
            other => panic!("{other:?}"),
        }
        assert!((session.nx - 0.3).abs() < f64::EPSILON);
    }
}
