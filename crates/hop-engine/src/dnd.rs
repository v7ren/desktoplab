//! Drag hand-off. Crossing with the button down starts a remote drag;
//! releasing it finishes the drop, and Escape or crossing home cancels it.

use hop_core::{ControlCommand, ControlEvent, ControlMachine, DeviceId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DragDecision {
    pub commands: Vec<ControlCommand>,
    pub phase_remote: bool,
}

pub fn begin_if_holding(
    machine: &mut ControlMachine,
    peer: DeviceId,
    button_held: bool,
) -> DragDecision {
    let commands = machine.handle(ControlEvent::CrossTo { peer, button_held });
    DragDecision {
        commands,
        phase_remote: matches!(machine.phase(), hop_core::Phase::Remote { .. }),
    }
}

#[allow(dead_code)]
pub fn finish(machine: &mut ControlMachine) -> Vec<ControlCommand> {
    machine.handle(ControlEvent::ButtonUp(0))
}

pub fn cancel(machine: &mut ControlMachine) -> Vec<ControlCommand> {
    machine.handle(ControlEvent::CancelDrag)
}

pub fn home(machine: &mut ControlMachine) -> Vec<ControlCommand> {
    machine.handle(ControlEvent::CrossHome)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_button_starts_and_finishes_a_drag() {
        let mut machine = ControlMachine::new();
        let peer = DeviceId("mac".into());
        let began = begin_if_holding(&mut machine, peer.clone(), true);
        assert!(began
            .commands
            .contains(&ControlCommand::BeginDrag { peer: peer.clone() }));
        assert!(machine.dragging());
        let done = finish(&mut machine);
        assert_eq!(done, vec![ControlCommand::FinishDrag { peer }]);
        assert!(!machine.dragging());
        assert!(matches!(machine.phase(), hop_core::Phase::Remote { .. }));
    }

    #[test]
    fn escape_cancels_and_crossing_home_releases() {
        let mut machine = ControlMachine::new();
        let peer = DeviceId("mac".into());
        begin_if_holding(&mut machine, peer.clone(), true);
        let cmds = cancel(&mut machine);
        assert_eq!(
            cmds,
            vec![ControlCommand::CancelDrag { peer: peer.clone() }]
        );
        let cmds = home(&mut machine);
        assert!(cmds.contains(&ControlCommand::ReleaseAll { peer }));
        assert!(matches!(machine.phase(), hop_core::Phase::Local));
    }
}
