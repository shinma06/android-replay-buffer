package io.github.shinma06.replaybuffer.core

internal data class AutoTrigger(val eventId: String, val facts: Map<String, Any?>)

/**
 * Called inside the store/clock freeze before pin admission, using the output rows' exact mapper.
 *
 * mapper accepts wall/host ns; admitted is host monotonic ns. Unknown identity/time throws AutoSaveRejected.
 * Returned facts concern platform reports, not a measured process-death or physical-image timestamp.
 */
internal fun frozenAutoTrigger(pair: CrashPair, capture: FrozenCapture, mapper: (Long, Long) -> MappedTime,
                              admitted: Long): AutoTrigger {
    fun reject(reason: AutoSaveReason): Nothing = throw AutoSaveRejected(reason)
    val java = pair.java
    val platform = pair.platform
    val arm = java.barrier
    val target = capture.settings.application.packageName
    if (target == null || java.header.process?.let { selectedProcess(it, target) } != true ||
        java.targetRevision != platform.targetRevision) reject(AutoSaveReason.NOT_SAVED_TARGET_CHANGED)
    if (java.row.generation != capture.generation || platform.row.generation != capture.generation ||
        arm.generation != capture.generation) reject(AutoSaveReason.NOT_SAVED_STALE_OWNER)
    if (arm != platform.barrier || arm.epoch != capture.windowClockEpoch ||
        capture.clocks.lastOrNull()?.boot != arm.boot) reject(AutoSaveReason.NOT_SAVED_CLOCK_UNKNOWN)
    val jt = mapper(java.row.source.wall, java.row.host)
    val pt = mapper(platform.row.source.wall, platform.row.host)
    val bt = mapper(arm.wall, arm.host)
    if (listOf(jt, pt, bt).any { it.sequence == null || it.elapsed == null || it.uncertainty !in 0..20_000_000 ||
            it.epoch != arm.epoch }) reject(AutoSaveReason.NOT_SAVED_CLOCK_UNKNOWN)
    val v = capture.endUncertainty
    if (v !in 0..20_000_000) reject(AutoSaveReason.NOT_SAVED_CLOCK_UNKNOWN)
    try {
        val j = jt.sequence!!
        val t = pt.sequence!!
        val u = pt.uncertainty
        val lower = Math.subtractExact(t, u)
        val upper = Math.addExact(t, u)
        val jLower = Math.subtractExact(j, jt.uncertainty)
        val jUpper = Math.addExact(j, jt.uncertainty)
        val barrierUpper = Math.addExact(bt.sequence!!, bt.uncertainty)
        if (java.row.host <= arm.host || platform.row.host <= arm.host ||
            jLower <= barrierUpper || lower <= barrierUpper) reject(AutoSaveReason.NOT_SAVED_PRE_ARM_EVENT)
        if (Math.subtractExact(maxOf(upper, jUpper), minOf(lower, jLower)) > 5_000_000_000)
            reject(AutoSaveReason.UNKNOWN_UNCORROBORATED)
        val elapsedLow = Math.subtractExact(jt.elapsed!!, jt.uncertainty)
        val elapsedHigh = Math.addExact(jt.elapsed, jt.uncertainty)
        val membership = capture.apps.filter { period ->
            period.packageName == target && period.epoch == jt.epoch && period.uid == java.row.source.uid &&
                period.uidExclusive && !period.endUnknown && period.from != null && elapsedLow >= period.from &&
                (period.to == null || elapsedHigh <= period.to)
        }.singleOrNull()
        if (java.row.app != true || membership == null || identityOrder(java, platform) == null)
            reject(AutoSaveReason.UNKNOWN_IDENTITY)
        if (upper < Math.subtractExact(capture.start, v)) reject(AutoSaveReason.NOT_SAVED_EVENT_OUTSIDE_WINDOW)
        if (lower < Math.addExact(capture.start, v) || upper > Math.subtractExact(capture.end, v))
            reject(AutoSaveReason.NOT_SAVED_WINDOW_UNKNOWN)
        if (capture.logs.none { it.id == platform.row.id }) reject(AutoSaveReason.NOT_SAVED_EVIDENCE_LOST)
        val duration = capture.seconds * 1_000_000_000L
        val desired = Math.subtractExact(t, duration)
        fun evidence(observation: CrashObservation, time: MappedTime): Map<String, Any?> = mapOf(
            "record_id" to observation.row.id, "source_fingerprint" to observation.fingerprint,
            "wall_ns" to observation.row.source.wall.toString(), "receipt_host_ns" to observation.row.host.toString(),
            "decode_status" to observation.row.source.decodeStatus,
            "lid" to observation.row.source.lid, "uid" to observation.row.source.uid,
            "pid" to observation.row.source.pid, "tid" to observation.row.source.tid,
            "frozen_sequence_ns" to time.sequence.toString(), "frozen_elapsed_ns" to time.elapsed.toString(),
            "clock_epoch" to time.epoch, "uncertainty_ns" to time.uncertainty.toString(),
            "in_output" to capture.logs.any { it.id == observation.row.id },
        )
        return AutoTrigger(pair.id, mapOf(
            "contract_version" to 2, "event_id" to pair.id, "kind" to "platform_reported_uncaught_exception", "auto" to true,
            "option_revision" to java.optionRevision, "target_revision" to java.targetRevision,
            "arming_revision" to java.armingRevision, "arming_host_ns" to arm.host.toString(),
            "arming_sequence_ns" to bt.sequence.toString(), "boot" to arm.boot,
            "target_identity" to mapOf("package" to target, "process" to java.header.process, "uid" to membership.uid,
                "pid" to java.row.source.pid, "user" to java.row.source.uid!!.div(100000), "uid_exclusive" to true,
                "basis" to "kernel_sender_uid_pid_and_resolved_package_period_with_system_server_report"),
            "reporter_poll" to platform.reporter?.let { mapOf("pid" to it.pid, "uid" to 1000,
                "generation" to it.generation, "start_host_ns" to it.start.toString(), "end_host_ns" to it.end.toString()) },
            "raw_first_two_platform_integers" to listOf(platform.header.first, platform.header.second),
            "identity_interpretation" to identityOrder(java, platform), "recoverable" to 0,
            "java_evidence" to evidence(java, jt), "platform_evidence" to evidence(platform, pt),
            "recognized_host_ns" to pair.recognizedHost.toString(), "admitted_host_ns" to admitted.toString(),
            "admission_reason" to "corroborated_target_report_inside_frozen_window",
            "time_basis" to "platform_report_not_process_death", "desired_pre_report_start_ns" to desired.toString(),
            "desired_pre_report_end_ns" to t.toString(),
            "report_to_save_nominal_ns" to Math.subtractExact(capture.end, t).toString(),
            "pre_report_shortfall_nominal_ns" to maxOf(0, Math.subtractExact(capture.start, desired)).toString(),
            "pre_report_shortfall_min_ns" to maxOf(0, Math.subtractExact(Math.subtractExact(capture.start, v), Math.addExact(desired, u))).toString(),
            "pre_report_shortfall_max_ns" to maxOf(0, Math.subtractExact(Math.addExact(capture.start, v), Math.subtractExact(desired, u))).toString(),
        ))
    } catch (_: ArithmeticException) { reject(AutoSaveReason.NOT_SAVED_CLOCK_UNKNOWN) }
}
