//! Pure ADB device serial validation and strict multi-device selection policy.
//!
//! Enforces ARB's safety invariant across `arbd`, `arb-cli`, `arb-tray`, and `ide-plugin`:
//! - When no preferred serial is configured:
//!   - Exactly 1 online device -> auto-select it cleanly.
//!   - 2 or more online devices -> NEVER auto-select; require explicit user selection.
//! - When a preferred serial IS configured:
//!   - If that device is online -> select it.
//!   - If that device disconnects / is missing -> NEVER silently switch to another online device!

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Maximum permitted byte length of an ADB transport serial.
pub const MAX_ADB_SERIAL_LEN: usize = 255;

/// Validation error returned by [`validate_adb_serial`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SerialValidationError {
    /// Serial string was empty or exceeded [`MAX_ADB_SERIAL_LEN`].
    #[error("invalid ADB serial '{value}': {reason}")]
    Invalid {
        /// Rejected serial string.
        value: String,
        /// Human-readable explanation of the rule violated.
        reason: &'static str,
    },
}

/// Validates an ADB device transport serial, accepting USB serials (`R3CN1234`, `emulator-5554`),
/// IPv4 Wi-Fi ADB (`192.168.1.50:5555`), IPv6 Wi-Fi ADB (`[fe80::1]:5555`), and mDNS TLS serials
/// (`adb-R3CN1234-AbCdEf._adb-tls-connect._tcp`) while strictly rejecting whitespace, control
/// characters, shell metacharacters, leading `-`, or `..` traversal segments.
///
/// # Errors
///
/// Returns [`SerialValidationError::Invalid`] if `serial` fails any safety rule.
pub fn validate_adb_serial(serial: &str) -> Result<(), SerialValidationError> {
    if serial.is_empty() {
        return Err(SerialValidationError::Invalid {
            value: serial.to_owned(),
            reason: "serial must not be empty",
        });
    }
    if serial.len() > MAX_ADB_SERIAL_LEN {
        return Err(SerialValidationError::Invalid {
            value: serial.to_owned(),
            reason: "serial exceeds 255 bytes",
        });
    }
    if serial.starts_with('-') {
        return Err(SerialValidationError::Invalid {
            value: serial.to_owned(),
            reason: "serial must not start with '-'",
        });
    }
    if serial.contains("..") {
        return Err(SerialValidationError::Invalid {
            value: serial.to_owned(),
            reason: "serial must not contain '..'",
        });
    }
    let allowed = serial
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b':' | b'[' | b']'));
    if !allowed {
        return Err(SerialValidationError::Invalid {
            value: serial.to_owned(),
            reason: "serial contains disallowed characters (only ASCII alphanumerics and . _ - : [ ] are allowed)",
        });
    }
    // Validate optional IPv6 bracket syntax: at most one leading '[' paired with ']' before optional ':port'.
    let open_count = serial.bytes().filter(|&b| b == b'[').count();
    let close_count = serial.bytes().filter(|&b| b == b']').count();
    if open_count != close_count || open_count > 1 {
        return Err(SerialValidationError::Invalid {
            value: serial.to_owned(),
            reason: "serial has unbalanced IPv6 brackets",
        });
    }
    if open_count == 1 {
        let Some(rest) = serial.strip_prefix('[') else {
            return Err(SerialValidationError::Invalid {
                value: serial.to_owned(),
                reason: "IPv6 bracket '[' must appear at the start of the serial",
            });
        };
        let Some((inside, after)) = rest.split_once(']') else {
            return Err(SerialValidationError::Invalid {
                value: serial.to_owned(),
                reason: "serial has malformed IPv6 brackets",
            });
        };
        if inside.is_empty() {
            return Err(SerialValidationError::Invalid {
                value: serial.to_owned(),
                reason: "IPv6 host inside brackets must not be empty",
            });
        }
        if !after.is_empty() {
            let Some(port_str) = after.strip_prefix(':') else {
                return Err(SerialValidationError::Invalid {
                    value: serial.to_owned(),
                    reason: "IPv6 bracketed serial may only be followed by ':port'",
                });
            };
            if port_str.is_empty() || !port_str.bytes().all(|b| b.is_ascii_digit()) {
                return Err(SerialValidationError::Invalid {
                    value: serial.to_owned(),
                    reason: "IPv6 serial port must be numeric",
                });
            }
        }
    }
    Ok(())
}

/// Returns `true` iff `serial` satisfies [`validate_adb_serial`].
#[must_use]
pub fn is_valid_adb_serial(serial: &str) -> bool {
    validate_adb_serial(serial).is_ok()
}

/// Result of evaluating the strict multi-device target selection policy.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DeviceSelectionOutcome {
    /// Target device serial is selected and currently online.
    Selected {
        /// Selected ADB device serial.
        serial: String,
    },
    /// User's preferred serial is currently offline or absent; never auto-switch to other devices.
    WaitingForPreferredDevice {
        /// Bound preferred serial that we are waiting to reconnect.
        preferred_serial: String,
        /// Other online devices that are intentionally NOT switched to.
        other_online_serials: Vec<String>,
    },
    /// No online devices are connected and no preferred serial is bound.
    NoOnlineDevices,
    /// Two or more online devices are connected without an explicit selection.
    /// Capture must NOT auto-start until the user picks one explicitly.
    MultipleDevicesNeedSelection {
        /// Online device serials awaiting explicit user choice.
        online_serials: Vec<String>,
    },
}

