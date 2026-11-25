package io.github.shinma06.arb.state

/**
 * Starvation-free planner for the Library's per-bundle metadata fetches (`timeline.summary` +
 * `timeline.crashes`), driven by an explicit poll tick so it is fully deterministic.
 *
 * A fixed "first N candidates" budget lets N permanently failing bundles at the head of the list
 * consume every poll, so later bundles are never tried. This scheduler instead
 * - orders candidates by their last attempt (never tried first, then least recently tried), and
 * - puts a failed candidate on an exponentially growing cool-down ([backoffTicks]),
 *
 * so a failing prefix is skipped until the rest was served, and failures are still retried
 * (they never become "settled" on their own: only [markSuccess] ends a candidate's retries).
 */
class BundleMetaScheduler(
    private val budgetPerTick: Int,
    private val maxBackoffTicks: Int = DEFAULT_MAX_BACKOFF_TICKS,
) {
    init {
        require(budgetPerTick > 0) { "budgetPerTick must be positive" }
        require(maxBackoffTicks > 0) { "maxBackoffTicks must be positive" }
    }

    private class Slot {
        var lastAttemptTick: Long = NEVER
        var failures: Int = 0
        var nextEligibleTick: Long = 0L
    }

    private val slots = HashMap<String, Slot>()

    /** Drops bookkeeping for bundles that disappeared from the catalog. */
    @Synchronized
    fun retainOnly(ids: Set<String>) {
        slots.keys.retainAll(ids)
    }

    /**
     * Picks up to [budgetPerTick] of [candidates] (catalog order, only the ones that still need
     * data) that are not cooling down at [tick], least recently attempted first.
     */
    @Synchronized
    fun select(candidates: List<String>, tick: Long): List<String> =
        candidates
            .withIndex()
            .filter { (_, id) -> (slots[id]?.nextEligibleTick ?: 0L) <= tick }
            .sortedWith(
                compareBy({ slots[it.value]?.lastAttemptTick ?: NEVER }, { it.index }),
            )
            .take(budgetPerTick)
            .map { it.value }

    @Synchronized
    fun markSuccess(id: String) {
        slots.remove(id)
    }

    @Synchronized
    fun markFailure(id: String, tick: Long) {
        val slot = slots.getOrPut(id) { Slot() }
        slot.failures += 1
        slot.lastAttemptTick = tick
        slot.nextEligibleTick = tick + backoffTicks(slot.failures)
    }

    /** Test/diagnostic view: consecutive failures recorded for [id]. */
    @Synchronized
    fun failureCount(id: String): Int = slots[id]?.failures ?: 0

    private fun backoffTicks(failures: Int): Long {
        // 1st failure -> retry next tick+2, then 4, 8 ... capped at maxBackoffTicks.
        val shift = failures.coerceIn(1, 30)
        return (1L shl shift).coerceAtMost(maxBackoffTicks.toLong())
    }

    companion object {
        const val DEFAULT_MAX_BACKOFF_TICKS: Int = 16
        private const val NEVER: Long = Long.MIN_VALUE
    }
}
