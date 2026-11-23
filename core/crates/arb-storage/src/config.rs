//! Atomic persistence for user configuration (`<data_dir>/config.json`) and SHA-256 helper.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use arb_core::config::ArbConfig;
use arb_os::fs::{fsync_dir, fsync_file};
use sha2::{Digest, Sha256};

use crate::error::StorageError;
use crate::ring::hex_lower;

/// File name of the persisted user configuration inside `data_dir`.
pub const CONFIG_FILE_NAME: &str = "config.json";

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
        let temp_path = self
            .data_dir
            .join(format!(".{CONFIG_FILE_NAME}.tmp.{}", std::process::id()));
        let payload = serde_json::to_vec_pretty(config)
            .map_err(|source| StorageError::json("serialize_config_json", source))?;

        let write_result = (|| -> Result<(), StorageError> {
            let mut file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
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
    fn sha256_hex_matches_known_vector() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "empty sha256 must match standard test vector"
        );
    }
}
