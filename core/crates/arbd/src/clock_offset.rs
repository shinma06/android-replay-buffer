//! Device clock offset (`device CLOCK_REALTIME − host wall clock`) for `system.status`, new
//! bundle manifests and the read-time estimate of existing bundles (#591, SPEC-02).
//!
//! The offset is only disclosed: no stored or returned timestamp is ever corrected with it.
use arb_core::bundle::{HostClockAnchor, RequestedWindow};
use arb_core::time::{
    ClockOffsetUnavailable, DEFAULT_DISCONTINUITY_THRESHOLD_NS, DEFAULT_MAX_HALF_WIDTH_NS,
    DeviceClockOffset, DeviceClockTriple, DiscontinuityEvent, DiscontinuityKind, HostClockSample,
    PcSyncProbe, best_sync_probe_offset, estimate_device_clock_offset_at_trigger,
    parse_utc_iso8601_ms,
};
use arb_storage::bundle_io::{BundleManifestSummary, ClockAnchorRecord};

/// Splits saved anchors into triples and probes.
fn split(anchors: &[ClockAnchorRecord]) -> (Vec<DeviceClockTriple>, Vec<PcSyncProbe>) {
    let mut triples = Vec::new();
    let mut probes = Vec::new();
    for anchor in anchors {
        match anchor {
            ClockAnchorRecord::DeviceTriple(t) => triples.push(t.clone()),
            ClockAnchorRecord::PcSyncProbe(p) => probes.push(p.clone()),
            _ => {}
        }
    }
    (triples, probes)
}

/// A new manifest records both successes and failures so later reads reproduce its decision.
pub(crate) fn saved_device_clock_offset(
    anchors: &[ClockAnchorRecord],
    host: Option<HostClockAnchor>,
) -> DeviceClockOffset {
    let Some(host) = host else {
        return DeviceClockOffset::unavailable(ClockOffsetUnavailable::NoHostClock);
    };
    let (triples, probes) = split(anchors);
    let estimate =
        best_sync_probe_offset(&triples, &probes, &host.sample(), DEFAULT_MAX_HALF_WIDTH_NS);
    match estimate {
        Ok(e) => DeviceClockOffset::from_estimate(Some(e), ClockOffsetUnavailable::OutOfRange),
        Err(reason) => DeviceClockOffset::unavailable(reason),
    }
}

/// When a manifest has no saved decision, compute from its host anchor or estimate from
/// the trigger wall time, the window end boottime and the nearest saved triple of that boot
/// (`source: trigger_estimate`, at least one second of uncertainty).
pub(crate) fn estimate_bundle_clock_offset(
    summary: &BundleManifestSummary,
    anchors: &[ClockAnchorRecord],
    crashes: &[crate::protocol::OverviewCrash],
) -> DeviceClockOffset {
    let Some(manifest) = summary.manifest_v2.as_ref() else {
        return DeviceClockOffset::unavailable(ClockOffsetUnavailable::NoTriggerWallClock);
    };
    if let Some(anchor) = manifest.host_clock_anchor {
        return saved_device_clock_offset(anchors, Some(anchor));
    }
    let RequestedWindow::RealElapsedSec {
        trigger_pc_wall_utc,
        ..
    } = &manifest.requested_window
    else {
        return DeviceClockOffset::unavailable(ClockOffsetUnavailable::NoTriggerWallClock);
    };
    let (Some((wall_ms, precision_ms)), Some(epoch)) = (
        parse_utc_iso8601_ms(trigger_pc_wall_utc),
        manifest.epochs.last(),
    ) else {
        return DeviceClockOffset::unavailable(ClockOffsetUnavailable::NoTriggerWallClock);
    };
    let trigger_bt = epoch.end_boottime_ns;
    if crashes
        .iter()
        .any(|c| c.marker.boottime_ns == Some(trigger_bt))
    {
        return DeviceClockOffset::unavailable(ClockOffsetUnavailable::TriggerDelayUnknown);
    }
    let (triples, _) = split(anchors);
    let same_boot = triples.iter().filter(|t| t.boot_id == epoch.boot_id);
    let before = same_boot
        .clone()
        .filter(|t| t.boottime_ns <= trigger_bt)
        .max_by_key(|t| t.boottime_ns);
    let after = same_boot
        .filter(|t| t.boottime_ns >= trigger_bt)
        .min_by_key(|t| t.boottime_ns);
    if let (Some(a), Some(b)) = (before, after)
        && a.wall_offset_ns().abs_diff(b.wall_offset_ns())
            > u128::from(DEFAULT_DISCONTINUITY_THRESHOLD_NS)
    {
        return DeviceClockOffset::unavailable(ClockOffsetUnavailable::WallClockJump);
    }
    let Some(triple) = before
        .into_iter()
        .chain(after)
        .min_by_key(|t| t.boottime_ns.abs_diff(trigger_bt))
    else {
        return DeviceClockOffset::unavailable(ClockOffsetUnavailable::NoClockTriple);
    };
    DeviceClockOffset::from_estimate(
        estimate_device_clock_offset_at_trigger(triple, trigger_bt, wall_ms, precision_ms),
        ClockOffsetUnavailable::OutOfRange,
    )
}

