package io.github.shinma06.replaybuffer.core

import com.google.gson.JsonElement
import com.google.gson.JsonObject

/** Host-observed packet order within a receiver session, not an encoder's sequence or start. */
internal data class AudioReceipt(val generation: Long, val session: Long, val ordinal: Long) {
    fun follows(previous: AudioReceipt) = generation == previous.generation && session == previous.session &&
        ordinal > previous.ordinal && ordinal - previous.ordinal == 1L
}

internal class AudioDecodeEvidence {
    private data class Range(val start: Int, var end: Int, val first: AudioReceipt, val leadingPreroll: Boolean) {
        fun receiptAt(index: Int) = first.copy(ordinal = first.ordinal + (index - start))
    }

    private val ranges = mutableListOf<Range>()

    fun add(index: Int, receipt: AudioReceipt?, leadingPreroll: Boolean = false) {
        if (receipt == null) return
        val previous = ranges.lastOrNull()
        if (previous != null && previous.end == index && !leadingPreroll && receipt.follows(previous.receiptAt(index - 1))) {
            previous.end++
        } else {
            ranges += Range(index, index + 1, receipt, leadingPreroll)
        }
    }

    fun receiptAt(index: Int): AudioReceipt? {
        val position = ranges.binarySearch {
            when {
                index < it.start -> 1
                index >= it.end -> -1
                else -> 0
            }
        }
        return ranges.getOrNull(position)?.receiptAt(index)
    }

    fun manifest(videoSha256: String?): Map<String, Any>? {
        if (videoSha256 == null || ranges.isEmpty()) return null
        return mapOf("video_sha256" to videoSha256, "ordinal_source" to "host_receive", "encoder_boundary" to "unknown",
            "ranges" to ranges.map { range -> mapOf(
                "packet_start" to range.start, "packet_end" to range.end,
                "generation" to range.first.generation.toString(), "receive_session" to range.first.session.toString(),
                "first_received_ordinal" to range.first.ordinal.toString(), "leading_preroll" to range.leadingPreroll,
            ) })
    }

    companion object {
        const val KEY = "decode_runs"

        fun read(element: JsonElement?, verifiedVideoSha256: String?, packets: Int,
                 checkActive: () -> Unit): AudioDecodeEvidence? {
            val record = element?.takeIf { it.isJsonObject }?.asJsonObject ?: return null
            if (verifiedVideoSha256 == null || record.string("video_sha256") != verifiedVideoSha256 ||
                record.string("ordinal_source") != "host_receive" || record.string("encoder_boundary") != "unknown") return null
            val source = record["ranges"]?.takeIf { it.isJsonArray }?.asJsonArray ?: return null
            if (source.isEmpty || source.size() > packets) return null
            val result = AudioDecodeEvidence()
            var end = 0
            for (elementRange in source) {
                checkActive()
                val range = elementRange.takeIf { it.isJsonObject }?.asJsonObject ?: return null
                val start = range.index("packet_start") ?: return null
                val finish = range.index("packet_end") ?: return null
                if (start < end || finish <= start || finish > packets) return null
                val generation = range.string("generation")?.toLongOrNull()?.takeIf { it >= 0 } ?: return null
                val session = range.string("receive_session")?.toLongOrNull()?.takeIf { it > 0 } ?: return null
                val ordinal = range.string("first_received_ordinal")?.toLongOrNull()
                    ?.takeIf { it in 0..Long.MAX_VALUE - (finish - start - 1) } ?: return null
                val preroll = range["leading_preroll"]?.takeIf { it.isJsonPrimitive && it.asJsonPrimitive.isBoolean }
                    ?.asBoolean ?: return null
                if (preroll && finish - start < 2) return null
                result.ranges += Range(start, finish, AudioReceipt(generation, session, ordinal), preroll)
                end = finish
            }
            return result
        }

        private fun JsonObject.string(name: String): String? =
            get(name)?.takeIf { it.isJsonPrimitive && it.asJsonPrimitive.isString }?.asString

        private fun JsonObject.index(name: String): Int? =
            get(name)?.takeIf { it.isJsonPrimitive && it.asJsonPrimitive.isNumber }?.asString?.toIntOrNull()
    }
}
