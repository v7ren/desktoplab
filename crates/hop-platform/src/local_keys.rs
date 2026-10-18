//! Keys that were passed through to the OS before remote control. Windows hook logic
//! uses this so KEYUPs for locally-held keys still reach the OS while remote.

use std::collections::HashSet;

/// Virtual keys checked on return from remote (left/right variants included).
pub const MODIFIER_VKS: [u16; 11] = [
    0x10, // VK_SHIFT
    0xA0, // VK_LSHIFT
    0xA1, // VK_RSHIFT
    0x11, // VK_CONTROL
    0xA2, // VK_LCONTROL
    0xA3, // VK_RCONTROL
    0x12, // VK_MENU (Alt)
    0xA4, // VK_LMENU
    0xA5, // VK_RMENU
    0x5B, // VK_LWIN
    0x5C, // VK_RWIN
];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LocalKeyState {
    local_down: HashSet<u16>,
}

impl LocalKeyState {
    pub fn on_passed_down(&mut self, vk: u16) {
        self.local_down.insert(vk);
    }

    pub fn on_passed_up(&mut self, vk: u16) {
        self.local_down.remove(&vk);
    }

    pub fn should_pass_up_to_os_while_remote(&self, vk: u16) -> bool {
        self.local_down.contains(&vk)
    }

    pub fn clear(&mut self) {
        self.local_down.clear();
    }

    pub fn local_down_snapshot(&self) -> Vec<u16> {
        self.local_down.iter().copied().collect()
    }

    /// VKs to synthesize KEYUP for when leaving remote: modifiers and local_down
    /// entries that are not physically held (`is_down` == GetAsyncKeyState < 0).
    pub fn keys_to_release_on_remote_end(
        &self,
        is_physically_down: impl Fn(u16) -> bool,
    ) -> Vec<u16> {
        let mut out = HashSet::new();
        for vk in MODIFIER_VKS {
            if !is_physically_down(vk) {
                out.insert(vk);
            }
        }
        for vk in &self.local_down {
            if !is_physically_down(*vk) {
                out.insert(*vk);
            }
        }
        out.into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_down_passes_up_to_os_only_when_tracked() {
        let mut state = LocalKeyState::default();
        assert!(!state.should_pass_up_to_os_while_remote(0xA0));
        state.on_passed_down(0xA0);
        assert!(state.should_pass_up_to_os_while_remote(0xA0));
        state.on_passed_up(0xA0);
        assert!(!state.should_pass_up_to_os_while_remote(0xA0));
    }

    #[test]
    fn release_stuck_modifier_not_physically_held() {
        let state = LocalKeyState::default();
        let releases = state.keys_to_release_on_remote_end(|vk| vk == 0xA2);
        assert!(releases.contains(&0xA0));
        assert!(!releases.contains(&0xA2));
    }

    #[test]
    fn release_local_down_when_not_physically_held() {
        let mut state = LocalKeyState::default();
        state.on_passed_down(0x41);
        let releases = state.keys_to_release_on_remote_end(|_| false);
        assert!(releases.contains(&0x41));
    }

    #[test]
    fn keep_key_user_still_holds() {
        let mut state = LocalKeyState::default();
        state.on_passed_down(0xA0);
        let releases = state.keys_to_release_on_remote_end(|vk| vk == 0xA0);
        assert!(!releases.contains(&0xA0));
    }
}