/// Live clock evidence for `system.status`: the latest triple and the latest probe within the
/// error budget of the current boot.
#[derive(Debug, Clone, Default)]
pub(crate) struct LiveClockOffset {
    triple: Option<DeviceClockTriple>,
    probe: Option<PcSyncProbe>,
    /// The latest triple detected a device wall clock jump; one more triple must confirm the
    /// new offset before it is reported.
    wall_jump_unconfirmed: bool,
    wall_epoch_start_bt: Option<i64>,
}

impl LiveClockOffset {
    /// Forgets every sample (new capture generation, disconnect).
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    /// Records a heartbeat triple with the discontinuities its ingestion reported.
    pub(crate) fn observe_triple(
        &mut self,
        triple: DeviceClockTriple,
        discontinuities: &[DiscontinuityEvent],
    ) {
        if self.triple.as_ref().is_some_and(|old| {
            old.boot_id == triple.boot_id && old.boottime_ns >= triple.boottime_ns
        }) {
            return;
        }
        let wall_jump = discontinuities
            .iter()
            .any(|d| d.kind == DiscontinuityKind::DeviceWallJump);
        if self
            .probe
            .as_ref()
            .is_some_and(|p| p.boot_id != triple.boot_id)
        {
            self.probe = None;
        }
        if wall_jump {
            self.wall_epoch_start_bt = Some(triple.boottime_ns);
            self.probe = None;
        } else if self
            .triple
            .as_ref()
            .is_some_and(|old| old.boot_id != triple.boot_id)
        {
            self.wall_epoch_start_bt = None;
        }
        self.wall_jump_unconfirmed = wall_jump;
        self.triple = Some(triple);
    }

    /// Records a probe when its half-width is within the default error budget.
    pub(crate) fn observe_probe(&mut self, probe: &PcSyncProbe) {
        if self
            .wall_epoch_start_bt
            .is_none_or(|start| probe.dev_boottime_ns >= start)
            && probe
                .half_width_ns()
                .is_some_and(|hw| hw <= DEFAULT_MAX_HALF_WIDTH_NS)
        {
            self.probe = Some(probe.clone());
        }
    }

