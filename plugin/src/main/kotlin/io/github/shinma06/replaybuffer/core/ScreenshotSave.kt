package io.github.shinma06.replaybuffer.core

import java.io.ByteArrayInputStream
import java.io.DataInputStream
import java.nio.ByteBuffer
import java.nio.file.Path
import java.time.LocalDateTime
import java.time.format.DateTimeFormatter
import java.util.UUID
import java.util.zip.CRC32
import javax.imageio.ImageIO
import javax.imageio.stream.MemoryCacheImageInputStream

internal class ScreenshotSaveFailure(message: String, val retained: Path? = null) : Exception(message)

/** Identity belongs to this completed image, never to the latest screenshot or a reusable pathname. */
internal class SavedScreenshot(
    val file: Path,
    val application: String,
    private val parentKey: Pair<Long, Long>,
    private val directoryKey: Pair<Long, Long>,
    private val fileKey: Pair<Long, Long>,
    private val hash: String,
) {
    private var deleted = false

    @Synchronized
    fun delete() {
        if (deleted) return
        SaveOutputDirectory.open(file.parent.parent).use { parent ->
            check(parent.key == parentKey) { "保存先が変更されています。ファイルは削除しません。" }
            parent.openChild(file.parent.fileName.toString()).use { directory ->
                check(directory.key == directoryKey) { "保存フォルダが変更されています。ファイルは削除しません。" }
                directory.requirePrivate()
                directory.openFile(file.fileName.toString()).use { original ->
                    check(original.key == fileKey && original.hash() == hash) { "保存後にファイルが変更されています。削除しません。" }
                    val quarantineName = ".delete-${UUID.randomUUID()}"
                    directory.createDirectory(quarantineName).use { quarantine ->
                        quarantine.requireEntries(emptyMap())
                        // Move first, then verify the moved object; a stale name must never delete its replacement.
                        directory.moveFile(file.fileName.toString(), quarantine, "image.png")
                        try {
                            quarantine.requireEntries(mapOf("image.png" to original))
                            check(original.hash() == hash) { "保存後にファイルが変更されています。削除しません。" }
                            quarantine.deleteFile("image.png", original)
                        } catch (failure: Exception) {
                            val restored = runCatching { quarantine.moveFile("image.png", directory, file.fileName.toString()) }.isSuccess
                            if (restored) runCatching { directory.deleteEmptyDirectory(quarantineName) }
                            throw ScreenshotSaveFailure("ファイルの変更または削除失敗を検出しました。保全した出力を確認してください。",
                                if (restored) file.parent else file.parent.resolve(quarantineName))
                        }
                        deleted = true
                        runCatching { quarantine.flush() }
                    }
                    // The PNG is already deleted; empty-directory housekeeping cannot change that outcome.
                    runCatching { directory.deleteEmptyDirectory(quarantineName); directory.flush() }
                }
            }
        }
    }
}

/** One finite USB request; does not create or access a replay capture, device file, forward, or server. */
internal fun saveScreenshot(adbPath: Path, application: String, destination: Path): SavedScreenshot {
    ApplicationTarget(application, ApplicationMode.MANUAL).validate()
    require(destination.isAbsolute) { "設定で保存先の絶対パスを指定してください。" }
    fun active() { if (Thread.currentThread().isInterrupted) throw InterruptedException("スクリーンショットを取り消しました。") }
    val bytes = OwnedAdb(adbPath).use { adb ->
        active()
        val serial = try { serialArgument(adb.command("-d", "get-serialno").trim()).also { check(it != "unknown") } }
        catch (_: Exception) { active(); throw ScreenshotSaveFailure("USB端末を1台だけ接続し、USBデバッグを許可してください。") }
        fun foreground(): String {
            active()
            return screenshotForeground(adb.command("-s", serial, "shell", "dumpsys", "window", limit = 4 * 1024 * 1024), application)
        }
        val display = screenshotDisplay(adb.command("-s", serial, "shell", "dumpsys", "SurfaceFlinger", "--display-id"))
        val before = foreground()
        active()
        val png = adb.commandBytes("-s", serial, "exec-out", "screencap", "-p", "-d", display, limit = 64 * 1024 * 1024, timeoutSeconds = 15)
        check(before == foreground()) { "撮影中に表示対象が変わりました。対象アプリを表示して再度保存してください。" }
        png
    }
    active()
    validateScreenshotPng(bytes)
    val root = try {
        destination.toRealPath()
    } catch (_: Exception) {
        throw ScreenshotSaveFailure("保存先フォルダを確認できません。設定の保存先・アクセス権を確認してください。")
    }
    val name = "screenshot-${LocalDateTime.now().format(DateTimeFormatter.ofPattern("yyyyMMdd-HHmmss-SSS"))}-${UUID.randomUUID()}"
    val partial = root.resolve(".$name.partial")
    val complete = root.resolve(name)
    var retained: Path? = null
    try {
        SaveOutputDirectory.open(root).use { parent ->
            active()
            parent.createDirectory(partial.fileName.toString()).use { staging ->
                retained = partial
                staging.requireEntries(emptyMap())
                staging.createFile("screen.png").use { output ->
                    output.channel().use { channel ->
                        val data = ByteBuffer.wrap(bytes)
                        while (data.hasRemaining()) { active(); channel.write(data) }
                        channel.force(true)
                    }
                    staging.flush()
                    active()
                    val publication = SavePublication(partial, complete, parent, staging) {
                        staging.requireEntries(mapOf("screen.png" to output))
                    }
                    try {
                        publication.publish()
                    } catch (failure: Exception) {
                        retained = listOf(partial, complete).firstOrNull { candidate ->
                            runCatching {
                                SaveOutputDirectory.open(candidate.parent).use { currentParent ->
                                    currentParent.key == parent.key &&
                                        currentParent.openChild(candidate.fileName.toString()).use { it.key == staging.key }
                                }
                            }.getOrDefault(false)
                        }
                        throw failure
                    }
                    retained = complete
                    parent.flush()
                    return SavedScreenshot(complete.resolve("screen.png"), application, parent.key, staging.key, output.key, output.hash())
                }
            }
        }
    } catch (failure: InterruptedException) {
        throw failure
    } catch (_: Exception) {
        throw ScreenshotSaveFailure("スクリーンショットの保存を完了できません。保存先・空き容量・アクセス権を確認してください。未完成の出力は保全します。", retained)
    }
}

