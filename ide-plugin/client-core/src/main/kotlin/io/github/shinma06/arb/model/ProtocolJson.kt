package io.github.shinma06.arb.model

import com.google.gson.FieldNamingPolicy
import com.google.gson.Gson
import com.google.gson.GsonBuilder
import com.google.gson.JsonDeserializationContext
import com.google.gson.JsonDeserializer
import com.google.gson.JsonElement
import com.google.gson.JsonNull
import com.google.gson.JsonObject
import com.google.gson.JsonParseException
import com.google.gson.JsonPrimitive
import com.google.gson.JsonSerializationContext
import com.google.gson.JsonSerializer
import java.lang.reflect.Type

/**
 * Configures Gson to match Rust `arbd` (`serde` + `serde_json`) wire representation bit-for-bit.
 */
object ProtocolJson {
    val gson: Gson = GsonBuilder()
        .setFieldNamingPolicy(FieldNamingPolicy.LOWER_CASE_WITH_UNDERSCORES)
        .disableHtmlEscaping()
        .registerWireEnum(IdeDefaultSaveAction::class.java, IdeDefaultSaveAction::wire, IdeDefaultSaveAction::fromWire)
        .registerWireEnum(CaptureMode::class.java, CaptureMode::wire, CaptureMode::fromWire)
        .registerWireEnum(SaveMode::class.java, SaveMode::wire, SaveMode::fromWire)
        .registerWireEnum(SaveTriggerKind::class.java, SaveTriggerKind::wire, SaveTriggerKind::fromWire)
        .registerWireEnum(BundleKind::class.java, BundleKind::wire, BundleKind::fromWire)
        .registerWireEnum(SaveJobState::class.java, SaveJobState::wire, SaveJobState::fromWire)
        .registerWireEnum(CatalogFormatVersion::class.java, CatalogFormatVersion::wire, CatalogFormatVersion::fromWire)
        .registerWireEnum(BundleLifecycleState::class.java, BundleLifecycleState::wire, BundleLifecycleState::fromWire)
        .registerWireEnum(BundleFormat::class.java, BundleFormat::wire, BundleFormat::fromWire)
        .registerWireEnum(LeaseHolderKind::class.java, LeaseHolderKind::wire, LeaseHolderKind::fromWire)
        .registerWireEnum(QueryBucket::class.java, QueryBucket::wire, QueryBucket::fromWire)
        .registerWireEnum(RecordBucket::class.java, RecordBucket::wire, RecordBucket::fromWire)
        .registerWireEnum(StreamKind::class.java, StreamKind::wire, StreamKind::fromWire)
        .registerWireEnum(StreamStatus::class.java, StreamStatus::wire, StreamStatus::fromWire)
        .registerWireEnum(TimestampSemantics::class.java, TimestampSemantics::wire, TimestampSemantics::fromWire)
        .registerWireEnum(AvOffsetStatus::class.java, AvOffsetStatus::wire, AvOffsetStatus::fromWire)
        .registerWireEnum(LogCompleteness::class.java, LogCompleteness::wire, LogCompleteness::fromWire)
        .registerWireEnum(DiscontinuityKind::class.java, DiscontinuityKind::wire, DiscontinuityKind::fromWire)
        .registerWireEnum(UncertaintyReason::class.java, UncertaintyReason::wire, UncertaintyReason::fromWire)
        .registerTypeAdapter(ArbConfigPatch::class.java, ArbConfigPatchAdapter)
        .registerTypeHierarchyAdapter(AdbDeviceState::class.java, AdbDeviceStateAdapter)
        .registerTypeHierarchyAdapter(Provenance::class.java, ProvenanceAdapter)
        .registerTypeHierarchyAdapter(TimeAlignment::class.java, TimeAlignmentAdapter)
        .create()

