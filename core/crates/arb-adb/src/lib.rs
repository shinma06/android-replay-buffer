//! ADB session management and device-agent wire protocol for ARB v2.
//!
//! Manages device discovery, auto-reconnect, session-scoped `app_process`
//! launch of the scrcpy-server 5.0.1 capture agent and clock/beacon/logd
//! companion, and binary framing between Android devices and `arbd`.

/// Returns the crate identifier alongside the underlying domain schema version.
pub const fn crate_info() -> (&'static str, u32) {
    ("arb-adb", arb_core::BUNDLE_SCHEMA_VERSION)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoke_crate_info_links_arb_core() {
        assert_eq!(crate_info(), ("arb-adb", 2));
    }
}
