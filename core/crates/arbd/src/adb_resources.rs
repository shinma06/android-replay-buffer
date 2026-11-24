//! Durable ownership of device-side resources while ADB outlives the daemon.
use std::fs;
use std::io::{BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};

use crate::error::DaemonError;
use arb_adb::adb::{AdbClient, AdbCommandRunner, DeployedAgentPaths};
use arb_core::device::validate_adb_serial;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct SessionResources {
    pid: u32,
    serial: String,
    pub(super) deployed: DeployedAgentPaths,
    forwards: Vec<(u16, String)>,
}

impl SessionResources {
    pub(super) fn new(serial: &str, scrcpy: Option<&Path>) -> Self {
        let pid = std::process::id();
        let token = crate::time_util::next_id("s");
        Self {
            pid,
            serial: serial.into(),
            forwards: vec![],
            deployed: Self::paths(&token, scrcpy),
        }
    }

    fn paths(token: &str, scrcpy: Option<&Path>) -> DeployedAgentPaths {
        DeployedAgentPaths {
            companion_jar: format!("/data/local/tmp/arb-{token}-companion.dex.jar"),
            scrcpy_jar: scrcpy.map(|_| format!("/data/local/tmp/arb-{token}-scrcpy-audio.dex.jar")),
            session_token: token.into(),
        }
    }

    fn path(&self, dir: &Path) -> PathBuf {
        dir.join(format!("{}.json", self.deployed.session_token))
    }

    fn valid(&self) -> bool {
        let token = &self.deployed.session_token;
        let parts: Vec<_> = token.split('-').collect();
        parts.len() == 4
            && parts[0] == "s"
            && parts[1].parse::<u64>().is_ok()
            && parts[2].parse::<u32>() == Ok(self.pid)
            && parts[3].parse::<u64>().is_ok()
            && validate_adb_serial(&self.serial).is_ok()
            && self.deployed
                == Self::paths(token, self.deployed.scrcpy_jar.as_deref().map(Path::new))
            && self.forwards.iter().all(|(_, s)| {
                *s == format!("arb-companion-{token}")
                    || [0, 1]
                        .into_iter()
                        .any(|salt| *s == format!("scrcpy_{}", self.scid(salt)))
            })
    }

    pub(super) fn scid(&self, salt: u32) -> String {
        let hash = arb_storage::sha256_hex(self.deployed.session_token.as_bytes());
        super::live_adb::scrcpy_scid_hex(u64::from_str_radix(&hash[..8], 16).unwrap_or(0), salt)
    }

    pub(super) fn persist(&self, dir: &Path) -> Result<(), DaemonError> {
        fs::create_dir_all(dir)?;
        let dest = self.path(dir);
        let tmp = dest.with_extension("tmp");
        let mut out = fs::File::create(&tmp)?;
        out.write_all(&serde_json::to_vec(self)?)?;
        out.sync_all()?;
        drop(out);
        arb_os::fs::replace_file(&tmp, &dest)?;
        arb_os::fs::fsync_dir(dir)?;
        Ok(())
    }

    pub(super) fn forward(
        &mut self,
        dir: &Path,
        adb: &AdbClient,
        socket: &str,
    ) -> Result<u16, DaemonError> {
        let port = TcpListener::bind(("127.0.0.1", 0))?.local_addr()?.port();
        self.forwards.push((port, socket.into()));
        self.persist(dir)?;
        adb.create_owned_forward(&self.serial, port, socket)?;
        Ok(port)
    }

    pub(super) fn cleanup<R: AdbCommandRunner>(
        &self,
        dir: &Path,
        adb: &AdbClient<R>,
    ) -> Result<(), DaemonError> {
        adb.remove_owned_forwards(&self.serial, &self.forwards)?;
        if let Some(err) = adb
            .cleanup_session(&self.serial, &self.deployed, &[])
            .into_iter()
            .next()
        {
            return Err(err.into());
        }
        fs::remove_file(self.path(dir))?;
        Ok(())
    }
}

pub(super) fn reap(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "json")
            || !entry.file_type().is_ok_and(|kind| kind.is_file())
        {
            continue;
        }
        let Ok(file) = fs::File::open(&path) else {
            continue;
        };
        let Ok(rec) = serde_json::from_reader::<_, SessionResources>(BufReader::new(file)) else {
            continue;
        };
        if rec.valid()
            && rec.path(dir) == path
            && !arb_os::fs::is_pid_alive(rec.pid)
            && let Ok(adb) = AdbClient::discover()
        {
            let _ = rec.cleanup(dir, &adb);
        }
    }
}
