//! Turn the settings strings into chords the hook can match.

use hop_core::{digit_hid, parse_chord, Chord, HotAction};

use crate::config::HotkeyConfig;

pub fn expand(config: &HotkeyConfig) -> Vec<(Chord, HotAction)> {
    let mut out = Vec::new();
    push(&mut out, &config.next_monitor, HotAction::NextMonitor);
    push(&mut out, &config.prev_monitor, HotAction::PrevMonitor);
    push(&mut out, &config.center, HotAction::Center);
    push(&mut out, &config.find_cursor, HotAction::FindCursor);
    push(&mut out, &config.lock_monitor, HotAction::LockMonitor);
    push(&mut out, &config.return_home, HotAction::ReturnHome);
    push(&mut out, &config.panic, HotAction::Panic);
    expand_digits(&mut out, &config.monitor_n, HotAction::Monitor);
    expand_digits(&mut out, &config.jump_device_n, HotAction::JumpDevice);
    out
}

fn push(out: &mut Vec<(Chord, HotAction)>, text: &str, action: HotAction) {
    if let Some(chord) = parse_chord(text) {
        out.push((chord, action));
    }
}

fn expand_digits(out: &mut Vec<(Chord, HotAction)>, pattern: &str, action: fn(u8) -> HotAction) {
    for n in 1..=9 {
        let text = pattern.replace("{n}", &n.to_string());
        if let Some(mut chord) = parse_chord(&text) {
            if let Some(hid) = digit_hid(n) {
                chord.hid = hid;
            }
            out.push((chord, action(n)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::HotkeyConfig;

    #[test]
    fn numbered_chords_use_digit_hids() {
        let chords = expand(&HotkeyConfig::default());
        let monitors: Vec<_> = chords
            .iter()
            .filter_map(|(_, action)| match action {
                HotAction::Monitor(n) => Some(*n),
                _ => None,
            })
            .collect();
        assert_eq!(monitors, vec![1, 2, 3, 4, 5, 6, 7, 8, 9]);
        let panic = chords
            .iter()
            .find(|(_, action)| matches!(action, HotAction::Panic))
            .unwrap();
        assert!(panic.0.ctrl && panic.0.alt);
        assert_eq!(panic.0.hid, 0x29);
    }
}
