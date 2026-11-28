//! Save preflight, failure classification and durable one-shot maintenance.

use crate::{DaemonError, DaemonState, capture::phase2_materialize, time_util::next_id};
use arb_core::save_failure::{SaveFailureClass, SaveFailureIoKind, SaveFailureStage, classify};
use arb_storage::catalog::{CatalogSaveJob, parse_utc_timestamp_ns};
use std::{fs, path::Path};

pub(super) fn probe_output_root(dir: &Path) -> Result<(), DaemonError> {
    let result = (|| {
        fs::create_dir_all(dir)?;
        let probe = dir.join(next_id(".arb-probe"));
        let file = fs::File::create_new(&probe)?;
        drop(file);
        fs::remove_file(probe)
    })();
    result.map_err(|error| {
        let kind = arb_os::fs::save_failure_io_kind(&error);
        if classify(SaveFailureStage::Preflight, kind) == SaveFailureClass::PermanentDestination {
            DaemonError::OutputDirUnusable {
                dest_dir: dir.to_owned(),
                kind,
            }
        } else {
            error.into()
        }
    })
}

pub(super) fn failure_class(stage: SaveFailureStage, error: &DaemonError) -> SaveFailureClass {
    use arb_storage::StorageError;
    use std::error::Error;
    let mut cause: Option<&dyn Error> = Some(error);
    while let Some(err) = cause {
        if let Some(io) = err.downcast_ref::<std::io::Error>() {
            return classify(stage, arb_os::fs::save_failure_io_kind(io));
        }
        cause = err.source();
    }
    let kind = match error {
        DaemonError::SaveFailed(_) | DaemonError::Storage(StorageError::MissingSaveData(_)) => {
            SaveFailureIoKind::ChunkMissing
        }
        DaemonError::Storage(StorageError::Os(arb_os::OsError::AlreadyExists { .. })) => {
            SaveFailureIoKind::PrivateDirConflict
        }
        DaemonError::Json(_) | DaemonError::Storage(StorageError::CorruptData { .. }) => {
            SaveFailureIoKind::ChunkCorrupt
        }
        _ => SaveFailureIoKind::Other,
    };
    classify(stage, kind)
}

pub(super) fn eligible_ms(job: &CatalogSaveJob, after_secs: u64) -> Option<i64> {
    if after_secs == 0
        || job.rescue_attempted
        || !matches!(
            job.failure_class,
            Some(SaveFailureClass::Transient | SaveFailureClass::PermanentDestination)
        )
    {
        return None;
    }
    i64::try_from(
        parse_utc_timestamp_ns(&job.updated_at_utc)? / 1_000_000 + i128::from(after_secs) * 1000,
    )
    .ok()
}

impl DaemonState {
    pub(super) fn rescue_failed_jobs(&self, now_ms: i64) -> Result<(), DaemonError> {
        let after = self
            .lock(&self.user_config, "user config")?
            .failed_job_rescue_after_secs;
        if after == 0 {
            return Ok(());
        }
        for job in self.catalog.failed_save_jobs()? {
            if eligible_ms(&job, after).is_none_or(|at| at > now_ms) {
                continue;
            }
            let id = &job.job.job_id;
            let plan = {
                let cap = self.lock_capture()?;
                if !self.catalog.claim_rescue(id, &job.updated_at_utc)? {
                    continue;
                }
                cap.prepare_rescue(&self.catalog, &job)
            };
            // Each claim is durable, even if the destination is full or the daemon restarts.
            if let Ok(plan) = plan {
                phase2_materialize(&plan, &self.catalog, id)?;
            }
        }
        Ok(())
    }
}
