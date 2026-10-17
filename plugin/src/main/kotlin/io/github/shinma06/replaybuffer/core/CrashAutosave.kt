package io.github.shinma06.replaybuffer.core

internal data class CrashBarrier(
    val device: String, val boot: String, val generation: Long, val epoch: Int,
    val host: Long, val wall: Long,
)

internal data class CrashObservation(
    val row: LogEntry, val header: CrashHeader, val fingerprint: String,
    val barrier: CrashBarrier, val optionRevision: Long, val targetRevision: Long, val armingRevision: Long,
    val reporter: ReporterPoll?,
) {
    val bytes: Int get() = row.source.raw.size + (header.process?.length ?: 0) * 2 + 256
}

internal data class CrashPair(val java: CrashObservation, val platform: CrashObservation, val recognizedHost: Long) {
    val id: String get() = platform.fingerprint
}

internal class AutoSaveRejected(val reason: AutoSaveReason) : Exception(reason.description)

/** A bounded reader mailbox, drained by the existing control tick; it never calls the store or a writer. */
internal class CrashAutosave {
    private var preference = false
    private var active = false
    private var target: String? = null
    private var optionRevision = 0L
    private var targetRevision = 0L
    private var armingRevision = 0L
    private var barrier: CrashBarrier? = null
    private val observations = mutableListOf<CrashObservation>()
    private data class Terminal(val outcome: AutoSaveOutcome, val sources: Set<String>)
    private val terminal = linkedMapOf<String, Terminal>()
    private var suspended: AutoSaveReason? = null
    private var warning: AutoSaveReason? = null
    private var last: AutoSaveOutcome? = null
    private var admissions = 0
    private var lastAdmission: Long? = null

    @Synchronized
    fun configure(preference: Boolean, active: Boolean, target: String?, revision: Long) {
        if (revision != optionRevision && preference == this.preference && target == this.target)
            invalidate(AutoSaveReason.NOT_SAVED_PRE_ARM_EVENT)
        optionRevision = revision
        if (preference != this.preference) {
            invalidate(AutoSaveReason.NOT_SAVED_DISABLED)
            if (preference) {
                terminal.clear()
                suspended = null
                warning = null
                last = null
                admissions = 0
                lastAdmission = null
                armingRevision++
            }
        }
        if (target != this.target) {
            invalidate(AutoSaveReason.NOT_SAVED_TARGET_CHANGED)
            targetRevision = revision
        }
        if (!active && this.active) invalidate(AutoSaveReason.NOT_SAVED_DISABLED)
        this.preference = preference
        this.active = active
        this.target = target
    }

    @Synchronized
    fun refresh(value: CrashBarrier?) {
        val old = barrier
        if (old != null && (value == null || old.generation != value.generation || old.device != value.device ||
                old.boot != value.boot || old.epoch != value.epoch)) {
            invalidate(if (value != null && old.generation != value.generation) AutoSaveReason.NOT_SAVED_STALE_OWNER
                else AutoSaveReason.NOT_SAVED_CLOCK_UNKNOWN)
        }
        if (preference && active && target != null && suspended == null && barrier == null) barrier = value
    }

    private fun invalidate(reason: AutoSaveReason) {
        observations.toList().forEach { finishObservation(it, reason) }
        observations.clear()
        barrier = null
    }

