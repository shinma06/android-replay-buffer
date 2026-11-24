//! Standard OS directory resolution with hermetic environment overrides.
//!
//! Supports `ARB_DATA_DIR`, `ARB_CACHE_DIR`, and `ARB_RUNTIME_DIR` overrides on
//! all platforms for hermetic testing, falling back to OS-native locations:
//! - macOS: `~/Library/Application Support/arb` and `~/Library/Caches/arb`
//! - Linux: XDG Base Directory paths (`$XDG_DATA_HOME/arb`, `$XDG_CACHE_HOME/arb`, `$XDG_RUNTIME_DIR/arb`)
//! - Windows: `%LOCALAPPDATA%\arb`

use std::ffi::OsString;
use std::path::PathBuf;

use crate::error::OsError;

/// Environment variable overriding [`ArbPaths::data_dir`].
pub const ENV_ARB_DATA_DIR: &str = "ARB_DATA_DIR";
/// Environment variable overriding [`ArbPaths::cache_dir`].
pub const ENV_ARB_CACHE_DIR: &str = "ARB_CACHE_DIR";
/// Environment variable overriding [`ArbPaths::runtime_dir`].
pub const ENV_ARB_RUNTIME_DIR: &str = "ARB_RUNTIME_DIR";

/// Resolved host directories for ARB persistent data, caches, and runtime files.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ArbPaths {
    /// Persistent data directory (saved bundles, SQLite catalog, configuration).
    pub data_dir: PathBuf,
    /// Ephemeral cache directory (live ring chunks, filmstrip caches).
    pub cache_dir: PathBuf,
    /// Runtime directory for IPC sockets, lock files, and shared-memory files.
    pub runtime_dir: PathBuf,
}

impl ArbPaths {
    /// Resolves standard ARB host directories using the process environment.
    ///
    /// # Errors
    ///
    /// Returns [`OsError::PathResolution`] if a required base environment
    /// variable (such as `HOME` on Unix or `LOCALAPPDATA`/`USERPROFILE` on
    /// Windows) is missing and not overridden via `ARB_*_DIR`.
    pub fn resolve() -> Result<Self, OsError> {
        Self::resolve_with_env(|key| std::env::var_os(key))
    }

    /// Resolves ARB host directories using a caller-supplied environment
    /// lookup function (useful for hermetic unit testing without mutating
    /// process-global environment variables).
    ///
    /// # Errors
    ///
    /// Returns [`OsError::PathResolution`] if a required base directory cannot
    /// be determined.
    pub fn resolve_with_env<F>(get_env: F) -> Result<Self, OsError>
    where
        F: Fn(&str) -> Option<OsString>,
    {
        imp::resolve_with_env(&get_env)
    }

    /// Returns the single-instance daemon lock file path (`<runtime_dir>/arbd.lock`).
    #[must_use]
    pub fn lock_file(&self) -> PathBuf {
        self.runtime_dir.join("arbd.lock")
    }

    /// Lock shared by all runtime directories that refer to this recording store.
    #[must_use]
    pub fn data_lock_file(&self) -> PathBuf {
        self.data_dir.join(".arbd-data.lock")
    }

    /// Returns the single-instance tray lock file path (`<runtime_dir>/arb-tray.lock`), held by
    /// a running `arb-tray` so repeated launches never create duplicate menu-bar icons.
    #[must_use]
    pub fn tray_lock_file(&self) -> PathBuf {
        self.runtime_dir.join("arb-tray.lock")
    }

    /// Returns the directory used for shared-memory frame ring backing files
    /// (`<runtime_dir>/shm`).
    #[must_use]
    pub fn shm_dir(&self) -> PathBuf {
        self.runtime_dir.join("shm")
    }

    /// Returns the platform-appropriate local IPC endpoint for `arbd`.
    ///
    /// - Unix: `<runtime_dir>/arbd.sock`
    /// - Windows: `\\.\pipe\arb-daemon-<hash>` derived deterministically from
    ///   `runtime_dir` so both default per-user directories and hermetic
    ///   `ARB_RUNTIME_DIR` overrides get isolated pipe names.
    #[must_use]
    pub fn ipc_endpoint(&self) -> PathBuf {
        imp::ipc_endpoint(&self.runtime_dir)
    }
}

