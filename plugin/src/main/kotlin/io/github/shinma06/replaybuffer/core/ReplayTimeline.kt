package io.github.shinma06.replaybuffer.core

/**
 * Movie-time presentation model of a saved replay for the editor player: the displayed frame spans,
 * the uncovered movie intervals (record gaps and empty edits), recorded touch positions and whether
 * record times are known. Read-only; derived from [SavedReplay].
 */
internal class ReplayTimeline(
    val durationUs: Long,
    /** Presented movie spans [startUs, endUs) in display order. */
    val spans: List<Span>,
    /** Movie intervals inside [0, durationUs) that no presented span covers. */
    val gaps: List<Span>,
    /** Movie positions of recorded touches (touches.jsonl movie_us). */
    val taps: List<Long>,
    /** False when record times are unknown (timebase.known == false). */
    val clockKnown: Boolean,
) {
    class Span(val startUs: Long, val endUs: Long)

    /** Index of the presented span shown at [us], or -1 inside a gap. */
    fun spanAt(us: Long): Int = spans.indexOfFirst { us in it.startUs until it.endUs }

    fun neighbour(index: Int, delta: Int): Int = (index + delta).coerceIn(0, spans.lastIndex)
}
