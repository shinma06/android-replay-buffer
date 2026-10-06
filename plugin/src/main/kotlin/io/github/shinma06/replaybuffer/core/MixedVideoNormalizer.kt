package io.github.shinma06.replaybuffer.core

import org.jcodec.codecs.h264.H264Decoder
import org.jcodec.codecs.h264.H264Encoder
import org.jcodec.codecs.h264.H264Utils
import org.jcodec.codecs.h264.decode.SliceHeaderReader
import org.jcodec.codecs.h264.encode.DumbRateControl
import org.jcodec.codecs.h264.io.model.AspectRatio
import org.jcodec.codecs.h264.io.model.SeqParameterSet
import org.jcodec.codecs.h264.io.model.SliceType
import org.jcodec.codecs.h264.io.model.VUIParameters
import org.jcodec.common.io.BitReader
import org.jcodec.common.model.ColorSpace
import org.jcodec.common.model.Picture
import org.jcodec.common.model.Size
import java.nio.ByteBuffer

/** Only mixed geometry uses decode/pad/encode; no scaling or colour conversion. */
internal class MixedVideoNormalizer(private val configs: List<ByteArray>) {
    private val sources = configs.map { config ->
        require(config.size in 1..ReplaySettings.MAX_CONFIG_PACKET_BYTES) { "変換元configのサイズが不正です" }
        val sps = H264Utils.getRawSPS(ByteBuffer.wrap(config)).map { boundedSps(it) }
        val pps = H264Utils.getRawPPS(ByteBuffer.wrap(config)).map { boundedPps(it) }
        require(sps.isNotEmpty() && pps.isNotEmpty()) { "変換元SPS/PPSがありません" }
        sps.forEach { s ->
            require(s.profileIdc in setOf(66, 77, 100) && s.chromaFormatIdc == ColorSpace.YUV420J &&
                s.bitDepthLumaMinus8 == 0 && s.bitDepthChromaMinus8 == 0 && s.frameMbsOnlyFlag &&
                !s.mbAdaptiveFrameFieldFlag && !s.separateColourPlaneFlag && !s.qpprimeYZeroTransformBypassFlag &&
                !s.gapsInFrameNumValueAllowedFlag && s.picOrderCntType in 0..2 &&
                s.log2MaxFrameNumMinus4 in 0..12 && s.log2MaxPicOrderCntLsbMinus4 in 0..12 &&
                s.numRefFrames in 1..16 && s.seqParameterSetId in 0..31 && s.scalingMatrix == null &&
                s.numRefFramesInPicOrderCntCycle in 0..255) { "変換元H.264のprofile・feature・参照frameが非対応です (profile=${s.profileIdc}, chroma=${s.chromaFormatIdc}, refs=${s.numRefFrames}, frame=${s.frameMbsOnlyFlag}, poc=${s.picOrderCntType}, frameBits=${s.log2MaxFrameNumMinus4}, pocBits=${s.log2MaxPicOrderCntLsbMinus4}, matrix=${s.scalingMatrix?.size})" }
            require(s.picWidthInMbsMinus1 in 0..119 && s.picHeightInMapUnitsMinus1 in 0..119 &&
                s.frameCropLeftOffset >= 0 && s.frameCropRightOffset >= 0 &&
                s.frameCropTopOffset >= 0 && s.frameCropBottomOffset >= 0) { "変換元SPSのcoded寸法・cropが不正です" }
            val cw = (s.picWidthInMbsMinus1 + 1) * 16
            val ch = (s.picHeightInMapUnitsMinus1 + 1) * 16
            require((s.frameCropLeftOffset.toLong() + s.frameCropRightOffset) * 2 < cw &&
                (s.frameCropTopOffset.toLong() + s.frameCropBottomOffset) * 2 < ch) { "変換元SPSのcropが不正です" }
            val size = H264Utils.getPicSize(s)
            require(size.width in 2..1920 && size.height in 2..1920 && size.width % 2 == 0 && size.height % 2 == 0 &&
                cw.toLong() * ch / 256 * s.numRefFrames <= 184_320) { "変換元の寸法・DPBが上限を超えています" }
            s.vuiParams?.let { v ->
                require(!v.aspectRatioInfoPresentFlag || v.aspectRatio?.value == 1 ||
                    v.aspectRatio?.value == 255 && v.sarWidth in 1..65535 && v.sarWidth == v.sarHeight) { "変換元pixel aspectが非対応です" }
                require(!v.chromaLocInfoPresentFlag || v.chromaSampleLocTypeTopField == 0 && v.chromaSampleLocTypeBottomField == 0) { "変換元chroma位置が非対応です" }
                require(!v.videoSignalTypePresentFlag || v.videoFormat in 0..5) { "変換元video formatが不正です" }
                require(!v.colourDescriptionPresentFlag || v.videoSignalTypePresentFlag &&
                    v.colourPrimaries in setOf(1, 2, 4, 5, 6, 7, 8, 9, 10, 11, 12, 22) &&
                    v.transferCharacteristics in setOf(1, 2, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18) &&
                    v.matrixCoefficients in setOf(1, 2, 4, 5, 6, 7, 8, 9, 10)) { "変換元の色指定が非対応です" }
            }
        }
        require(sps.map { H264Utils.getPicSize(it) }.distinct().size == 1 && sps.map { colour(it.vuiParams) }.distinct().size == 1) { "config内の寸法・色指定が一致しません" }
        pps.forEach { p ->
            require(p.picParameterSetId in 0..255 && sps.any { it.seqParameterSetId == p.seqParameterSetId } &&
                p.numSliceGroupsMinus1 == 0 && !p.constrainedIntraPredFlag && !p.redundantPicCntPresentFlag && p.weightedBipredIdc in 0..2 &&
                p.numRefIdxActiveMinus1.size == 2 && p.numRefIdxActiveMinus1.all { it in 0..15 } &&
                p.picInitQpMinus26 in -26..25 && p.picInitQsMinus26 in -26..25 && p.chromaQpIndexOffset in -12..12 &&
                p.extended?.scalingMatrix == null) { "変換元PPSのfeatureが非対応です" }
        }
        sps.first()
    }
    val width = sources.maxOf { H264Utils.getPicSize(it).width }.let { (it + 15) and -16 }
    val height = sources.maxOf { H264Utils.getPicSize(it).height }.let { (it + 15) and -16 }
    val colourKnown = sources.all { colour(it.vuiParams).last() == true }
    private val canvas = Picture.create(width, height, ColorSpace.YUV420J)
    private val encoded = ByteBuffer.allocate(width * height * 3)
    private lateinit var decoder: H264Decoder
    private lateinit var encoder: H264Encoder
    private lateinit var source: SeqParameterSet
    private lateinit var config: ByteArray
    private var planes = emptyArray<ByteArray>()

