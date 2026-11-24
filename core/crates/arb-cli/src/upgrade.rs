//! Standalone launchers use one IPC connection to identify and stop the same daemon.
use std::fs::File;
use std::time::{Duration, Instant};

use arb_os::{error::OsError, fs::SingleInstanceLock, paths::ArbPaths};
use semver::Version;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::{CliError, RpcClient, auto_start_timeout, rpc, wait_for_daemon};

fn should_replace(
    build_id: Option<&str>,
    daemon_version: &str,
    local_sha: &str,
    local_version: &str,
) -> bool {
    let fingerprint = build_id.and_then(|id| id.split_once(":sha256:"));
    if fingerprint.is_some_and(|(_, sha)| sha.eq_ignore_ascii_case(local_sha)) {
        return false;
    }
    // Older v2 builds used size and timestamp. Only a strictly newer version
    // justifies replacement when the running binary has no content digest.
    let running_version = fingerprint.map_or(daemon_version, |(ver, _)| ver);
    let (Ok(local), Ok(old)) = (
        Version::parse(local_version),
        Version::parse(running_version),
    ) else {
        return false;
    };
    match local.cmp_precedence(&old) {
        std::cmp::Ordering::Equal => fingerprint.is_some_and(|(_, sha)| !sha.is_empty()),
        order => order.is_gt(),
    }
}

pub(super) fn prepare(paths: &ArbPaths, start: rpc::AutoStartMode) -> Result<(), CliError> {
    let endpoint = paths.ipc_endpoint();
    let Ok(mut client) = RpcClient::connect_endpoint(&endpoint) else {
        return Ok(());
    };
    let hello = client.verify_hello().map_err(CliError::from_rpc)?;
    let mut file =
        File::open(rpc::locate_arbd_binary()).map_err(|e| CliError::general(e.to_string()))?;
    let mut hash = Sha256::new();
    std::io::copy(&mut file, &mut hash).map_err(|e| CliError::general(e.to_string()))?;
    let digest = format!("{:x}", hash.finalize());
    if !should_replace(
        hello["build_id"].as_str(),
        hello["daemon_version"].as_str().unwrap_or_default(),
        &digest,
        env!("CARGO_PKG_VERSION"),
    ) {
        return Ok(());
    }
    if hello["parent_bound"] == true {
        eprintln!("Android Studio が管理中のため、デーモンの更新を延期しました。");
        return Ok(());
    }
    let response = match client.call("system.shutdown_for_handover", json!({})) {
        Err(rpc::RpcClientError::Rpc { code: -32601, .. }) => {
            eprintln!("稼働中のデーモンは安全な引き継ぎに未対応です。終了後に更新します。");
            return Ok(());
        }
        result => result.map_err(CliError::from_rpc)?,
    };
    if response["ok"] != true {
        eprintln!("デーモンの処理が完了するまで更新を延期しました。");
        return Ok(());
    }
    eprintln!("稼働中の arbd を更新します。");
    drop(client);
    wait_for_daemon_locks(paths, Duration::from_secs(15))?;

    if start == rpc::AutoStartMode::Enabled {
        let (mut child, log) = rpc::spawn_daemon_process(paths).map_err(CliError::from_rpc)?;
        let mut new = wait_for_daemon(
            &endpoint,
            auto_start_timeout(),
            Some(&mut child),
            Some(&log),
        )
        .map_err(CliError::from_rpc)?;
        let new_id = new.verify_hello().map_err(CliError::from_rpc)?;
        if !new_id["build_id"]
            .as_str()
            .is_some_and(|id| id.ends_with(&format!(":sha256:{digest}")))
        {
            return Err(CliError::general("別の arbd が先に起動しました"));
        }
    }
    Ok(())
}

fn wait_for_daemon_locks(paths: &ArbPaths, timeout: Duration) -> Result<(), CliError> {
    let deadline = Instant::now() + timeout;
    let mut runtime_lock = None;
    loop {
        if runtime_lock.is_none() {
            match SingleInstanceLock::try_acquire(paths.lock_file()) {
                Ok(guard) => runtime_lock = Some(guard),
                Err(OsError::LockContended { .. }) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
                Err(err) => return Err(CliError::general(err.to_string())),
            }
        }
        match SingleInstanceLock::try_acquire(paths.data_lock_file()) {
            Ok(_data_guard) => return Ok(()),
            Err(OsError::LockContended { .. }) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(err) => return Err(CliError::general(err.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::Duration;

    use arb_os::{fs::SingleInstanceLock, paths::ArbPaths};

    use super::{should_replace, wait_for_daemon_locks};

    #[test]
    fn wait_for_daemon_locks_waits_for_data_lock_and_allows_shared_dir() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let dir = tmp.path().join("shared");
        std::fs::create_dir_all(&dir).expect("create shared dir");
        let paths = ArbPaths {
            data_dir: dir.clone(),
            cache_dir: tmp.path().join("cache"),
            runtime_dir: dir,
        };
        let data_lock = SingleInstanceLock::try_acquire(paths.data_lock_file()).expect("lock data");
        assert!(
            wait_for_daemon_locks(&paths, Duration::from_millis(50)).is_err(),
            "must not succeed while .arbd-data.lock is still held"
        );
        let releaser = thread::spawn(move || {
            thread::sleep(Duration::from_millis(150));
            drop(data_lock);
        });
        wait_for_daemon_locks(&paths, Duration::from_secs(5)).expect(
            "must succeed once .arbd-data.lock is released even when runtime_dir == data_dir",
        );
        releaser.join().expect("join releaser");
    }

    #[test]
    fn launcher_upgrade_uses_semver_precedence_and_binary_identity() {
        for (old, new, expected) in [
            ("2.0.0-rc.2", "2.0.0-rc.10+build", true),
            ("2.0.0-rc.10", "2.0.0-rc.2", false),
            ("2.0.0-alpha.1", "2.0.0-rc.1", true),
            ("2.1.0-dev", "2.0.0-dev", false),
            ("2.0.0-dev", "2.0.0-dev", true),
            ("2.0.0", "2.0.0-dev", false),
            ("2.0.0-rc.1", "2.0.0", true),
            ("2.0.0+z", "2.0.0+a", true),
            ("", "2.0.0", false),
            ("2.0.0", "", false),
        ] {
            let old_id = format!("{old}:sha256:dead");
            assert_eq!(
                should_replace(Some(&old_id), old, "beef", new),
                expected,
                "{old} -> {new}"
            );
            assert!(!should_replace(Some(&old_id), old, "dead", new));
        }
        assert!(should_replace(
            Some("0.1.0-7756288-318178800"),
            "0.1.0",
            "beef",
            "2.0.0"
        ));
        assert!(should_replace(None, "0.1.0", "beef", "2.0.0"));
        assert!(!should_replace(None, "2.0.0", "beef", "2.0.0"));
    }
}