internal fun screenshotDisplay(dump: String): String {
    val displays = Regex("(?m)^Display ([0-9]+) \\(HWC display [0-9]+\\):").findAll(dump).map { it.groupValues[1] }.toList()
    check(displays.size == 1 && displays.single().toULongOrNull() != null) {
        "撮影する端末画面を1つに特定できません。単一画面の対応端末を接続してください。"
    }
    return displays.single()
}

internal fun screenshotForeground(dump: String, application: String): String {
    val displays = Regex("(?m)^\\s*Display: mDisplayId=(\\d+)\\b").findAll(dump).map { it.groupValues[1] }.toList()
    check(displays == listOf("0")) { "単一の標準画面を確認できません。外部画面を外し、端末画面で対象アプリを表示してください。" }
    val keyguard = dump.lineSequence().dropWhile { it.trim() != "KeyguardServiceDelegate" }.drop(1).take(20).map(String::trim).toList()
    check(listOf("showing=false", "inputRestricted=false", "dreaming=false", "screenState=SCREEN_STATE_ON").all { it in keyguard }) {
        "画面の点灯とロック解除を確認できません。端末を操作してから再度保存してください。"
    }
    val focus = Regex("(?m)^\\s*mCurrentFocus=(Window\\{[^\\r\\n}]+})\\s*$").findAll(dump).map { it.groupValues[1] }.toList()
    check(focus.size == 1 && Regex("Window\\{[0-9a-fA-F]+ u[0-9]+ " + Regex.escape(application) + "/[^\\s}]+}").matches(focus.single())) {
        "選択アプリが最前面にありません。端末で対象アプリを表示してください。"
    }
    // shortcut: dumpsys observations are not atomic with screencap; retain the documented screen-switch/overlay limit.
    return focus.single()
}

internal fun validateScreenshotPng(bytes: ByteArray) {
    val input = DataInputStream(ByteArrayInputStream(bytes))
    check(input.readLong() == 0x89504e470d0a1a0aUL.toLong()) { "PNG画像を取得できませんでした。" }
    var first = true
    var dataSeen = false
    while (true) {
        val length = input.readInt()
        check(length >= 0 && length <= input.available() - 8) { "PNG画像が途中で切れています。" }
        val type = input.readNBytes(4)
        val data = input.readNBytes(length)
        val crc = CRC32().apply { update(type); update(data) }
        check(input.readInt().toLong() and 0xffffffffL == crc.value) { "PNG画像が破損しています。" }
        val kind = type.toString(Charsets.US_ASCII)
        if (kind == "IHDR") check(first) { "PNG画像のヘッダーが重複しています。" }
        if (first) {
            check(kind == "IHDR" && length == 13)
            val header = ByteBuffer.wrap(data)
            val width = header.int
            val height = header.int
            check(width in 1..16384 && height in 1..16384 && width.toLong() * height <= 16_777_216) { "画像の寸法が保存上限を超えています。" }
            first = false
        }
        if (kind == "IDAT") dataSeen = true
        if (kind == "IEND") { check(length == 0 && dataSeen && input.available() == 0); break }
    }
    MemoryCacheImageInputStream(ByteArrayInputStream(bytes)).use { imageInput ->
        val reader = ImageIO.getImageReadersByFormatName("png").next()
        try { reader.setInput(imageInput, false, true); reader.read(0).flush() } finally { reader.dispose() }
    }
}
