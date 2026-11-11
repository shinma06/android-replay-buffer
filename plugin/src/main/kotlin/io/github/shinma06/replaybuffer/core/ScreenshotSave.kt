package io.github.shinma06.replaybuffer.core

import java.io.ByteArrayInputStream
import java.io.DataInputStream
import java.nio.ByteBuffer
import java.nio.file.Path
import java.time.ZonedDateTime
import java.util.UUID
import java.util.zip.CRC32
import javax.imageio.ImageIO
import javax.imageio.stream.MemoryCacheImageInputStream

internal class ScreenshotSaveFailure(message: String, val retained: Path? = null) : Exception(message)

/** Identity belongs to this completed image, never to the latest screenshot or a reusable pathname. */
internal class SavedScreenshot(
    val file: Path,
    val serial: String,
    private val parentKey: Pair<Long, Long>,
    private val fileKey: Pair<Long, Long>,
    private val hash: String,
) {
    private var deleted = false

    @Synchronized
    fun delete() {
        if (deleted) return
        SaveOutputDirectory.open(file.parent).use { directory ->
            check(directory.key == parentKey) { "保存先が変更されています。ファイルは削除しません。" }
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

/**
 * One blocking request off EDT; no replay capture, device file, forward or server is created.
 *
 * Saves the current physical screen of the selected serial (or the single connected device) as shown,
 * whatever app, lock screen or overlay is in front. Failure/cancellation may leave output;
 * [ScreenshotSaveFailure.retained] identifies confirmed retained output when available.
 */
internal fun saveScreenshot(adbPath: Path, destination: Path, selectedSerial: String? = null): SavedScreenshot {
    require(destination.isAbsolute) { "設定で保存先の絶対パスを指定してください。" }
    fun active() { if (Thread.currentThread().isInterrupted) throw InterruptedException("スクリーンショットを取り消しました。") }
    lateinit var serial: String
    val bytes = OwnedAdb(adbPath).use { adb ->
        active()
        val devices = try { parseDevices(adb.command("devices", "-l")) }
        catch (_: Exception) { active(); throw ScreenshotSaveFailure("端末の接続状態を確認できません。接続とデバッグ許可を確認してください。") }
        serial = selectDevice(devices, selectedSerial)?.serial ?: throw ScreenshotSaveFailure(
            if (selectedSerial == null) "端末を1台だけ接続し、デバッグを許可してください。"
            else "録画対象の端末が接続されていません。同じ端末を再接続してください。",
        )
        active()
        val display = try {
            screenshotDisplay(adb.commandBytes("-s", serial, "shell", "dumpsys", "display",
                limit = 4 * 1024 * 1024, timeoutSeconds = 15).toString(Charsets.UTF_8))
        } catch (cancelled: InterruptedException) { throw cancelled } catch (_: Exception) { active(); null }
        active()
        val target = if (display == null) emptyArray() else arrayOf("-d", display)
        try {
            adb.commandBytes("-s", serial, "exec-out", "screencap", "-p", *target, limit = 64 * 1024 * 1024, timeoutSeconds = 15)
        } catch (cancelled: InterruptedException) {
            throw cancelled
        } catch (_: Exception) {
            active()
            throw ScreenshotSaveFailure("端末画面を撮影できません。接続とデバッグ許可を確認して再度保存してください。")
        }
    }
    active()
    validateScreenshotPng(bytes)
    val root = try {
        destination.toRealPath()
    } catch (_: Exception) {
        throw ScreenshotSaveFailure("保存先フォルダを確認できません。設定の保存先・アクセス権を確認してください。")
    }
    val name = screenshotFileName(ZonedDateTime.now())
    val partial = root.resolve(".${name.removeSuffix(".png")}.partial")
    val complete = root.resolve(name)
    var retained: Path? = null
    try {
        SaveOutputDirectory.open(root).use { parent ->
            active()
            parent.createFile(partial.fileName.toString()).use { output ->
                retained = partial
                output.requirePrivate()
                output.channel().use { channel ->
                    val data = ByteBuffer.wrap(bytes)
                    while (data.hasRemaining()) { active(); channel.write(data) }
                    channel.force(true)
                }
                val hash = java.util.HexFormat.of().formatHex(java.security.MessageDigest.getInstance("SHA-256").digest(bytes))
                fun verify(path: Path) {
                    SaveOutputDirectory.open(root).use { current ->
                        check(current.key == parent.key) { "保存先が変更されています。" }
                        current.verifyFile(path.fileName.toString(), output)
                    }
                    output.requirePrivate()
                    check(output.hash() == hash) { "保存画像が変更されています。" }
                }
                try {
                    active()
                    verify(partial)
                    parent.moveFile(partial.fileName.toString(), parent, complete.fileName.toString())
                    retained = complete
                    verify(complete)
                    parent.flush()
                } catch (failure: Exception) {
                    retained = listOf(partial, complete).firstOrNull { candidate -> runCatching {
                        SaveOutputDirectory.open(root).use { current ->
                            check(current.key == parent.key)
                            current.verifyFile(candidate.fileName.toString(), output)
                        }
                    }.isSuccess }
                    throw failure
                }
                return SavedScreenshot(complete, serial, parent.key, output.key, hash)
            }
        }
    } catch (failure: InterruptedException) {
        throw failure
    } catch (_: Exception) {
        throw ScreenshotSaveFailure("スクリーンショットの保存を完了できません。保存先・空き容量・アクセス権を確認してください。未完成の出力は保全します。", retained)
    }
}

/**
 * Physical id of logical display 0 (the default screen the user sees and scrcpy records) for `screencap -d`,
 * read from `dumpsys display`; null when it cannot be determined, so screencap uses its own default screen.
 *
 * LocalDisplayAdapter names physical screens `local:<id>`: the SurfaceFlinger physical id since Android 10,
 * the built-in display index that `screencap -d` takes before. Android 10+ DisplayInfo carries `displayId`;
 * older dumps only place it on the preceding `mDisplayId=` line of the logical display.
 */
internal fun screenshotDisplay(dump: String): String? {
    val defaultInfo = Regex("""DisplayInfo\{"[^"]*(?:", |, )displayId 0[",]""")
    val logicalId = Regex("""^\s*mDisplayId=(.*)$""")
    val logicalInfo = Regex("""^\s*m(?:Base|Override)DisplayInfo=DisplayInfo\{""")
    val uniqueId = Regex("""\buniqueId "([^"]*)"""")
    var logical: String? = null
    val ids = mutableSetOf<String>()
    for (line in dump.lineSequence()) {
        val header = logicalId.find(line)
        if (header != null) logical = header.groupValues[1].trim()
        val info = defaultInfo.find(line) ?: logicalInfo.find(line)?.takeIf { logical == "0" } ?: continue
        uniqueId.find(line, info.range.first)?.let { ids += it.groupValues[1] }
    }
    val id = ids.singleOrNull()?.takeIf { it.startsWith("local:") }?.removePrefix("local:") ?: return null
    return id.takeIf { it.toULongOrNull()?.toString() == it }
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
