#![forbid(unsafe_code)]

//! Pure domain logic for Android Replay Buffer (ARB).
//!
//! This crate has zero OS, ADB, SQLite, IPC, or media-codec dependencies.
//! It owns the dual-layer time model, the shared 3-valued selection engine
//! used by both save and trim, WORM bundle v2 schemas, legacy read-only
//! parsing helpers, and save/lease/ring-budget state machines.

pub mod analytics;
pub mod bundle;
pub mod config;
pub mod device;
pub mod gesture;
pub mod legacy;
pub mod selection;
pub mod state;
pub mod time;

/// Schema version for ARB immutable saved bundles.
pub const BUNDLE_SCHEMA_VERSION: u32 = 2;

/// Returns the crate identifier and bundle schema version for smoke diagnostics.
pub const fn crate_info() -> (&'static str, u32) {
    ("arb-core", BUNDLE_SCHEMA_VERSION)
}

/// Encodes binary data using the standard padded Base64 alphabet.
pub fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = usize::from(chunk[0]);
        let b1 = chunk.get(1).copied().map_or(0, usize::from);
        let b2 = chunk.get(2).copied().map_or(0, usize::from);
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(char::from(TABLE[(n >> 18) & 0x3F]));
        out.push(char::from(TABLE[(n >> 12) & 0x3F]));
        if chunk.len() > 1 {
            out.push(char::from(TABLE[(n >> 6) & 0x3F]));
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(char::from(TABLE[n & 0x3F]));
        } else {
            out.push('=');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{BUNDLE_SCHEMA_VERSION, crate_info};

    #[test]
    fn smoke_crate_info_reports_schema_v2() {
        assert_eq!(
            crate_info(),
            ("arb-core", BUNDLE_SCHEMA_VERSION),
            "crate_info must report arb-core with schema v2"
        );
    }
}
