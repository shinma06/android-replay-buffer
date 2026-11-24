//! Standalone launchers use one IPC connection to identify and stop the same daemon.
use std::fs::File;
use std::time::{Duration, Instant};

use arb_os::{error::OsError, fs::SingleInstanceLock, paths::ArbPaths};
use semver::Version;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::{CliError, RpcClient, auto_start_timeout, rpc, wait_for_daemon};

fn should_replace(build_id: Option<&str>, local_sha: &str, local_version: &str) -> bool {
    let Some(id) = build_id else {
        // Earlier builds predate daemon fingerprints.
        return true;
    };
    let Some((running_version, digest)) = id.split_once(":sha256:") else {
        return false;
    };
    if digest.eq_ignore_ascii_case(local_sha) || digest.is_empty() {
        return false;
    }
    Version::parse(local_version)
        .and_then(|local| Version::parse(running_version).map(|old| local.cmp_precedence(&old)))
        .is_ok_and(std::cmp::Ordering::is_ge)
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
        &digest,
        env!("CARGO_PKG_VERSION"),
    ) {
        return Ok(());
    }
    eprintln!("[arb] Replacing the running arbd");
    client
        .call("system.shutdown", json!({}))
        .map_err(CliError::from_rpc)?;
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
                should_replace(Some(&old_id), "beef", new),
                expected,
                "{old} -> {new}"
            );
            assert!(!should_replace(Some(&old_id), "dead", new));
        }
        assert!(should_replace(None, "beef", "2.0.0"));
    }
}