    @Synchronized
    fun observe(row: LogEntry, period: AppPeriod?, reporter: ReporterPoll?, selectedUid: Long?) {
        val arm = barrier ?: return
        val selected = target ?: return
        if (!preference || !active || suspended != null || row.generation != arm.generation) return
        val source = row.source
        // Definite other reporters cannot spend target capacity, even when their payload claims the target.
        if (source.lid == 2 && (source.uid != null && source.uid != 1000L ||
                reporter?.fresh(row.host, row.generation) == true && source.pid != reporter.pid)) return
        val header = crashHeader(source) ?: return
        if (header.process != null && !selectedProcess(header.process, selected)) return
        if (header.java && selectedUid != null && source.uid != null && source.uid != selectedUid) return
        if (header.process == null && (!header.java || period?.packageName != selected ||
                period.uidExclusive.not() || source.uid != period.uid)) return
        val fingerprint = crashFingerprint(arm.device, arm.boot, source)
        if (terminal.values.any { fingerprint in it.sources } || observations.any { it.fingerprint == fingerprint }) return
        if (terminal.size >= 256) { suspend(AutoSaveReason.LIMIT_REACHED); return }
        val problem = header.problem ?: when {
            row.host <= arm.host -> AutoSaveReason.NOT_SAVED_PRE_ARM_EVENT
            header.java && (row.app != true || period?.packageName != selected || period.uidExclusive.not() ||
                period.uid != source.uid) -> AutoSaveReason.UNKNOWN_IDENTITY
            !header.java && (source.uid != 1000L || reporter?.fresh(row.host, row.generation) != true ||
                source.pid != reporter.pid) -> AutoSaveReason.UNKNOWN_IDENTITY
            else -> null
        }
        if (problem != null) {
            record(AutoSaveOutcome(fingerprint, false, problem), setOf(fingerprint))
            return
        }
        val bytes = source.raw.size + (header.process?.length ?: 0) * 2 + 256
        if (observations.size >= 16 || observations.sumOf { it.bytes } + bytes > 64 * 1024) {
            suspend(AutoSaveReason.DETECTOR_OVERFLOW)
            return
        }
        observations += CrashObservation(row.copy(source = source.copy(raw = source.raw.copyOf(), tag = null, message = null)),
            header, fingerprint, arm, optionRevision, targetRevision, armingRevision, reporter)
    }

    @Synchronized
    fun drain(host: Long): List<CrashPair> {
        if (suspended != null || !active || !preference) return emptyList()
        val pairs = mutableListOf<CrashPair>()
        // Every already received row participates before an unmatched receipt deadline is applied.
        for (platform in observations.filter { !it.header.java }) {
            if (platform !in observations) continue
            val nearby = observations.filter { it.header.java && it.header.process == platform.header.process &&
                receiptPair(it.row.host, platform.row.host) }
            val matches = nearby.filter { identityOrder(it, platform) != null }
            if (matches.size == 1) {
                val java = matches.single()
                val competing = observations.filter { !it.header.java && it.header.process == java.header.process &&
                    receiptPair(java.row.host, it.row.host) && identityOrder(java, it) != null }
                val sources = (competing + java).map { it.fingerprint }.toSet()
                observations.removeAll { it.fingerprint in sources }
                if (competing.size != 1) {
                    record(AutoSaveOutcome(platform.fingerprint, true, AutoSaveReason.UNKNOWN_AMBIGUOUS), sources)
                } else if (record(AutoSaveOutcome(platform.fingerprint, true, AutoSaveReason.DETECTED), sources)) {
                    pairs += CrashPair(java, platform, host)
                }
            } else if (nearby.isNotEmpty()) {
                val sources = (nearby + platform).map { it.fingerprint }.toSet()
                observations.removeAll { it.fingerprint in sources }
                record(AutoSaveOutcome(platform.fingerprint, true, if (matches.size > 1)
                    AutoSaveReason.UNKNOWN_AMBIGUOUS else AutoSaveReason.UNKNOWN_IDENTITY), sources)
            }
        }
        observations.filter { host > it.row.host && host - it.row.host > 5_000_000_000 }.forEach {
            finishObservation(it, AutoSaveReason.UNKNOWN_UNCORROBORATED)
        }
        observations.removeAll { host > it.row.host && host - it.row.host > 5_000_000_000 }
        return pairs
    }