/// Pure, stateless strict multi-device target selection policy shared across daemon, CLI, and tray.
#[derive(Debug, Clone, Copy, Default)]
pub struct DeviceSelectionPolicy;

impl DeviceSelectionPolicy {
    /// Evaluates which device should be targeted given an optional `preferred_serial`
    /// and the list of currently online (`state == device`) ADB serials.
    ///
    /// Guarantees:
    /// 1. If `preferred_serial` is set (non-empty):
    ///    - Returns [`DeviceSelectionOutcome::Selected`] when it is online.
    ///    - Returns [`DeviceSelectionOutcome::WaitingForPreferredDevice`] when it is offline
    ///      or missing, even when other devices are online (NEVER auto-switches).
    /// 2. If `preferred_serial` is unset/empty:
    ///    - 0 online -> [`DeviceSelectionOutcome::NoOnlineDevices`]
    ///    - 1 online -> [`DeviceSelectionOutcome::Selected`]
    ///    - 2+ online -> [`DeviceSelectionOutcome::MultipleDevicesNeedSelection`]
    #[must_use]
    pub fn evaluate<S: AsRef<str>>(
        preferred_serial: Option<&str>,
        online_serials: &[S],
    ) -> DeviceSelectionOutcome {
        let online: Vec<String> = online_serials
            .iter()
            .map(|s| s.as_ref().trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect();

        if let Some(pref) = preferred_serial.map(str::trim).filter(|s| !s.is_empty()) {
            if online.iter().any(|s| s == pref) {
                return DeviceSelectionOutcome::Selected {
                    serial: pref.to_owned(),
                };
            }
            return DeviceSelectionOutcome::WaitingForPreferredDevice {
                preferred_serial: pref.to_owned(),
                other_online_serials: online,
            };
        }

        match online.as_slice() {
            [] => DeviceSelectionOutcome::NoOnlineDevices,
            [only] => DeviceSelectionOutcome::Selected {
                serial: only.clone(),
            },
            many => DeviceSelectionOutcome::MultipleDevicesNeedSelection {
                online_serials: many.to_vec(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DeviceSelectionOutcome, DeviceSelectionPolicy, is_valid_adb_serial, validate_adb_serial,
    };

    #[test]
    fn accepts_usb_emulator_wifi_ipv4_ipv6_and_mdns_tls_serials() {
        let valid = [
            "emulator-5554",
            "R3CN1234567",
            "192.168.1.50:5555",
            "[fe80::1]:5555",
            "[2001:db8::42]:37891",
            "adb-R3CN1234-AbCdEf._adb-tls-connect._tcp",
            "adb-2A121FDH200ABC-xYz123._adb-tls-connect._tcp.",
        ];
        for serial in valid {
            assert_eq!(
                validate_adb_serial(serial),
                Ok(()),
                "expected valid ADB serial: {serial}"
            );
            assert!(is_valid_adb_serial(serial), "is_valid_adb_serial({serial})");
        }
    }

    #[test]
    fn rejects_whitespace_control_shell_metachars_and_traversal_serials() {
        let invalid = [
            "",
            " ",
            "emulator-5554;rm -rf /",
            "192.168.1.50:5555 && id",
            "dev|cat",
            "dev$(whoami)",
            "dev`id`",
            "dev\nline2",
            "dev\x00null",
            "../escape",
            "foo..bar",
            "-flag-injection",
            "foo/bar",
            "foo\\bar",
            "[fe80::1",
            "fe80::1]:5555",
            "[]:5555",
            "[fe80::1]:abc",
            "a b",
        ];
        for serial in invalid {
            assert!(
                validate_adb_serial(serial).is_err(),
                "expected invalid ADB serial to be rejected: {serial:?}"
            );
        }
    }

    #[test]
    fn policy_enforces_strict_no_auto_switch_and_multi_device_guard() {
        // 0 devices, no preference -> NoOnlineDevices.
        assert_eq!(
            DeviceSelectionPolicy::evaluate::<&str>(None, &[]),
            DeviceSelectionOutcome::NoOnlineDevices
        );

        // 1 online device, no preference -> auto-select cleanly.
        assert_eq!(
            DeviceSelectionPolicy::evaluate(None, &["emulator-5554"]),
            DeviceSelectionOutcome::Selected {
                serial: "emulator-5554".to_owned(),
            }
        );

        // 2+ online devices, no preference -> MUST NOT auto-select.
        assert_eq!(
            DeviceSelectionPolicy::evaluate(None, &["emulator-5554", "192.168.1.50:5555"]),
            DeviceSelectionOutcome::MultipleDevicesNeedSelection {
                online_serials: vec!["emulator-5554".to_owned(), "192.168.1.50:5555".to_owned()],
            }
        );

        // Preferred device online among multiple -> Selected.
        assert_eq!(
            DeviceSelectionPolicy::evaluate(
                Some("192.168.1.50:5555"),
                &["emulator-5554", "192.168.1.50:5555"]
            ),
            DeviceSelectionOutcome::Selected {
                serial: "192.168.1.50:5555".to_owned(),
            }
        );

        // Preferred device disconnects while another device stays online -> NEVER switches!
        assert_eq!(
            DeviceSelectionPolicy::evaluate(Some("192.168.1.50:5555"), &["emulator-5554"]),
            DeviceSelectionOutcome::WaitingForPreferredDevice {
                preferred_serial: "192.168.1.50:5555".to_owned(),
                other_online_serials: vec!["emulator-5554".to_owned()],
            }
        );
    }
}
