//! Atomic persistence for user configuration (`<data_dir>/config.json`) and SHA-256 helper.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use arb_core::config::ArbConfig;
use arb_os::fs::{fsync_dir, fsync_file};
use sha2::{Digest, Sha256};

use crate::error::StorageError;
use crate::ring::hex_lower;

/// File name of the persisted user configuration inside `data_dir`.
pub const CONFIG_FILE_NAME: &str = "config.json";

/// Process-wide counter making every temporary config file name unique.
static CONFIG_TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Serialises the write + rename section so concurrent savers in one process never race a
/// rename onto `config.json` (which can fail transiently on Windows).
static CONFIG_WRITE_LOCK: Mutex<()> = Mutex::new(());

/// Computes the lowercase 64-character hexadecimal SHA-256 digest of `bytes`.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex_lower(&hasher.finalize())
}

/// Crash-safe atomic store for `<data_dir>/config.json`.
///
/// Writes go through a sibling temporary file + `fsync_file` + atomic rename +
/// `fsync_dir`, so a crash or power loss never leaves a half-written `config.json`.
#[derive(Debug, Clone)]
pub struct ConfigStore {
    data_dir: PathBuf,
}

impl ConfigStore {
    /// Creates a store targeting `<data_dir>/config.json`.
    #[must_use]
    pub fn new(data_dir: impl AsRef<Path>) -> Self {
        Self {
            data_dir: data_dir.as_ref().to_path_buf(),
        }
    }

    /// Returns the full path to `config.json`.
    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.data_dir.join(CONFIG_FILE_NAME)
    }

    /// Loads and validates `config.json`, falling back to [`ArbConfig::default`] when absent.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Io`] if the file cannot be read, [`StorageError::Json`] if
    /// malformed, or [`StorageError::ConfigValidation`] if stored values fail validation.
    pub fn load_or_default(&self) -> Result<ArbConfig, StorageError> {
        let path = self.path();
        let bytes = match fs::read(&path) {
            Ok(data) => data,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(ArbConfig::default()),
            Err(source) => return Err(StorageError::io("read_config_json", &path, source)),
        };
        let config: ArbConfig = serde_json::from_slice(&bytes)
            .map_err(|source| StorageError::json("parse_config_json", source))?;
        config.validate()?;
        Ok(config)
    }

    /// Best-effort read of the stored `config_version` without full deserialization, so a file
    /// written by a *newer* build (unknown enum values, fields) is never mistaken for corruption.
    #[must_use]
    pub fn stored_config_version(&self) -> Option<u32> {
        let bytes = fs::read(self.path()).ok()?;
        let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        u32::try_from(value.get("config_version")?.as_u64()?).ok()
    }

    /// Moves an unreadable / invalid `config.json` aside to
    /// `config.json.invalid-<stamp>` (never deleting it) and returns the new path.
    ///
    /// `stamp` is sanitised to `[0-9A-Za-z]` so it is safe as a file name component on every OS.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Io`] if the rename fails.
    pub fn quarantine_invalid(&self, stamp: &str) -> Result<PathBuf, StorageError> {
        let clean: String = stamp.chars().filter(char::is_ascii_alphanumeric).collect();
        let source = self.path();
        // Never replace an earlier quarantined file (POSIX `rename` would silently overwrite).
        let mut target = self
            .data_dir
            .join(format!("{CONFIG_FILE_NAME}.invalid-{clean}"));
        let mut suffix = 1_u32;
        while fs::symlink_metadata(&target).is_ok() {
            target = self
                .data_dir
                .join(format!("{CONFIG_FILE_NAME}.invalid-{clean}-{suffix}"));
            suffix = suffix.saturating_add(1);
        }
        fs::rename(&source, &target)
            .map_err(|err| StorageError::io("quarantine_config_json", &source, err))?;
        Ok(target)
    }

    /// Validates `config` and persists it atomically to `<data_dir>/config.json`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::ConfigValidation`] if `config` is invalid, or
    /// [`StorageError::Io`] / [`StorageError::Os`] / [`StorageError::Json`] on write failure.
    pub fn save_atomic(&self, config: &ArbConfig) -> Result<(), StorageError> {
        config.validate()?;
        fs::create_dir_all(&self.data_dir)
            .map_err(|source| StorageError::io("create_config_dir", &self.data_dir, source))?;

        let final_path = self.path();
        // Unique per call (pid + sequence) and opened with `create_new`, so concurrent savers
        // can never share or truncate one another's temporary file.
        let seq = CONFIG_TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let temp_path = self.data_dir.join(format!(
            ".{CONFIG_FILE_NAME}.tmp.{}.{seq}",
            std::process::id()
        ));
        let payload = serde_json::to_vec_pretty(config)
            .map_err(|source| StorageError::json("serialize_config_json", source))?;

        let _write_guard = CONFIG_WRITE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let write_result = (|| -> Result<(), StorageError> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp_path)
                .map_err(|source| StorageError::io("create_config_tmp", &temp_path, source))?;
            file.write_all(&payload)
                .map_err(|source| StorageError::io("write_config_tmp", &temp_path, source))?;
            fsync_file(&file)?;
            drop(file);
            fs::rename(&temp_path, &final_path)
                .map_err(|source| StorageError::io("rename_config_json", &final_path, source))?;
            fsync_dir(&self.data_dir)?;
            Ok(())
        })();

        if write_result.is_err() {
            let _ = fs::remove_file(&temp_path);
        }
        write_result
    }
}