    init {
        require(sources.map { colour(it.vuiParams) }.distinct().size == 1) { "サイズ変更前後の色指定が非互換です（色変換は非対応）" }
        require(width in 16..1920 && height in 16..1920 && width.toLong() * height / 256 * 30 <= 983_040) { "変換canvasがLevel 5.1上限を超えています" }
        require(sources.all { s ->
            (s.picWidthInMbsMinus1 + 1L) * 16 * (s.picHeightInMapUnitsMinus1 + 1) * 16 * (s.numRefFrames + 2) * 8 +
                width.toLong() * height * 32 <= 512L * 1024 * 1024
        }) { "変換のframe・参照buffer見積りが512MiBを超えています" }
    }

    fun startRun(bytes: ByteArray) {
        config = bytes
        source = sources[configs.indexOfFirst { it.contentEquals(bytes) }.also { require(it >= 0) }]
        decoder = serialDecoder().apply {
            // addSps/addPps clone their buffers in fixed 0.2.5; decodeFrame does not.
            addSps(H264Utils.getRawSPS(ByteBuffer.wrap(bytes)))
            addPps(H264Utils.getRawPPS(ByteBuffer.wrap(bytes)))
        }
        planes = Picture.create((source.picWidthInMbsMinus1 + 1) * 16,
            (source.picHeightInMapUnitsMinus1 + 1) * 16, ColorSpace.YUV420J).data
        encoder = object : H264Encoder(object : DumbRateControl() {
            override fun startPicture(size: Size, maxSize: Int, type: SliceType): Int {
                super.startPicture(size, maxSize, type)
                return 12
            }
        }) {
            override fun initSPS(size: Size): SeqParameterSet = super.initSPS(size).apply {
                levelIdc = 51
                // Preserve only picture semantics; source timing/HRD cannot describe a reencoded stream.
                vuiParams = source.vuiParams?.let { v -> VUIParameters().apply {
                    aspectRatioInfoPresentFlag = true
                    aspectRatio = AspectRatio.fromValue(1)
                    videoSignalTypePresentFlag = v.videoSignalTypePresentFlag
                    videoFormat = v.videoFormat
                    videoFullRangeFlag = v.videoFullRangeFlag
                    colourDescriptionPresentFlag = v.colourDescriptionPresentFlag
                    colourPrimaries = v.colourPrimaries
                    transferCharacteristics = v.transferCharacteristics
                    matrixCoefficients = v.matrixCoefficients
                    chromaLocInfoPresentFlag = v.chromaLocInfoPresentFlag
                    chromaSampleLocTypeTopField = v.chromaSampleLocTypeTopField
                    chromaSampleLocTypeBottomField = v.chromaSampleLocTypeBottomField
                } }
            }
        }.apply { setKeyInterval(30) }
    }

