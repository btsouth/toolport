//! Authoritative Windows launch-at-login status.
//!
//! `auto-launch` infers the Task Manager state from timestamp bytes in
//! `StartupApproved`. Windows records the state in the first byte, and the
//! timestamp is not a reliable enabled flag after a restart.

use std::io::ErrorKind;
use winreg::enums::{RegType, HKEY_CURRENT_USER, KEY_READ};
use winreg::RegKey;

const RUN_KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Run";
const STARTUP_APPROVED_KEY: &str =
    r"SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";

/// Read Toolport's Run entry and Windows' Task Manager override without
/// converting an unknown or malformed value into a false Off state.
pub fn is_enabled(app_name: &str) -> Result<bool, String> {
    let current_user = RegKey::predef(HKEY_CURRENT_USER);
    let run = match current_user.open_subkey_with_flags(RUN_KEY, KEY_READ) {
        Ok(key) => key,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(format!(
                "Could not read the Windows startup registry: {error}"
            ))
        }
    };
    match run.get_value::<String, _>(app_name) {
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("Could not read Toolport's startup entry: {error}")),
    }

    let approved = match current_user.open_subkey_with_flags(STARTUP_APPROVED_KEY, KEY_READ) {
        Ok(key) => key,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(format!("Could not read Windows startup approval: {error}")),
    };
    let raw = match approved.get_raw_value(app_name) {
        Ok(raw) => raw,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(format!("Could not read Windows startup approval: {error}")),
    };
    startup_approved_value_enabled(raw.vtype, &raw.bytes)
        .ok_or_else(|| "Windows returned an invalid startup approval value".to_string())
}

fn startup_approved_value_enabled(vtype: RegType, bytes: &[u8]) -> Option<bool> {
    if vtype != RegType::REG_BINARY || bytes.len() != 12 {
        return None;
    }
    startup_approved_enabled(bytes)
}

/// Windows uses 1, 3, and 7 for disabled states. Known enabled states include
/// 0, 2, and 6. Reject other values so Settings can show unreadable, not Off.
fn startup_approved_enabled(bytes: &[u8]) -> Option<bool> {
    match bytes.first().copied()? {
        0 | 2 | 6 => Some(true),
        1 | 3 | 7 => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{startup_approved_enabled, startup_approved_value_enabled};
    use winreg::enums::RegType;

    #[test]
    fn startup_approval_uses_state_byte_not_timestamp() {
        assert_eq!(
            startup_approved_enabled(&[2, 0, 0, 0, 0xa5, 0x20, 0xf6, 0x4a, 0x95, 0xd7, 0xd9, 1]),
            Some(true)
        );
        assert_eq!(
            startup_approved_enabled(&[3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
            Some(false)
        );
        assert_eq!(startup_approved_enabled(&[7, 0, 0, 0]), Some(false));
        assert_eq!(startup_approved_enabled(&[]), None);
        assert_eq!(startup_approved_enabled(&[9]), None);
    }

    #[test]
    fn startup_approval_rejects_wrong_type_and_length() {
        let enabled = [2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(
            startup_approved_value_enabled(RegType::REG_SZ, &enabled),
            None
        );
        assert_eq!(
            startup_approved_value_enabled(RegType::REG_BINARY, &enabled[..4]),
            None
        );
        assert_eq!(
            startup_approved_value_enabled(RegType::REG_BINARY, &enabled),
            Some(true)
        );
    }
}