    @Synchronized
    fun admissionProblem(pair: CrashPair, host: Long): AutoSaveReason? = when {
        !active || !preference -> AutoSaveReason.NOT_SAVED_DISABLED
        pair.java.targetRevision != targetRevision -> AutoSaveReason.NOT_SAVED_TARGET_CHANGED
        pair.java.optionRevision != optionRevision || pair.platform.optionRevision != optionRevision ->
            AutoSaveReason.NOT_SAVED_PRE_ARM_EVENT
        barrier != pair.java.barrier || pair.platform.barrier != pair.java.barrier -> AutoSaveReason.NOT_SAVED_CLOCK_UNKNOWN
        suspended != null || admissions >= 8 -> AutoSaveReason.LIMIT_REACHED
        lastAdmission?.let { host < it || host - it < 30_000_000_000 } == true -> AutoSaveReason.NOT_SAVED_COOLDOWN
        else -> null
    }

    @Synchronized
    fun admitted(pair: CrashPair, requestId: String, host: Long) {
        admissions++
        lastAdmission = host
        outcome(pair.id, AutoSaveReason.ACCEPTED_WRITING, requestId)
        if (admissions == 8) suspend(AutoSaveReason.LIMIT_REACHED)
    }

    @Synchronized
    fun outcome(id: String, reason: AutoSaveReason, requestId: String? = null) {
        val previous = terminal[id] ?: return
        record(previous.outcome.copy(reason = reason, requestId = requestId ?: previous.outcome.requestId), previous.sources)
    }

    @Synchronized
    fun snapshot(): AutoSaveSnapshot {
        val state = when {
            !preference -> AutoSaveState.OFF
            suspended != null -> AutoSaveState.SUSPENDED
            !active || barrier == null -> AutoSaveState.WAITING
            else -> AutoSaveState.ARMED
        }
        return AutoSaveSnapshot(state, when (state) {
            AutoSaveState.OFF -> "自動保存は無効です"
            AutoSaveState.WAITING -> if (!active) "設定は有効です。取得をONにすると新しい報告から適用します" else "対象・現在の取得接続・時計の確認を待っています"
            AutoSaveState.ARMED -> "適用済み。標準Java/Kotlin報告を相互確認した場合だけ保存します"
            AutoSaveState.SUSPENDED -> suspended?.description
        }, 8 - admissions, terminal.values.count { isWarning(it.outcome.reason) }, last, warning?.description)
    }

    private fun finishObservation(value: CrashObservation, reason: AutoSaveReason) {
        record(AutoSaveOutcome(value.fingerprint, false, reason), setOf(value.fingerprint))
    }

    private fun record(outcome: AutoSaveOutcome, sources: Set<String>): Boolean {
        if (outcome.id !in terminal && terminal.size >= 256) { suspend(AutoSaveReason.LIMIT_REACHED); return false }
        terminal[outcome.id] = Terminal(outcome, sources)
        last = outcome
        if (isWarning(outcome.reason)) warning = outcome.reason
        return true
    }

    private fun suspend(reason: AutoSaveReason) { suspended = reason; warning = reason; observations.clear(); barrier = null }

    private fun isWarning(reason: AutoSaveReason): Boolean = reason !in setOf(
        AutoSaveReason.DETECTED, AutoSaveReason.ACCEPTED_WRITING, AutoSaveReason.SAVED,
    )
}

private fun receiptPair(a: Long, b: Long): Boolean = try {
    Math.subtractExact(maxOf(a, b), minOf(a, b)) <= 5_000_000_000
} catch (_: ArithmeticException) { false }

internal fun identityOrder(java: CrashObservation, platform: CrashObservation): String? {
    val pid = java.row.source.pid
    val user = java.row.source.uid?.div(100000) ?: return null
    val a = platform.header.first?.toLong()
    val b = platform.header.second?.toLong()
    return when {
        a == pid.toLong() && b == user && a == b -> "equivalent_pid_user_and_user_pid"
        a == pid.toLong() && b == user -> "pid_user"
        b == pid.toLong() && a == user -> "user_pid"
        else -> null
    }
}
