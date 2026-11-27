package io.github.shinma06.arb.ide

import com.google.gson.JsonObject
import com.google.gson.JsonParser
import com.intellij.openapi.fileEditor.FileEditorManager
import com.intellij.openapi.fileTypes.FileType
import com.intellij.openapi.fileTypes.FileTypeRegistry
import com.intellij.openapi.project.ProjectManager
import com.intellij.openapi.ui.Messages
import com.intellij.openapi.util.io.ByteSequence
import com.intellij.openapi.vfs.VirtualFile
import com.intellij.openapi.vfs.VirtualFilePreCloseCheck
import io.github.shinma06.arb.ui.ArbIcons
import java.nio.charset.StandardCharsets
import java.nio.file.Files
import java.nio.file.Path
import javax.swing.Icon

/**
 * Pure, strict ARB bundle manifest detector used by [ArbBundleFileTypeDetector] and
 * [ArbBundleFileEditorProvider].
 *
 * Strictly inspects BOTH filename, sibling bundle files in the parent directory, AND ARB marker
 * fields in the JSON content so ordinary `package.json`, PWA/Android/Chrome `manifest.json`, or
 * unrelated `session.json` / `replay.json` files are NEVER hijacked!
 */
object ArbBundleDetector {
    private const val MAX_PREFIX_BYTES: Int = 8_192

    private val V2_SIBLING_FILES = setOf("timeline.sqlite", "video.mp4", "logs.jsonl")
    private val LEGACY_S4_SIBLING_FILES = setOf(
        "video.mp4",
        "video-frames.jsonl",
        "logcat-device.jsonl",
        "logcat-app.jsonl",
        "touches.jsonl",
    )
    private val LEGACY_S3_SIBLING_FILES = setOf(
        "video.mp4",
        "video-001.mp4",
        "frames.jsonl",
        "logcat.txt",
        "logcat.jsonl",
    )

    enum class DetectedKind(val isLegacy: Boolean) {
        V2_MANIFEST(isLegacy = false),
        LEGACY_SCHEMA4_REPLAY(isLegacy = true),
        LEGACY_SCHEMA3_SESSION(isLegacy = true),
    }

    /**
     * Detects whether a local filesystem file [path] is a valid ARB or Legacy bundle manifest.
     */
    fun detectPath(path: Path): DetectedKind? {
        if (!Files.isRegularFile(path)) return null
        val fileName = path.fileName?.toString() ?: return null
        val parent = path.parent ?: return null
        val siblings = listSiblingRegularFileNames(parent)
        val contentPrefix = readPrefixSafely(path) ?: return null
        return detectFromMetadata(fileName, siblings, contentPrefix)
    }

    /**
     * Detects whether an IntelliJ [VirtualFile] is a valid ARB or Legacy bundle manifest.
     */
    fun detectVirtualFile(file: VirtualFile, prefixChars: CharSequence? = null): DetectedKind? {
        if (file.isDirectory) return null
        val fileName = file.name
        if (fileName != "manifest.json" && fileName != "replay.json" && fileName != "session.json") {
            return null
        }
        val parent = file.parent ?: return null
        // Probe only the few marker siblings by name (no full directory listing during detection).
        val markerNames = when (fileName) {
            "manifest.json" -> V2_SIBLING_FILES
            "replay.json" -> LEGACY_S4_SIBLING_FILES
            else -> LEGACY_S3_SIBLING_FILES
        }
        val siblings = markerNames
            .filter { name -> parent.findChild(name)?.takeIf { !it.isDirectory } != null }
            .toSet()

        val text = prefixChars?.toString()
            ?: runCatching { String(file.contentsToByteArray(), StandardCharsets.UTF_8) }.getOrNull()
            ?: return null
        return detectFromMetadata(fileName, siblings, text)
    }

