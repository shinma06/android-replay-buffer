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
        eprintln!("Android Studio が録画中のため、デーモンの更新を延期しました。");
        return Ok(());
    }
    let response = client
        .call("system.shutdown_for_handover", json!({}))
        .map_err(CliError::from_rpc)?;
    if response["ok"] != true {
        eprintln!("デーモンの処理が完了するまで更新を延期しました。");
        return Ok(());
    }
    eprintln!("稼働中の arbd を更新します。");
    drop(client);
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match SingleInstanceLock::try_acquire(paths.lock_file()) {
            Ok(_guard) => break,
            Err(OsError::LockContended { .. }) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(err) => return Err(CliError::general(err.to_string())),
        }
    }

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

#[cfg(test)]
mod tests {
    use super::should_replace;

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
