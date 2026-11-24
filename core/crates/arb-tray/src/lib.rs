//! Library surface for `arb-tray`: the tray controller, menu/view-state model, and local IPC
//! JSON-RPC 2.0 client, wired exclusively to `arbd` over `arb_os::ipc` (`SPEC-01`, `SPEC-03`).
//!
//! This crate is split into a thin binary (`src/main.rs`, CLI parsing + native tray wiring) and
//! this library so integration tests (`tests/ipc_integration.rs`) can drive the real
//! [`arb_os::ipc::LocalIpcStream`] transport against a mock `arbd` server; the binary and any
//! future GUI/CLI frontends are the only other intended callers.

#![deny(unsafe_code)]

pub mod controller;
pub mod instance;
#[cfg(feature = "native-tray")]
pub mod native_tray;
pub mod rpc;