    fun encode(bytes: ByteArray): ByteBuffer {
        val nals = H264Utils.splitFrame(ByteBuffer.wrap(bytes))
        var slices = 0
        nals.forEach { nal ->
            val type = nal.get(0).toInt() and 31
            require(nal.remaining() >= 2 && nal.get(0).toInt() and 128 == 0 && type in setOf(1, 5, 6, 7, 8, 9, 12)) { "変換元NAL featureが非対応です" }
            if (type == 7 || type == 8) require(H264Utils.splitFrame(ByteBuffer.wrap(config)).any { it == nal }) { "packetのSPS/PPSが固定configと一致しません" }
            if (type == 1 || type == 5) {
                val slice = ByteBuffer.wrap(ByteArray(nal.remaining() - 1).also { nal.duplicate().apply { get() }.get(it) })
                H264Utils.unescapeNAL(slice)
                val header = SliceHeaderReader.readPart1(BitReader.createBitReader(slice))
                require(header.sliceType == SliceType.I || header.sliceType == SliceType.P) { "B/SP/SI-frame変換は非対応です" }
                require(header.firstMbInSlice in 0 until (planes[0].size / 256) &&
                    H264Utils.getRawPPS(ByteBuffer.wrap(config)).any { boundedPps(it).picParameterSetId == header.picParameterSetId }) { "変換元sliceの位置・PPSが不正です" }
                slices++
            }
        }
        require(slices > 0) { "変換元packetに映像sliceがありません" }
        val decoded = decoder.decodeFrame(ByteBuffer.wrap(bytes.copyOf()), planes).cloneCropped()
        val size = H264Utils.getPicSize(source)
        require(decoded.width == size.width && decoded.height == size.height && decoded.data.size == 3) { "復号frameの寸法・planeが一致しません" }
        canvas.fill(0)
        java.util.Arrays.fill(canvas.getPlaneData(0), ((if (source.vuiParams?.videoSignalTypePresentFlag == true && source.vuiParams.videoFullRangeFlag) 0 else 16) - 128).toByte())
        val x = ((width - size.width) / 2) and -2
        val y = ((height - size.height) / 2) and -2
        for (c in 0..2) {
            val shift = if (c == 0) 0 else 1
            val w = size.width shr shift
            val h = size.height shr shift
            require(decoded.getPlaneData(c).size >= w * h)
            for (row in 0 until h) System.arraycopy(decoded.getPlaneData(c), row * w, canvas.getPlaneData(c),
                (row + (y shr shift)) * (width shr shift) + (x shr shift), w)
        }
        encoded.clear()
        val result = encoder.encodeFrame(canvas, encoded).data
        require(result.remaining().toLong() * 8 * 30 <= 240_000_000) { "変換出力のbitrateがLevel 5.1上限を超えています" }
        H264Utils.getRawSPS(result.duplicate()).forEach { raw ->
            val s = boundedSps(raw)
            require(s.profileIdc == 66 && s.levelIdc == 51 && s.numRefFrames == 1 && s.frameMbsOnlyFlag &&
                H264Utils.getPicSize(s) == Size(width, height) && colour(s.vuiParams) == colour(source.vuiParams)) { "変換出力SPSの寸法・Level・色指定が一致しません" }
        }
        return result
    }

    fun rectangle(): Map<String, Int> {
        val size = H264Utils.getPicSize(source)
        val x = ((width - size.width) / 2) and -2
        val y = ((height - size.height) / 2) and -2
        return mapOf("x" to x, "y" to y, "width" to size.width, "height" to size.height,
            "padding_left" to x, "padding_top" to y, "padding_right" to width - x - size.width,
            "padding_bottom" to height - y - size.height)
    }

    private fun serialDecoder(): H264Decoder = H264Decoder().also { value ->
        // Fixed JCodec 0.2.5 exposes neither close nor serial mode. Disable its unused pool before any work,
        // keeping multislice decode on the cancellable save thread and preventing permanent daemon/decoder leaks.
        H264Decoder::class.java.getDeclaredField("threaded").apply { isAccessible = true }.setBoolean(value, false)
        val pool = H264Decoder::class.java.getDeclaredField("tp").apply { isAccessible = true }.get(value) as? java.util.concurrent.ExecutorService
        pool?.shutdown()
        check(pool == null || pool.isTerminated) { "動画decoderの未使用workerを終了できません" }
    }

