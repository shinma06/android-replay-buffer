package io.github.shinma06.arb.playback

import java.io.Closeable
import java.io.IOException
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.nio.channels.FileChannel
import java.nio.charset.StandardCharsets
import java.nio.file.Path
import java.nio.file.StandardOpenOption
import java.util.Arrays

/**
 * Signed 16-bit little-endian interleaved PCM format descriptor.
 */
data class PcmFormat(
    val sampleRateHz: Int = DEFAULT_SAMPLE_RATE_HZ,
    val channels: Int = DEFAULT_CHANNELS,
    val bitsPerSample: Int = BITS_PER_SAMPLE_S16LE,
) {
    init {
        require(sampleRateHz > 0) { "sampleRateHz must be positive: $sampleRateHz" }
        require(channels in 1..8) { "channels must be in 1..8: $channels" }
        require(bitsPerSample == BITS_PER_SAMPLE_S16LE) {
            "Only 16-bit signed little-endian PCM is supported, got $bitsPerSample-bit"
        }
    }

    val bytesPerSample: Int
        get() = bitsPerSample / 8

    val frameSizeBytes: Int
        get() = channels * bytesPerSample

    val bytesPerSecond: Long
        get() = sampleRateHz.toLong() * frameSizeBytes.toLong()

    /**
     * Overflow-safe conversion from non-negative nanoseconds to PCM sample frame index.
     */
    fun nsToSampleFrame(ns: Long): Long {
        if (ns <= 0L) return 0L
        val wholeSec = ns / NANOS_PER_SECOND
        val remNs = ns % NANOS_PER_SECOND
        val wholeSamples = if (wholeSec > Long.MAX_VALUE / sampleRateHz.toLong()) {
            return Long.MAX_VALUE
        } else {
            wholeSec * sampleRateHz.toLong()
        }
        val remSamples = (remNs * sampleRateHz.toLong()) / NANOS_PER_SECOND
        return if (wholeSamples > Long.MAX_VALUE - remSamples) Long.MAX_VALUE else wholeSamples + remSamples
    }

    /**
     * Overflow-safe conversion from non-negative PCM sample frame index to nanoseconds.
     */
    fun sampleFrameToNs(sampleFrame: Long): Long {
        if (sampleFrame <= 0L) return 0L
        val sr = sampleRateHz.toLong()
        val wholeSec = sampleFrame / sr
        val remFrames = sampleFrame % sr
        val wholeNs = if (wholeSec > Long.MAX_VALUE / NANOS_PER_SECOND) {
            return Long.MAX_VALUE
        } else {
            wholeSec * NANOS_PER_SECOND
        }
        val remNs = (remFrames * NANOS_PER_SECOND) / sr
        return if (wholeNs > Long.MAX_VALUE - remNs) Long.MAX_VALUE else wholeNs + remNs
    }

    companion object {
        const val DEFAULT_SAMPLE_RATE_HZ: Int = 48_000
        const val DEFAULT_CHANNELS: Int = 2
        const val BITS_PER_SAMPLE_S16LE: Int = 16
        const val NANOS_PER_SECOND: Long = 1_000_000_000L

        val DEFAULT: PcmFormat = PcmFormat(DEFAULT_SAMPLE_RATE_HZ, DEFAULT_CHANNELS, BITS_PER_SAMPLE_S16LE)
    }
}

/**
 * Read-only signed 16-bit little-endian interleaved PCM audio source.
 *
 * Designed so both in-memory test sources and `arbd`'s decoded WAV/PCM buffers
 * (`media.decode_audio_pcm`) plug directly into [AudioSyncPlayer].
 */
interface PcmAudioSource : Closeable {
    val sampleRateHz: Int
    val channels: Int
    val durationNs: Long

    val format: PcmFormat
        get() = PcmFormat(sampleRateHz = sampleRateHz, channels = channels)

    /**
     * Reads signed 16-bit little-endian interleaved PCM covering `[startNs, endNs)`.
     *
     * Returns an empty [ByteArray] when `endNs <= startNs` or the requested interval does not
     * intersect `[0, durationNs)`. Returned length is always an exact multiple of `channels * 2`.
     */
    fun readPcmSlice(startNs: Long, endNs: Long): ByteArray

    override fun close() {}
}

/**
 * In-memory [PcmAudioSource] backed by a raw signed 16-bit LE PCM byte array.
 */
