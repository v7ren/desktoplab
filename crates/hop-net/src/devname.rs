//! Human-readable device name for discovery adverts (N4.3).

/// Best-effort name for this computer in the device list.
///
/// On macOS, GUI apps often lack `HOSTNAME`; we prefer the user-visible
/// **Computer Name** from `scutil`. Elsewhere (and as fallback) we use the
/// `hostname` crate.
pub fn device_name() -> String {
    #[cfg(target_os = "macos")]
    if let Some(name) = macos_computer_name() {
        if !name.is_empty() {
            return name;
        }
    }

    hostname::get()
        .ok()
        .map(|h| h.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "devhop".into())
}

#[cfg(target_os = "macos")]
fn macos_computer_name() -> Option<String> {
    let output = std::process::Command::new("scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_name_is_non_empty() {
        let name = device_name();
        assert!(!name.is_empty());
    }
}