#[cfg(test)]
mod tests {
    use arb_core::config::{ArbConfig, IdeDefaultSaveAction};
    use tempfile::TempDir;

    use super::{ConfigStore, sha256_hex};
    use crate::error::StorageError;

    #[test]
    fn config_store_defaults_roundtrips_and_rejects_invalid_without_clobbering() {
        let tmp = TempDir::new().expect("tempdir");
        let store = ConfigStore::new(tmp.path());

        let initial = store
            .load_or_default()
            .expect("missing config.json must fall back to defaults");
        assert_eq!(initial, ArbConfig::default(), "must match default config");
        assert!(
            !store.path().exists(),
            "load_or_default must not create config.json"
        );

        let custom = ArbConfig {
            save_window_secs: 180,
            video_max_fps: 60,
            audio_enabled: false,
            crash_autosave_cooldown_secs: 30,
            ide_default_save_action: IdeDefaultSaveAction::SaveOnly,
            ..ArbConfig::default()
        };
        store.save_atomic(&custom).expect("save_atomic must work");
        assert!(store.path().is_file(), "config.json must exist on disk");

        let reloaded = store.load_or_default().expect("reload must succeed");
        assert_eq!(reloaded, custom, "persisted config must round-trip");

        // Invalid config must fail validation before touching disk.
        let bad = ArbConfig {
            save_window_secs: 1,
            ..custom.clone()
        };
        let err = store
            .save_atomic(&bad)
            .expect_err("invalid config must fail");
        assert!(
            matches!(err, StorageError::ConfigValidation(_)),
            "expected ConfigValidation, got {err:?}"
        );
        assert_eq!(
            store.load_or_default().expect("existing file intact"),
            custom,
            "failed save must preserve existing config.json"
        );
    }

    #[test]
    fn concurrent_save_atomic_never_corrupts_config_json() {
        let tmp = TempDir::new().expect("tempdir");
        let store = ConfigStore::new(tmp.path());
        let handles: Vec<_> = (0..8_u32)
            .map(|worker| {
                let store = store.clone();
                std::thread::spawn(move || {
                    for round in 0..25_u32 {
                        let cfg = ArbConfig {
                            // Alternate long and short payloads to expose tail corruption.
                            save_window_secs: 60 + worker * 10 + round % 2,
                            ide_default_save_action: if round % 2 == 0 {
                                IdeDefaultSaveAction::SaveOnly
                            } else {
                                IdeDefaultSaveAction::default()
                            },
                            ..ArbConfig::default()
                        };
                        store.save_atomic(&cfg).expect("concurrent save_atomic");
                        store
                            .load_or_default()
                            .expect("config.json must always parse while racing writers");
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().expect("writer thread");
        }
        store.load_or_default().expect("final config.json is valid");
        let leftovers: Vec<_> = std::fs::read_dir(tmp.path())
            .expect("read_dir")
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(
            leftovers.is_empty(),
            "no temp files may leak: {leftovers:?}"
        );
    }

    #[test]
    fn quarantine_invalid_moves_file_aside_and_defaults_return() {
        let tmp = TempDir::new().expect("tempdir");
        let store = ConfigStore::new(tmp.path());
        std::fs::write(store.path(), br#"{"config_version": 999}"#).expect("write bad config");
        assert!(
            store.load_or_default().is_err(),
            "newer version must fail to load"
        );
        let aside = store
            .quarantine_invalid("2026-11-23T00:00:00Z")
            .expect("quarantine");
        assert!(aside.is_file(), "original bytes preserved at {aside:?}");
        assert!(!store.path().exists(), "config.json moved away");
        assert_eq!(
            store.load_or_default().expect("defaults"),
            ArbConfig::default()
        );
    }

    #[test]
    fn sha256_hex_matches_known_vector() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "empty sha256 must match standard test vector"
        );
    }
}