    /**
     * Core pure detection rule operating solely on filename, sibling file names, and JSON text.
     */
    fun detectFromMetadata(
        fileName: String,
        siblingFileNames: Set<String>,
        jsonText: String,
    ): DetectedKind? {
        val trimmed = jsonText.trimStart()
        if (!trimmed.startsWith("{")) return null

        return when (fileName) {
            "manifest.json" -> {
                if (siblingFileNames.none { it in V2_SIBLING_FILES }) return null
                if (hasV2ManifestMarkers(trimmed)) DetectedKind.V2_MANIFEST else null
            }

            "replay.json" -> {
                if (siblingFileNames.none { it in LEGACY_S4_SIBLING_FILES }) return null
                if (hasLegacySchema4Markers(trimmed)) DetectedKind.LEGACY_SCHEMA4_REPLAY else null
            }

            "session.json" -> {
                if (siblingFileNames.none { it in LEGACY_S3_SIBLING_FILES }) return null
                if (hasLegacySchema3Markers(trimmed)) DetectedKind.LEGACY_SCHEMA3_SESSION else null
            }

            else -> null
        }
    }

    private fun hasV2ManifestMarkers(jsonText: String): Boolean {
        val obj = parseJsonObjectOrNull(jsonText)
        if (obj != null) {
            val schemaVer = obj.get("schema_version")?.takeIf { it.isJsonPrimitive }?.asIntOrNull()
            val hasBundleId = obj.get("bundle_id")?.takeIf { it.isJsonPrimitive }?.asString?.isNotBlank() == true
            val hasArbStructure = obj.has("streams") || obj.has("provenance") || obj.has("epochs")
            return schemaVer == 2 && hasBundleId && hasArbStructure
        }
        // Prefix-truncated large manifest fallback: check mandatory ARB manifest top-level keys.
        return jsonText.contains("\"schema_version\"") &&
            Regex(""""schema_version"\s*:\s*2\b""").containsMatchIn(jsonText) &&
            jsonText.contains("\"bundle_id\"") &&
            (jsonText.contains("\"streams\"") || jsonText.contains("\"provenance\"") || jsonText.contains("\"epochs\""))
    }

    private fun hasLegacySchema4Markers(jsonText: String): Boolean {
        val obj = parseJsonObjectOrNull(jsonText)
        if (obj != null) {
            val schema = obj.get("schema")?.takeIf { it.isJsonPrimitive }?.asIntOrNull()
            val hasArbMarker = obj.has("capture_mode") ||
                obj.has("save_id") ||
                obj.get("plugin_id")?.takeIf { it.isJsonPrimitive }?.asString == "io.github.shinma06.android-replay-buffer"
            return schema == 4 && hasArbMarker
        }
        return Regex(""""schema"\s*:\s*4\b""").containsMatchIn(jsonText) &&
            (jsonText.contains("\"capture_mode\"") || jsonText.contains("\"save_id\"") || jsonText.contains("android-replay-buffer"))
    }

    private fun hasLegacySchema3Markers(jsonText: String): Boolean {
        val obj = parseJsonObjectOrNull(jsonText)
        if (obj != null) {
            val schema = obj.get("schema")?.takeIf { it.isJsonPrimitive }?.asIntOrNull()
                ?: obj.get("schema_version")?.takeIf { it.isJsonPrimitive }?.asIntOrNull()
            val hasArbMarker = obj.has("capture_mode") ||
                obj.has("replay_seconds") ||
                obj.has("save_id") ||
                obj.get("plugin_id")?.takeIf { it.isJsonPrimitive }?.asString == "io.github.shinma06.android-replay-buffer"
            return (schema == 3 || schema == 4) && hasArbMarker
        }
        return Regex(""""schema(?:_version)?"\s*:\s*[34]\b""").containsMatchIn(jsonText) &&
            (jsonText.contains("\"capture_mode\"") || jsonText.contains("\"replay_seconds\"") || jsonText.contains("\"save_id\""))
    }

    private fun parseJsonObjectOrNull(text: String): JsonObject? = runCatching {
        JsonParser.parseString(text).takeIf { it.isJsonObject }?.asJsonObject
    }.getOrNull()

    private fun com.google.gson.JsonElement.asIntOrNull(): Int? = runCatching { asInt }.getOrNull()