class InMemoryPcmSource(
    override val sampleRateHz: Int,
    override val channels: Int,
    pcmBytes: ByteArray,
) : PcmAudioSource {
    override val format: PcmFormat = PcmFormat(sampleRateHz, channels)
    private val data: ByteArray
    private val totalSampleFrames: Long

    init {
        val alignedLen = (pcmBytes.size / format.frameSizeBytes) * format.frameSizeBytes
        data = pcmBytes.copyOf(alignedLen)
        totalSampleFrames = (alignedLen / format.frameSizeBytes).toLong()
    }

    override val durationNs: Long = format.sampleFrameToNs(totalSampleFrames)

    override fun readPcmSlice(startNs: Long, endNs: Long): ByteArray {
        if (endNs <= startNs || endNs <= 0L || totalSampleFrames == 0L) return ByteArray(0)
        val startFrame = format.nsToSampleFrame(startNs.coerceAtLeast(0L)).coerceAtMost(totalSampleFrames)
        val endFrame = format.nsToSampleFrame(endNs).coerceAtMost(totalSampleFrames)
        if (endFrame <= startFrame) return ByteArray(0)
        val startByte = (startFrame * format.frameSizeBytes.toLong()).toInt()
        val endByte = (endFrame * format.frameSizeBytes.toLong()).toInt()
        return data.copyOfRange(startByte, endByte)
    }
}

class WavFormatException(message: String) : IOException(message)

/**
 * Pure-Kotlin RIFF/WAV PCM (`s16le`) reader supporting both random-access local files and
 * in-memory WAV buffers with zero external dependencies.
 */