fn non_empty_env(get_env: &dyn Fn(&str) -> Option<OsString>, key: &str) -> Option<PathBuf> {
    get_env(key)
        .filter(|val| !val.is_empty())
        .map(PathBuf::from)
}

/// Deterministic `\\.\pipe\arb-daemon-<fnv1a64>` pipe name for `runtime_dir`, shared
/// bit-for-bit with the Kotlin `ArbPaths.computeWindowsPipeEndpoint` client. Compiled in
/// test builds on every OS so cross-client golden-vector tests run on all CI platforms.
#[cfg(any(windows, test))]
fn windows_pipe_endpoint(runtime_dir: &std::path::Path, sid_bytes: Option<&[u8]>) -> PathBuf {
    // Stable 64-bit FNV-1a hash of the current user SID (when available) + runtime
    // directory gives per-user and per-hermetic-test Windows Named Pipe isolation,
    // mitigating local pipe squatting across user accounts.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    if let Some(sid) = sid_bytes.filter(|s| !s.is_empty()) {
        for byte in sid {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        hash ^= u64::from(b'|');
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    for byte in runtime_dir.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    PathBuf::from(format!(r"\\.\pipe\arb-daemon-{hash:016x}"))
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    use crate::error::OsError;
    use crate::paths::{
        ArbPaths, ENV_ARB_CACHE_DIR, ENV_ARB_DATA_DIR, ENV_ARB_RUNTIME_DIR, non_empty_env,
    };

    pub(super) fn resolve_with_env(
        get_env: &dyn Fn(&str) -> Option<OsString>,
    ) -> Result<ArbPaths, OsError> {
        let data_override = non_empty_env(get_env, ENV_ARB_DATA_DIR);
        let cache_override = non_empty_env(get_env, ENV_ARB_CACHE_DIR);
        let runtime_override = non_empty_env(get_env, ENV_ARB_RUNTIME_DIR);

        let resolve_home = || -> Result<PathBuf, OsError> {
            non_empty_env(get_env, "HOME").ok_or_else(|| OsError::PathResolution {
                reason: "HOME environment variable is unset or empty".to_owned(),
            })
        };

        let data_dir = match data_override {
            Some(dir) => dir,
            None => resolve_home()?
                .join("Library")
                .join("Application Support")
                .join("arb"),
        };
        let cache_dir = match cache_override {
            Some(dir) => dir,
            None => resolve_home()?.join("Library").join("Caches").join("arb"),
        };
        let runtime_dir = match runtime_override {
            Some(dir) => dir,
            None => resolve_home()?.join("Library").join("Caches").join("arb"),
        };

        Ok(ArbPaths {
            data_dir,
            cache_dir,
            runtime_dir,
        })
    }

    pub(super) fn ipc_endpoint(runtime_dir: &Path) -> PathBuf {
        runtime_dir.join("arbd.sock")
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod imp {
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    use crate::error::OsError;
    use crate::paths::{
        ArbPaths, ENV_ARB_CACHE_DIR, ENV_ARB_DATA_DIR, ENV_ARB_RUNTIME_DIR, non_empty_env,
    };

    pub(super) fn resolve_with_env(
        get_env: &dyn Fn(&str) -> Option<OsString>,
    ) -> Result<ArbPaths, OsError> {
        let data_dir = match non_empty_env(get_env, ENV_ARB_DATA_DIR) {
            Some(dir) => dir,
            None => match non_empty_env(get_env, "XDG_DATA_HOME") {
                Some(xdg) => xdg.join("arb"),
                None => require_home(get_env)?
                    .join(".local")
                    .join("share")
                    .join("arb"),
            },
        };

        let cache_dir = match non_empty_env(get_env, ENV_ARB_CACHE_DIR) {
            Some(dir) => dir,
            None => match non_empty_env(get_env, "XDG_CACHE_HOME") {
                Some(xdg) => xdg.join("arb"),
                None => require_home(get_env)?.join(".cache").join("arb"),
            },
        };

        let runtime_dir = match non_empty_env(get_env, ENV_ARB_RUNTIME_DIR) {
            Some(dir) => dir,
            None => match non_empty_env(get_env, "XDG_RUNTIME_DIR") {
                Some(xdg) => xdg.join("arb"),
                None => cache_dir.join("run"),
            },
        };

        Ok(ArbPaths {
            data_dir,
            cache_dir,
            runtime_dir,
        })
    }

    fn require_home(get_env: &dyn Fn(&str) -> Option<OsString>) -> Result<PathBuf, OsError> {
        non_empty_env(get_env, "HOME").ok_or_else(|| OsError::PathResolution {
            reason: "HOME environment variable is unset or empty".to_owned(),
        })
    }

    pub(super) fn ipc_endpoint(runtime_dir: &Path) -> PathBuf {
        runtime_dir.join("arbd.sock")
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    use crate::error::OsError;
    use crate::paths::{
        ArbPaths, ENV_ARB_CACHE_DIR, ENV_ARB_DATA_DIR, ENV_ARB_RUNTIME_DIR, non_empty_env,
    };

    pub(super) fn resolve_with_env(
        get_env: &dyn Fn(&str) -> Option<OsString>,
    ) -> Result<ArbPaths, OsError> {
        let data_override = non_empty_env(get_env, ENV_ARB_DATA_DIR);
        let cache_override = non_empty_env(get_env, ENV_ARB_CACHE_DIR);
        let runtime_override = non_empty_env(get_env, ENV_ARB_RUNTIME_DIR);

        let resolve_base = || -> Result<PathBuf, OsError> {
            let local_app_data = non_empty_env(get_env, "LOCALAPPDATA").or_else(|| {
                non_empty_env(get_env, "USERPROFILE").map(|home| home.join("AppData").join("Local"))
            });
            local_app_data
                .map(|dir| dir.join("arb"))
                .ok_or_else(|| OsError::PathResolution {
                    reason: "neither LOCALAPPDATA nor USERPROFILE is set".to_owned(),
                })
        };

        let data_dir = match data_override {
            Some(dir) => dir,
            None => resolve_base()?,
        };
        let cache_dir = match cache_override {
            Some(dir) => dir,
            None => resolve_base()?.join("cache"),
        };
        let runtime_dir = match runtime_override {
            Some(dir) => dir,
            None => resolve_base()?.join("runtime"),
        };

        Ok(ArbPaths {
            data_dir,
            cache_dir,
            runtime_dir,
        })
    }

    pub(super) fn ipc_endpoint(runtime_dir: &Path) -> PathBuf {
        crate::paths::windows_pipe_endpoint(runtime_dir, current_user_sid_bytes().as_deref())
    }

    fn current_user_sid_bytes() -> Option<Vec<u8>> {
        use std::ptr;

        use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
        use windows_sys::Win32::Security::{
            GetLengthSid, GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser,
        };
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

        struct Token(HANDLE);
        impl Drop for Token {
            fn drop(&mut self) {
                // SAFETY: this guard exclusively owns the handle returned by OpenProcessToken.
                unsafe { CloseHandle(self.0) };
            }
        }

        // SAFETY: `GetCurrentProcess` returns a pseudo-handle that does not need closing.
        let proc_handle = unsafe { GetCurrentProcess() };
        let mut token: HANDLE = ptr::null_mut();
        // SAFETY: `proc_handle` is the current process handle and `token` is stack storage.
        let ok = unsafe { OpenProcessToken(proc_handle, TOKEN_QUERY, &raw mut token) };
        if ok == 0 || token.is_null() {
            return None;
        }

        let _token_guard = Token(token);
        let mut needed: u32 = 0;
        // SAFETY: Querying required buffer length with a null output buffer is documented Win32 usage.
        let _ =
            unsafe { GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &raw mut needed) };
        let buf_len = usize::try_from(needed).ok()?;
        if buf_len < std::mem::size_of::<TOKEN_USER>() {
            return None;
        }

        let mut buf = vec![0_u8; buf_len];
        // SAFETY: `buf` is writable for `needed` bytes and `token` is valid.
        let ok = unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                buf.as_mut_ptr().cast(),
                needed,
                &raw mut needed,
            )
        };

        if ok == 0 {
            return None;
        }

        // SAFETY: `GetTokenInformation(TokenUser)` initialized a `TOKEN_USER` prefix in `buf`.
        let token_user = unsafe { ptr::read_unaligned(buf.as_ptr().cast::<TOKEN_USER>()) };
        let sid_ptr = token_user.User.Sid;
        if sid_ptr.is_null() {
            return None;
        }
        let offset = sid_ptr.addr().checked_sub(buf.as_ptr().addr())?;
        let tail = buf.get(offset..usize::try_from(needed).ok()?)?;
        // A SID has an 8-byte header followed by SubAuthorityCount DWORDs.
        if tail.len() < 8 || 8 + usize::from(tail[1]) * 4 > tail.len() {
            return None;
        }
        // SAFETY: `sid_ptr` points to a valid SID inside `buf`.
        let sid_len_u32 = unsafe { GetLengthSid(sid_ptr) };
        let sid_len = usize::try_from(sid_len_u32).ok()?;
        if sid_len > tail.len() {
            return None;
        }
        // SAFETY: `sid_ptr` points to `sid_len` readable bytes inside `buf`.
        let sid_slice = unsafe { std::slice::from_raw_parts(sid_ptr.cast::<u8>(), sid_len) };
        Some(sid_slice.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::ffi::OsString;
    use std::path::PathBuf;

    use super::{ArbPaths, ENV_ARB_CACHE_DIR, ENV_ARB_DATA_DIR, ENV_ARB_RUNTIME_DIR};

    fn lookup_from<'a>(
        map: &'a HashMap<&'a str, &'a str>,
    ) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |key| map.get(key).map(OsString::from)
    }

    #[test]
    fn env_overrides_take_precedence_over_os_defaults() {
        let env = HashMap::from([
            (ENV_ARB_DATA_DIR, "/tmp/arb-hermetic/data"),
            (ENV_ARB_CACHE_DIR, "/tmp/arb-hermetic/cache"),
            (ENV_ARB_RUNTIME_DIR, "/tmp/arb-hermetic/run"),
        ]);

        // Even with HOME/LOCALAPPDATA absent, explicit overrides succeed.
        let paths = ArbPaths::resolve_with_env(lookup_from(&env))
            .expect("explicit ARB_*_DIR overrides must succeed without HOME");
        assert_eq!(
            paths.data_dir,
            PathBuf::from("/tmp/arb-hermetic/data"),
            "data_dir must honor ARB_DATA_DIR"
        );
        assert_eq!(
            paths.cache_dir,
            PathBuf::from("/tmp/arb-hermetic/cache"),
            "cache_dir must honor ARB_CACHE_DIR"
        );
        assert_eq!(
            paths.runtime_dir,
            PathBuf::from("/tmp/arb-hermetic/run"),
            "runtime_dir must honor ARB_RUNTIME_DIR"
        );
        assert_eq!(
            paths.lock_file(),
            PathBuf::from("/tmp/arb-hermetic/run/arbd.lock"),
            "lock_file should live under runtime_dir"
        );
        assert_eq!(
            paths.tray_lock_file(),
            PathBuf::from("/tmp/arb-hermetic/run/arb-tray.lock"),
            "tray_lock_file should live under runtime_dir"
        );
        assert_eq!(
            paths.shm_dir(),
            PathBuf::from("/tmp/arb-hermetic/run/shm"),
            "shm_dir should live under runtime_dir"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_defaults_match_spec_paths() {
        let env = HashMap::from([("HOME", "/Users/tester"), (ENV_ARB_DATA_DIR, "")]);
        let paths = ArbPaths::resolve_with_env(lookup_from(&env))
            .expect("macOS path resolution with HOME must succeed");
        assert_eq!(
            paths.data_dir,
            PathBuf::from("/Users/tester/Library/Application Support/arb"),
            "macOS data_dir mismatch (empty override must fall back to default)"
        );
        assert_eq!(
            paths.cache_dir,
            PathBuf::from("/Users/tester/Library/Caches/arb"),
            "macOS cache_dir mismatch"
        );
        assert_eq!(
            paths.runtime_dir,
            PathBuf::from("/Users/tester/Library/Caches/arb"),
            "macOS runtime_dir mismatch"
        );
        assert_eq!(
            paths.ipc_endpoint(),
            PathBuf::from("/Users/tester/Library/Caches/arb/arbd.sock"),
            "macOS ipc_endpoint must match SPEC-03"
        );
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn linux_xdg_defaults_match_spec_paths() {
        let env = HashMap::from([
            ("HOME", "/home/tester"),
            ("XDG_DATA_HOME", "/xdg/data"),
            ("XDG_CACHE_HOME", "/xdg/cache"),
            ("XDG_RUNTIME_DIR", "/run/user/1000"),
        ]);
        let paths = ArbPaths::resolve_with_env(lookup_from(&env))
            .expect("Linux XDG path resolution must succeed");
        assert_eq!(
            paths.data_dir,
            PathBuf::from("/xdg/data/arb"),
            "Linux XDG data_dir mismatch"
        );
        assert_eq!(
            paths.cache_dir,
            PathBuf::from("/xdg/cache/arb"),
            "Linux XDG cache_dir mismatch"
        );
        assert_eq!(
            paths.runtime_dir,
            PathBuf::from("/run/user/1000/arb"),
            "Linux XDG runtime_dir mismatch"
        );
        assert_eq!(
            paths.ipc_endpoint(),
            PathBuf::from("/run/user/1000/arb/arbd.sock"),
            "Linux ipc_endpoint must match SPEC-03"
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_localappdata_defaults_match_spec_paths() {
        let env = HashMap::from([("LOCALAPPDATA", r"C:\Users\tester\AppData\Local")]);
        let paths = ArbPaths::resolve_with_env(lookup_from(&env))
            .expect("Windows path resolution must succeed");
        assert_eq!(
            paths.data_dir,
            PathBuf::from(r"C:\Users\tester\AppData\Local\arb"),
            "Windows data_dir mismatch"
        );
        assert!(
            paths
                .ipc_endpoint()
                .to_string_lossy()
                .starts_with(r"\\.\pipe\arb-daemon-"),
            "Windows ipc_endpoint must use local named pipe prefix"
        );
    }

    #[test]
    fn windows_pipe_endpoint_matches_kotlin_golden_vectors() {
        use super::windows_pipe_endpoint;

        // Golden vectors shared bit-for-bit with client-core `ArbPathsTest`
        // (`windowsPipeHashMatchesRustGoldenVectors`).
        let runtime = PathBuf::from(r"C:\Users\alice\AppData\Local\arb\runtime");
        assert_eq!(
            windows_pipe_endpoint(&runtime, None),
            PathBuf::from(r"\\.\pipe\arb-daemon-7fb9c25d31c01ee3"),
            "no-SID FNV-1a pipe name must match the Kotlin golden vector"
        );

        // Binary Win32 SID encoding of S-1-5-21-3623811015-3361044348-30300820-1013.
        let sid: [u8; 28] = [
            0x01, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05, 0x15, 0x00, 0x00, 0x00, 0xc7, 0xf7,
            0xfe, 0xd7, 0x7c, 0x77, 0x55, 0xc8, 0x94, 0x5a, 0xce, 0x01, 0xf5, 0x03, 0x00, 0x00,
        ];
        assert_eq!(
            windows_pipe_endpoint(&runtime, Some(&sid)),
            PathBuf::from(r"\\.\pipe\arb-daemon-5e550661a2714c87"),
            "SID-mixed FNV-1a pipe name must match the Kotlin golden vector"
        );
    }

    #[test]
    fn live_resolve_succeeds_in_host_environment() {
        let paths = ArbPaths::resolve().expect("ArbPaths::resolve must succeed on host");
        assert!(
            !paths.data_dir.as_os_str().is_empty(),
            "resolved data_dir must be non-empty"
        );
        assert!(
            !paths.cache_dir.as_os_str().is_empty(),
            "resolved cache_dir must be non-empty"
        );
        assert!(
            !paths.runtime_dir.as_os_str().is_empty(),
            "resolved runtime_dir must be non-empty"
        );
    }
}