    private fun <T : Enum<T>> GsonBuilder.registerWireEnum(
        clazz: Class<T>,
        toWire: (T) -> String,
        fromWire: (String) -> T,
    ): GsonBuilder {
        val adapter = object : JsonSerializer<T>, JsonDeserializer<T> {
            override fun serialize(src: T, typeOfSrc: Type, context: JsonSerializationContext): JsonElement =
                JsonPrimitive(toWire(src))

            override fun deserialize(json: JsonElement, typeOfT: Type, context: JsonDeserializationContext): T {
                if (!json.isJsonPrimitive || !json.asJsonPrimitive.isString) {
                    throw JsonParseException("Expected string wire enum for ${clazz.simpleName}, got: $json")
                }
                return fromWire(json.asString)
            }
        }
        return registerTypeAdapter(clazz, adapter)
    }

    private object AdbDeviceStateAdapter : JsonSerializer<AdbDeviceState>, JsonDeserializer<AdbDeviceState> {
        override fun serialize(
            src: AdbDeviceState,
            typeOfSrc: Type,
            context: JsonSerializationContext,
        ): JsonElement = when (src) {
            is AdbDeviceState.Unknown -> JsonObject().apply { addProperty("unknown", src.value) }
            else -> JsonPrimitive(src.wireName)
        }

        override fun deserialize(
            json: JsonElement,
            typeOfT: Type,
            context: JsonDeserializationContext,
        ): AdbDeviceState {
            if (json.isJsonPrimitive && json.asJsonPrimitive.isString) {
                return when (val s = json.asString) {
                    "device" -> AdbDeviceState.Device
                    "offline" -> AdbDeviceState.Offline
                    "unauthorized" -> AdbDeviceState.Unauthorized
                    "recovery" -> AdbDeviceState.Recovery
                    "bootloader" -> AdbDeviceState.Bootloader
                    else -> AdbDeviceState.Unknown(s)
                }
            }
            if (json.isJsonObject) {
                val unknown = json.asJsonObject.get("unknown")
                if (unknown != null && unknown.isJsonPrimitive) {
                    return AdbDeviceState.Unknown(unknown.asString)
                }
            }
            throw JsonParseException("Unexpected AdbDeviceState JSON: $json")
        }
    }

    private object ProvenanceAdapter : JsonSerializer<Provenance>, JsonDeserializer<Provenance> {
        override fun serialize(
            src: Provenance,
            typeOfSrc: Type,
            context: JsonSerializationContext,
        ): JsonElement = when (src) {
            Provenance.Original -> JsonObject().apply { addProperty("kind", "original") }
            is Provenance.Trimmed -> JsonObject().apply {
                addProperty("kind", "trimmed")
                addProperty("parent_bundle_id", src.parentBundleId)
                addProperty("in_frame_index", src.inFrameIndex)
                addProperty("out_frame_index", src.outFrameIndex)
            }
        }

        override fun deserialize(
            json: JsonElement,
            typeOfT: Type,
            context: JsonDeserializationContext,
        ): Provenance {
            val obj = json.asJsonObject
            return when (val kind = obj.requireString("kind")) {
                "original" -> Provenance.Original
                "trimmed" -> Provenance.Trimmed(
                    parentBundleId = obj.requireString("parent_bundle_id"),
                    inFrameIndex = obj.requireLong("in_frame_index"),
                    outFrameIndex = obj.requireLong("out_frame_index"),
                )
                else -> throw JsonParseException("Unknown Provenance kind: $kind")
            }
        }
    }

    private object TimeAlignmentAdapter : JsonSerializer<TimeAlignment>, JsonDeserializer<TimeAlignment> {
        override fun serialize(
            src: TimeAlignment,
            typeOfSrc: Type,
            context: JsonSerializationContext,
        ): JsonElement = when (src) {
            is TimeAlignment.Anchored -> JsonObject().apply {
                addProperty("certainty", "anchored")
                addProperty("boot_id", src.bootId)
                addProperty("boottime_ns", src.boottimeNs)
                addProperty("half_width_ns", src.halfWidthNs)
                addProperty("semantics", src.semantics.wire)
            }
            is TimeAlignment.Bounded -> JsonObject().apply {
                addProperty("certainty", "bounded")
                addProperty("boot_id", src.bootId)
                addProperty("min_boottime_ns", src.minBoottimeNs)
                addProperty("max_boottime_ns", src.maxBoottimeNs)
                addProperty("reason", src.reason.wire)
            }
            is TimeAlignment.Unanchored -> JsonObject().apply {
                addProperty("certainty", "unanchored")
                addProperty("pc_ingest_cont_ns", src.pcIngestContNs)
                addProperty("reason", src.reason.wire)
            }
        }