    private fun listSiblingRegularFileNames(parent: Path?): Set<String> {
        if (parent == null || !Files.isDirectory(parent)) return emptySet()
        return runCatching {
            Files.list(parent).use { stream ->
                stream.filter { Files.isRegularFile(it) }
                    .map { it.fileName.toString() }
                    .toList()
                    .toSet()
            }
        }.getOrDefault(emptySet())
    }

    private fun readPrefixSafely(path: Path): String? = runCatching {
        Files.newInputStream(path).use { input ->
            val buf = ByteArray(MAX_PREFIX_BYTES)
            var total = 0
            while (total < buf.size) {
                val n = input.read(buf, total, buf.size - total)
                if (n <= 0) break
                total += n
            }
            String(buf, 0, total, StandardCharsets.UTF_8)
        }
    }.getOrNull()
}

/**
 * Custom [FileType] identifying recognized ARB (`manifest.json`) and Legacy (`replay.json` /
 * `session.json`) bundle manifests so opening them launches the ARB Integrated Studio.
 */
object ArbBundleFileType : FileType {
    override fun getName(): String = "ArbBundleManifest"

    override fun getDisplayName(): String = "ARB バンドル"

    override fun getDescription(): String = "Android Replay Buffer バンドルマニフェスト"

    override fun getDefaultExtension(): String = ""

    override fun getIcon(): Icon = ArbIcons.Panel.swing

    override fun isBinary(): Boolean = false
}

class ArbBundleFileTypeDetector : FileTypeRegistry.FileTypeDetector {
    override fun getDesiredContentPrefixLength(): Int = 8_192

    override fun detect(
        file: VirtualFile,
        firstBytes: ByteSequence,
        firstCharsIfText: CharSequence?,
    ): FileType? {
        val text = firstCharsIfText
            ?: runCatching { String(firstBytes.toBytes(), StandardCharsets.UTF_8) }.getOrNull()
        return ArbBundleFileType.takeIf { ArbBundleDetector.detectVirtualFile(file, text) != null }
    }
}

/**
 * Politely asks for confirmation before closing an [ArbBundleFileEditor] tab if the user adjusted
 * a custom In/Out trim range without exporting it yet. Closes silently without nagging when the
 * range is untouched (full range / baseline) or already exported.
 */
class ArbBundlePreCloseCheck : VirtualFilePreCloseCheck {
    override fun canCloseFile(file: VirtualFile): Boolean {
        val openProjects = runCatching { ProjectManager.getInstance().openProjects.filterNot { it.isDisposed } }
            .getOrDefault(emptyList())
        for (project in openProjects) {
            val fem = project.getServiceIfCreated(FileEditorManager::class.java) ?: continue
            val editors = fem.getAllEditors(file).filterIsInstance<ArbBundleFileEditor>()
            for (editor in editors) {
                val saved = editor.flushBeforeClose()
                val unexported = editor.hasUnexportedTrimRange()
                val (inFrame, outFrame) = editor.currentTrimRange()
                val reason = buildString {
                    if (!saved) append("編集範囲（IN/OUT）を保存できませんでした。再度開くと以前の範囲に戻る場合があります。\n")
                    if (unexported) append("選択したトリム範囲（#$inFrame .. #$outFrame）はまだ書き出されていません。\n")
                    append("このままタブを閉じますか？")
                }
                val allowed = evaluatePreClose(unexported || !saved) {
                    Messages.showYesNoDialog(
                        project, reason, "トリム範囲の確認", "閉じる", "キャンセル", Messages.getQuestionIcon(),
                    ) == Messages.YES
                }
                if (!allowed) return false
            }
        }
        return true
    }

    companion object {
        /**
         * Pure decision helper: closes immediately when [hasUnexportedTrim] is `false`, otherwise
         * invokes [askUserToClose] and honors the user's choice.
         */
        fun evaluatePreClose(
            hasUnexportedTrim: Boolean,
            askUserToClose: () -> Boolean,
        ): Boolean = if (!hasUnexportedTrim) true else askUserToClose()
    }
}