    private fun colour(v: VUIParameters?): List<Any> = listOf(v?.videoSignalTypePresentFlag == true,
        if (v?.videoSignalTypePresentFlag == true) v.videoFormat else -1,
        v?.videoSignalTypePresentFlag == true && v.videoFullRangeFlag,
        v?.videoSignalTypePresentFlag == true && v.colourDescriptionPresentFlag,
        if (v?.colourDescriptionPresentFlag == true) v.colourPrimaries else -1,
        if (v?.colourDescriptionPresentFlag == true) v.transferCharacteristics else -1,
        if (v?.colourDescriptionPresentFlag == true) v.matrixCoefficients else -1,
        v?.chromaLocInfoPresentFlag == true,
        if (v?.chromaLocInfoPresentFlag == true) v.chromaSampleLocTypeTopField else 0,
        if (v?.chromaLocInfoPresentFlag == true) v.chromaSampleLocTypeBottomField else 0,
        v?.videoSignalTypePresentFlag == true && v.colourDescriptionPresentFlag && v.colourPrimaries != 2 &&
            v.transferCharacteristics != 2 && v.matrixCoefficients != 2)
}

/** JCodec allocates POC/HRD/FMO arrays from Exp-Golomb values before checking their sizes. */
internal fun boundedSps(raw: ByteBuffer): SeqParameterSet {
    val bytes = ByteBuffer.wrap(ByteArray(raw.remaining()).also { raw.duplicate().get(it) })
    H264Utils.unescapeNAL(bytes)
    val r = BitReader.createBitReader(bytes)
    fun bits(n: Int): Int { require(r.remaining() >= n) { "SPSが途中で終了しています" }; return r.readNBit(n) }
    fun ue(max: Int = Int.MAX_VALUE): Int {
        var zeros = 0
        while (bits(1) == 0) { zeros++; require(zeros <= 30) { "SPS整数が上限を超えています" } }
        return ((1L shl zeros) - 1 + bits(zeros)).also { require(it <= max) { "SPS配列・整数が上限を超えています" } }.toInt()
    }
    val profile = bits(8); bits(8); bits(8); ue(31)
    if (profile in setOf(100, 110, 122, 144)) {
        val chroma = ue(3)
        if (chroma == 3) bits(1)
        ue(6); ue(6); bits(1)
        if (bits(1) == 1) repeat(8) { n ->
            if (bits(1) == 1) {
                var last = 8; var next = 8
                repeat(if (n < 6) 16 else 64) {
                    if (next != 0) {
                        val value = ue(510)
                        val delta = if (value % 2 == 0) -(value / 2) else (value + 1) / 2
                        next = (last + delta + 256) % 256
                    }
                    if (next != 0) last = next
                }
            }
        }
    }
    ue(12)
    when (ue(2)) {
        0 -> ue(12)
        1 -> { bits(1); ue(); ue(); repeat(ue(255)) { ue() } }
    }
    ue(16); bits(1); ue(119); ue(119)
    if (bits(1) == 0) bits(1)
    bits(1)
    if (bits(1) == 1) repeat(4) { ue(1920) }
    if (bits(1) == 1) {
        if (bits(1) == 1 && bits(8) == 255) { bits(16); bits(16) }
        if (bits(1) == 1) bits(1)
        if (bits(1) == 1) { bits(3); bits(1); if (bits(1) == 1) repeat(3) { bits(8) } }
        if (bits(1) == 1) { ue(5); ue(5) }
        if (bits(1) == 1) { bits(32); bits(32); bits(1) }
        fun hrd() {
            val count = ue(31); bits(4); bits(4)
            repeat(count + 1) { ue(); ue(); bits(1) }
            repeat(4) { bits(5) }
        }
        val nal = bits(1) == 1; if (nal) hrd()
        val vcl = bits(1) == 1; if (vcl) hrd()
        if (nal || vcl) bits(1)
        bits(1)
        if (bits(1) == 1) { bits(1); repeat(6) { ue() } }
    }
    return H264Utils.readSPS(raw.duplicate())
}

internal fun boundedPps(raw: ByteBuffer): org.jcodec.codecs.h264.io.model.PictureParameterSet {
    val bytes = ByteBuffer.wrap(ByteArray(raw.remaining()).also { raw.duplicate().get(it) })
    H264Utils.unescapeNAL(bytes)
    val r = BitReader.createBitReader(bytes)
    fun ue(max: Int): Int {
        var zeros = 0
        while (true) {
            require(r.remaining() > 0) { "PPSが途中で終了しています" }
            if (r.read1Bit() != 0) break
            zeros++; require(zeros <= 30)
        }
        require(r.remaining() >= zeros)
        return ((1L shl zeros) - 1 + r.readNBit(zeros)).also { require(it <= max) { "PPS配列・整数が上限を超えています" } }.toInt()
    }
    ue(255); ue(31)
    require(r.remaining() >= 2); r.readNBit(2)
    // The capture path has no FMO. Reject before JCodec can allocate sliceGroupId/runLength arrays.
    require(ue(7) == 0) { "FMO/slice groupは非対応です" }
    return H264Utils.readPPS(raw.duplicate())
}