        override fun deserialize(
            json: JsonElement,
            typeOfT: Type,
            context: JsonDeserializationContext,
        ): TimeAlignment {
            val obj = json.asJsonObject
            return when (val certainty = obj.requireString("certainty")) {
                "anchored" -> TimeAlignment.Anchored(
                    bootId = obj.requireString("boot_id"),
                    boottimeNs = obj.requireLong("boottime_ns"),
                    halfWidthNs = obj.requireLong("half_width_ns"),
                    semantics = TimestampSemantics.fromWire(obj.requireString("semantics")),
                )
                "bounded" -> TimeAlignment.Bounded(
                    bootId = obj.requireString("boot_id"),
                    minBoottimeNs = obj.requireLong("min_boottime_ns"),
                    maxBoottimeNs = obj.requireLong("max_boottime_ns"),
                    reason = UncertaintyReason.fromWire(obj.requireString("reason")),
                )
                "unanchored" -> TimeAlignment.Unanchored(
                    pcIngestContNs = obj.requireLong("pc_ingest_cont_ns"),
                    reason = UncertaintyReason.fromWire(obj.requireString("reason")),
                )
                else -> throw JsonParseException("Unknown TimeAlignment certainty: $certainty")
            }
        }
    }

    fun toConfigPatchJson(src: ArbConfigPatch): JsonObject = JsonObject().apply {
        src.saveWindowSecs?.let { addProperty("save_window_secs", it) }
        src.ringBudgetBytes?.let { addProperty("ring_budget_bytes", it) }
        src.minFreeDiskBytes?.let { addProperty("min_free_disk_bytes", it) }
        src.videoMaxSize?.let { addProperty("video_max_size", it) }
        src.videoMaxFps?.let { addProperty("video_max_fps", it) }
        src.audioEnabled?.let { addProperty("audio_enabled", it) }
        src.captureMode?.let { addProperty("capture_mode", it.wire) }
        when (val serial = src.deviceSerial) {
            StringPatch.Unchanged -> Unit
            StringPatch.Clear -> add("device_serial", JsonNull.INSTANCE)
            is StringPatch.Set -> addProperty("device_serial", serial.value)
        }
        src.crashAutosaveEnabled?.let { addProperty("crash_autosave_enabled", it) }
        src.crashAutosaveCooldownSecs?.let { addProperty("crash_autosave_cooldown_secs", it) }
        when (val dir = src.outputDirOverride) {
            OutputDirPatch.Unchanged -> Unit
            OutputDirPatch.Clear -> add("output_dir_override", JsonNull.INSTANCE)
            is OutputDirPatch.Set -> addProperty("output_dir_override", dir.path)
        }
        when (val dir = src.screenshotDirOverride) {
            OutputDirPatch.Unchanged -> Unit
            OutputDirPatch.Clear -> add("screenshot_dir_override", JsonNull.INSTANCE)
            is OutputDirPatch.Set -> addProperty("screenshot_dir_override", dir.path)
        }
        src.ideDefaultSaveAction?.let { addProperty("ide_default_save_action", it.wire) }
    }

    private object ArbConfigPatchAdapter : JsonSerializer<ArbConfigPatch> {
        override fun serialize(
            src: ArbConfigPatch,
            typeOfSrc: Type,
            context: JsonSerializationContext,
        ): JsonElement = toConfigPatchJson(src)
    }

    private fun JsonObject.requireString(key: String): String =
        get(key)?.asString ?: throw JsonParseException("Missing required string field '$key'")

    private fun JsonObject.requireLong(key: String): Long =
        get(key)?.asLong ?: throw JsonParseException("Missing required integer field '$key'")
}