class WavFilePcmSource private constructor(
    override val format: PcmFormat,
    private val dataOffset: Long,
    private val dataSizeBytes: Long,
    private val channel: FileChannel?,
    private val inMemoryBytes: ByteArray?,
) : PcmAudioSource {
    override val sampleRateHz: Int = format.sampleRateHz
    override val channels: Int = format.channels

    private val totalSampleFrames: Long = dataSizeBytes / format.frameSizeBytes.toLong()
    override val durationNs: Long = format.sampleFrameToNs(totalSampleFrames)

    override fun readPcmSlice(startNs: Long, endNs: Long): ByteArray {
        if (endNs <= startNs || endNs <= 0L || totalSampleFrames == 0L) return ByteArray(0)
        val startFrame = format.nsToSampleFrame(startNs.coerceAtLeast(0L)).coerceAtMost(totalSampleFrames)
        val endFrame = format.nsToSampleFrame(endNs).coerceAtMost(totalSampleFrames)
        if (endFrame <= startFrame) return ByteArray(0)

        val byteOffsetLong = startFrame * format.frameSizeBytes.toLong()
        val byteLenLong = (endFrame - startFrame) * format.frameSizeBytes.toLong()
        if (byteLenLong <= 0L) return ByteArray(0)
        require(byteLenLong <= Int.MAX_VALUE.toLong()) { "Requested PCM slice exceeds Int.MAX_VALUE: $byteLenLong" }
        val byteLen = byteLenLong.toInt()

        if (inMemoryBytes != null) {
            val startIdx = (dataOffset + byteOffsetLong).toInt()
            return inMemoryBytes.copyOfRange(startIdx, startIdx + byteLen)
        }

        val ch = channel ?: return ByteArray(0)
        val out = ByteArray(byteLen)
        val buf = ByteBuffer.wrap(out)
        var pos = dataOffset + byteOffsetLong
        while (buf.hasRemaining()) {
            val n = ch.read(buf, pos)
            if (n < 0) break
            pos += n.toLong()
        }
        val readBytes = (buf.position() / format.frameSizeBytes) * format.frameSizeBytes
        return if (readBytes == out.size) out else out.copyOf(readBytes)
    }

    override fun close() {
        channel?.close()
    }

    companion object {
        private const val WAVE_FORMAT_PCM = 1
        private const val WAVE_FORMAT_EXTENSIBLE = 0xFFFE
        private const val RIFF_HEADER_BYTES = 12L
        private const val CHUNK_HEADER_BYTES = 8L

        // KSDATAFORMAT_SUBTYPE_PCM: 00000001-0000-0010-8000-00AA00389B71
        private val SUBTYPE_PCM_GUID: ByteArray = byteArrayOf(
            0x01, 0x00, 0x00, 0x00,
            0x00, 0x00,
            0x10, 0x00,
            0x80.toByte(), 0x00,
            0x00, 0xAA.toByte(), 0x00, 0x38, 0x9B.toByte(), 0x71,
        )

        /**
         * Opens a local RIFF/WAV file containing 16-bit signed LE PCM.
         */
        @Throws(IOException::class)
        fun open(path: Path): WavFilePcmSource {
            val ch = FileChannel.open(path, StandardOpenOption.READ)
            try {
                val fileSize = ch.size()
                val parsed = parseHeader(fileSize) { offset, len ->
                    val bytes = ByteArray(len)
                    val buf = ByteBuffer.wrap(bytes)
                    var pos = offset
                    while (buf.hasRemaining()) {
                        val n = ch.read(buf, pos)
                        if (n < 0) {
                            throw WavFormatException("Unexpected EOF while parsing WAV header at offset $pos")
                        }
                        pos += n.toLong()
                    }
                    bytes
                }
                return WavFilePcmSource(
                    format = parsed.format,
                    dataOffset = parsed.dataOffset,
                    dataSizeBytes = parsed.dataSizeBytes,
                    channel = ch,
                    inMemoryBytes = null,
                )
            } catch (t: Throwable) {
                runCatching { ch.close() }
                throw t
            }
        }

        /**
         * Parses an in-memory RIFF/WAV buffer containing 16-bit signed LE PCM.
         */
        @Throws(WavFormatException::class)
        fun fromBytes(wavBytes: ByteArray): WavFilePcmSource {
            val copy = wavBytes.copyOf()
            val parsed = parseHeader(copy.size.toLong()) { offset, len ->
                val start = offset.toInt()
                if (start < 0 || start + len > copy.size) {
                    throw WavFormatException("WAV buffer truncated at offset $offset (needed $len bytes)")
                }
                copy.copyOfRange(start, start + len)
            }
            return WavFilePcmSource(
                format = parsed.format,
                dataOffset = parsed.dataOffset,
                dataSizeBytes = parsed.dataSizeBytes,
                channel = null,
                inMemoryBytes = copy,
            )
        }

        /**
         * Encodes signed 16-bit little-endian interleaved PCM bytes into a standard RIFF/WAV buffer.
         */
        fun encodeWav(
            sampleRateHz: Int = PcmFormat.DEFAULT_SAMPLE_RATE_HZ,
            channels: Int = PcmFormat.DEFAULT_CHANNELS,
            pcmBytes: ByteArray,
        ): ByteArray {
            val fmt = PcmFormat(sampleRateHz, channels)
            val alignedDataLen = (pcmBytes.size / fmt.frameSizeBytes) * fmt.frameSizeBytes
            val riffChunkSize = 36 + alignedDataLen
            val out = ByteArray(44 + alignedDataLen)
            val bb = ByteBuffer.wrap(out).order(ByteOrder.LITTLE_ENDIAN)
            bb.put("RIFF".toByteArray(StandardCharsets.US_ASCII))
            bb.putInt(riffChunkSize)
            bb.put("WAVE".toByteArray(StandardCharsets.US_ASCII))
            bb.put("fmt ".toByteArray(StandardCharsets.US_ASCII))
            bb.putInt(16) // PCM fmt chunk size
            bb.putShort(WAVE_FORMAT_PCM.toShort())
            bb.putShort(channels.toShort())
            bb.putInt(sampleRateHz)
            bb.putInt(fmt.bytesPerSecond.toInt())
            bb.putShort(fmt.frameSizeBytes.toShort())
            bb.putShort(PcmFormat.BITS_PER_SAMPLE_S16LE.toShort())
            bb.put("data".toByteArray(StandardCharsets.US_ASCII))
            bb.putInt(alignedDataLen)
            System.arraycopy(pcmBytes, 0, out, 44, alignedDataLen)
            return out
        }

        private data class ParsedWav(
            val format: PcmFormat,
            val dataOffset: Long,
            val dataSizeBytes: Long,
        )

        private fun parseHeader(
            totalSize: Long,
            readAt: (offset: Long, len: Int) -> ByteArray,
        ): ParsedWav {
            if (totalSize < RIFF_HEADER_BYTES) {
                throw WavFormatException("WAV input too small ($totalSize bytes)")
            }
            val riffHeader = readAt(0L, 12)
            val riffId = String(riffHeader, 0, 4, StandardCharsets.US_ASCII)
            val waveId = String(riffHeader, 8, 4, StandardCharsets.US_ASCII)
            if (riffId != "RIFF" || waveId != "WAVE") {
                throw WavFormatException("Invalid RIFF/WAVE header: '$riffId' / '$waveId'")
            }

            var format: PcmFormat? = null
            var dataOffset: Long? = null
            var dataSizeBytes: Long? = null

            var cursor = RIFF_HEADER_BYTES
            while (cursor + CHUNK_HEADER_BYTES <= totalSize) {
                val hdr = readAt(cursor, 8)
                val chunkId = String(hdr, 0, 4, StandardCharsets.US_ASCII)
                val chunkSize = ByteBuffer.wrap(hdr, 4, 4).order(ByteOrder.LITTLE_ENDIAN).int.toLong() and 0xFFFF_FFFFL
                val payloadStart = cursor + CHUNK_HEADER_BYTES
                if (payloadStart > totalSize) {
                    throw WavFormatException("Corrupt WAV chunk '$chunkId' starts past EOF")
                }
                val available = totalSize - payloadStart

                when (chunkId) {
                    "fmt " -> {
                        if (chunkSize < 16L || chunkSize > available) {
                            throw WavFormatException("Invalid WAV 'fmt ' chunk size: $chunkSize")
                        }
                        if (chunkSize > 1024L) {
                            throw WavFormatException("Unsupported oversized WAV 'fmt ' chunk: $chunkSize")
                        }
                        val fmtBytes = readAt(payloadStart, chunkSize.toInt())
                        format = parseFmtChunk(fmtBytes)
                    }

                    "data" -> {
                        dataOffset = payloadStart
                        // Clamp to actual file length for robustness against streaming WAV headers.
                        dataSizeBytes = minOf(chunkSize, available)
                    }
                }

                if (format != null && dataOffset != null && dataSizeBytes != null) {
                    break
                }

                // RIFF chunks are word-aligned (odd chunk sizes have a 1-byte pad).
                val paddedSize = chunkSize + (chunkSize and 1L)
                val nextCursor = payloadStart + paddedSize
                if (nextCursor <= cursor) {
                    throw WavFormatException("WAV chunk size overflow at offset $cursor")
                }
                cursor = nextCursor
            }

            val resolvedFormat = format ?: throw WavFormatException("WAV missing 'fmt ' chunk")
            val resolvedDataOffset = dataOffset ?: throw WavFormatException("WAV missing 'data' chunk")
            val resolvedDataSize = dataSizeBytes ?: 0L
            val alignedDataSize =
                (resolvedDataSize / resolvedFormat.frameSizeBytes.toLong()) * resolvedFormat.frameSizeBytes.toLong()

            return ParsedWav(
                format = resolvedFormat,
                dataOffset = resolvedDataOffset,
                dataSizeBytes = alignedDataSize,
            )
        }

        private fun parseFmtChunk(fmtBytes: ByteArray): PcmFormat {
            val bb = ByteBuffer.wrap(fmtBytes).order(ByteOrder.LITTLE_ENDIAN)
            val audioFormat = bb.short.toInt() and 0xFFFF
            val channels = bb.short.toInt() and 0xFFFF
            val sampleRate = bb.int
            /* byteRate */ bb.int
            val blockAlign = bb.short.toInt() and 0xFFFF
            val bitsPerSample = bb.short.toInt() and 0xFFFF

            when (audioFormat) {
                WAVE_FORMAT_PCM -> Unit
                WAVE_FORMAT_EXTENSIBLE -> {
                    if (fmtBytes.size < 40) {
                        throw WavFormatException("WAVE_FORMAT_EXTENSIBLE 'fmt ' chunk too short (${fmtBytes.size} bytes)")
                    }
                    val subFormatGuid = fmtBytes.copyOfRange(24, 40)
                    if (!Arrays.equals(subFormatGuid, SUBTYPE_PCM_GUID)) {
                        throw WavFormatException("Unsupported WAVE_FORMAT_EXTENSIBLE subtype (expected PCM)")
                    }
                }
                else -> throw WavFormatException("Unsupported WAV audioFormat $audioFormat (only PCM=1 supported)")
            }

            if (sampleRate <= 0) {
                throw WavFormatException("Invalid WAV sample rate: $sampleRate")
            }
            if (channels !in 1..8) {
                throw WavFormatException("Unsupported WAV channel count: $channels")
            }
            if (bitsPerSample != PcmFormat.BITS_PER_SAMPLE_S16LE) {
                throw WavFormatException("Unsupported WAV bit depth $bitsPerSample (only 16-bit PCM supported)")
            }
            val expectedBlockAlign = channels * 2
            if (blockAlign != expectedBlockAlign) {
                throw WavFormatException("WAV blockAlign mismatch: got $blockAlign, expected $expectedBlockAlign")
            }
            return PcmFormat(sampleRateHz = sampleRate, channels = channels, bitsPerSample = bitsPerSample)
        }
    }
}