    /// The current offset of a connected device against `host` (`None` outside the live host
    /// clock domain). The caller reports `not_connected` without a device.
    pub(crate) fn offset(&self, host: Option<HostClockSample>) -> DeviceClockOffset {
        use ClockOffsetUnavailable as Why;
        let reason = if host.is_none() {
            Some(Why::ReplayedClock)
        } else if self.triple.is_none() {
            Some(Why::NoClockTriple)
        } else if self.wall_jump_unconfirmed {
            Some(Why::WallClockJump)
        } else if self.probe.is_none() {
            Some(Why::NoSyncProbe)
        } else {
            None
        };
        if let Some(reason) = reason {
            return DeviceClockOffset::unavailable(reason);
        }
        let (Some(triple), Some(probe), Some(host)) = (&self.triple, &self.probe, host) else {
            return DeviceClockOffset::unavailable(Why::NoSyncProbe);
        };
        if triple.boot_id != probe.boot_id {
            return DeviceClockOffset::unavailable(Why::BootMismatch);
        }
        DeviceClockOffset::from_estimate(
            arb_core::time::compute_device_clock_offset(triple, probe, &host),
            Why::OutOfRange,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{LiveClockOffset, estimate_bundle_clock_offset, saved_device_clock_offset};
    use arb_core::bundle::HostClockAnchor;
    use arb_core::time::{
        ClockOffsetSource, ClockOffsetUnavailable, DeviceClockTriple, DiscontinuityEvent,
        DiscontinuityKind, HostClockSample, PcSyncProbe,
    };
    use arb_storage::bundle_io::ClockAnchorRecord;

    const WALL_AT_CONT_0: i64 = 1_795_829_000_000_000_000;

    fn triple(boot: &str, bt: i64, ahead_ns: i64) -> DeviceClockTriple {
        // Device boottime = host continuous + 7 s.
        DeviceClockTriple {
            boot_id: boot.into(),
            boottime_ns: bt,
            monotonic_ns: bt,
            realtime_ns: WALL_AT_CONT_0 + bt - 7_000_000_000 + ahead_ns,
        }
    }

    fn probe(boot: &str, send: i64, rtt: i64) -> PcSyncProbe {
        PcSyncProbe {
            boot_id: boot.into(),
            pc_send_cont_ns: send,
            pc_send_active_ns: send,
            dev_boottime_ns: send + rtt / 2 + 7_000_000_000,
            dev_monotonic_ns: send + rtt / 2 + 7_000_000_000,
            pc_recv_cont_ns: send + rtt,
            pc_recv_active_ns: send + rtt,
        }
    }

    fn jump() -> DiscontinuityEvent {
        DiscontinuityEvent {
            kind: DiscontinuityKind::DeviceWallJump,
            stream: None,
            boot_id: Some("b".into()),
            start_boottime_ns: None,
            end_boottime_ns: None,
            pc_cont_start_ns: 0,
            pc_cont_end_ns: None,
            delta_ns: None,
            detail: None,
        }
    }

    fn host(cont: i64) -> HostClockSample {
        HostClockSample {
            continuous_mono_ns: cont,
            wall_epoch_ns: WALL_AT_CONT_0 + cont,
        }
    }

    #[test]
    fn live_offset_needs_every_sample_and_waits_out_a_wall_jump() {
        let mut live = LiveClockOffset::default();
        assert_eq!(
            live.offset(None).reason,
            Some(ClockOffsetUnavailable::ReplayedClock)
        );
        assert_eq!(
            live.offset(Some(host(1))).reason,
            Some(ClockOffsetUnavailable::NoClockTriple)
        );
        live.observe_triple(triple("b", 17_000_000_000, 1_700_000_000), &[]);
        assert_eq!(
            live.offset(Some(host(10_000_000_000))).reason,
            Some(ClockOffsetUnavailable::NoSyncProbe)
        );
        // A probe over the 50 ms half-width budget is ignored.
        live.observe_probe(&probe("b", 9_000_000_000, 200_000_000));
        assert_eq!(
            live.offset(Some(host(10_000_000_000))).reason,
            Some(ClockOffsetUnavailable::NoSyncProbe)
        );
        live.observe_probe(&probe("b", 9_900_000_000, 4_000_000));
        let offset = live.offset(Some(host(10_000_000_000)));
        assert_eq!(offset.offset_ms, Some(1_700));
        assert_eq!(offset.source, Some(ClockOffsetSource::SyncProbe));
        assert!(offset.uncertainty_ms.is_some_and(|u| u >= 4));
        // A jump is reported as uncertain until the next triple confirms it.
        live.observe_triple(triple("b", 17_300_000_000, 9_000_000_000), &[jump()]);
        let jumped = live.offset(Some(host(10_300_000_000)));
        assert_eq!(jumped.offset_ms, None);
        assert_eq!(jumped.reason, Some(ClockOffsetUnavailable::WallClockJump));
        live.observe_triple(triple("b", 17_600_000_000, 9_000_000_000), &[]);
        assert_eq!(live.offset(Some(host(10_600_000_000))).offset_ms, None);
        live.observe_probe(&probe("b", 10_500_000_000, 4_000_000));
        assert_eq!(
            live.offset(Some(host(10_600_000_000))).offset_ms,
            Some(9_000)
        );
        // A new boot drops the old probe.
        live.observe_triple(triple("c", 1_000_000_000, 0), &[]);
        assert_eq!(
            live.offset(Some(host(10_900_000_000))).reason,
            Some(ClockOffsetUnavailable::NoSyncProbe)
        );
        live.reset();
        assert_eq!(
            live.offset(Some(host(1))).reason,
            Some(ClockOffsetUnavailable::NoClockTriple)
        );
    }

    #[test]
    fn saved_offset_uses_the_host_anchor_only() {
        let anchors = vec![
            ClockAnchorRecord::DeviceTriple(triple("b", 17_000_000_000, -1_200_000_000)),
            ClockAnchorRecord::PcSyncProbe(probe("b", 9_900_000_000, 2_000_000)),
        ];
        let anchor = HostClockAnchor {
            trigger_pc_cont_ns: 12_000_000_000,
            trigger_wall_epoch_ns: WALL_AT_CONT_0 + 12_000_000_000,
        };
        assert_eq!(
            saved_device_clock_offset(&anchors, None).reason,
            Some(ClockOffsetUnavailable::NoHostClock)
        );
        let saved = saved_device_clock_offset(&anchors, Some(anchor));
        assert_eq!(saved.offset_ms, Some(-1_200));
        let no_probe = saved_device_clock_offset(&anchors[..1], Some(anchor));
        assert_eq!(no_probe.reason, Some(ClockOffsetUnavailable::NoSyncProbe));
    }

    #[test]
    fn bundle_offset_prefers_the_manifest_then_estimates_from_the_trigger() {
        // The manifest and trigger paths run end to end in `t2b_mcp_share_audio_e2e`; a legacy
        // bundle without a v2 manifest has no trigger wall time.
        let summary = arb_storage::bundle_io::BundleManifestSummary {
            bundle_id: "legacy".into(),
            format: arb_storage::bundle_io::BundleFormat::LegacySchema4,
            bundle_kind: arb_core::bundle::BundleKind::Full,
            created_at_utc: None,
            provenance: arb_core::bundle::Provenance::Original,
            device: None,
            streams: arb_core::bundle::StreamsSummary {
                video: None,
                audio: None,
                logcat: None,
                input: None,
            },
            unanchored_summary: arb_core::bundle::UnanchoredSummary {
                included_count: 0,
                truncated_count: 0,
            },
            epochs: Vec::new(),
            app_logs: Vec::new(),
            manifest_v2: None,
            legacy_metadata: None,
        };
        assert_eq!(
            estimate_bundle_clock_offset(&summary, &[], &[]).reason,
            Some(ClockOffsetUnavailable::NoTriggerWallClock)
        );
    }
}
