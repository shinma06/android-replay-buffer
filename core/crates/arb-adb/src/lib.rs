//! ADB transport, device-agent binary wire protocol, scrcpy-server stream parsing,
//! auto-reconnect session supervision, and deterministic fault-injection harness
//! for Android Replay Buffer (ARB) v2.
//!
//! # Architecture
//!
//! - [`wire`]: Exact 20-byte Little-Endian binary frame header and payload codec
//!   matching `device-agent/PROTOCOL.md` bit-for-bit, with strict `max_payload_len`
//!   denial-of-service guards.
//! - [`scrcpy`]: Big-Endian video (`h264`) and audio (`aac`) tunnel parsers for
//!   Apache-2.0 `scrcpy-server-v5.0.1`, including explicit API < 33 audio
//!   unavailability classification.
//! - [`adb`]: Host `adb` binary discovery, session-scoped `app_process` deployment
//!   and cleanup (never invoking `adb kill-server`), exponential-backoff reconnect
//!   policy, `SessionSupervisor` state machine, and live [`adb::AdbSessionSource`].
//! - [`mock`]: Unified [`mock::CaptureEvent`] stream, [`mock::DeviceEventSource`]
//!   trait, and deterministic [`mock::MockDeviceSource`] / [`mock::FaultScenario`]
//!   harness exercising all ARB edge cases without physical Android hardware.

pub mod adb;
pub mod mock;
pub mod scrcpy;
pub mod wire;

/// Returns the crate identifier alongside the underlying domain schema version.
pub const fn crate_info() -> (&'static str, u32) {
    ("arb-adb", arb_core::BUNDLE_SCHEMA_VERSION)
}

#[cfg(test)]
mod tests {
    use super::crate_info;

    #[test]
    fn smoke_crate_info_links_arb_core() {
        assert_eq!(
            crate_info(),
            ("arb-adb", 2),
            "crate_info must report arb-adb linked to schema v2"
        );
    }
}
