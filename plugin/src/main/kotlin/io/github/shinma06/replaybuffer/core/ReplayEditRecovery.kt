package io.github.shinma06.replaybuffer.core

import com.google.gson.Gson
import java.nio.ByteBuffer
import java.nio.file.Path
import java.nio.file.StandardOpenOption.READ
import java.util.UUID

/** Memory-only authority from one completed save-and-edit attempt. Never serialized into editor state. */
internal class ReplayEditOrigin private constructor(
    val requestId: String,
    val folder: Path,
    private val parentKey: Pair<Long, Long>,
    private val folderKey: Pair<Long, Long>,
    private val files: Map<String, Pair<Pair<Long, Long>, String>>,
) {
    @Synchronized
    fun relocate(checkActive: () -> Unit = {}, afterMove: (Int) -> Unit = {}, flushParent: (SaveOutputDirectory) -> Unit = { it.flush() }): Path {
        checkActive()
        check(PrivateFiles.isOwnedWithoutSharedWrite(folder.parent)) { "保存先の所有を確認できません。本体は残します。" }
        SaveOutputDirectory.open(folder.parent).use { parent ->
            check(parent.key == parentKey) { "保存先が変更されています。本体は残します。" }
            parent.openChild(folder.fileName.toString()).use { source ->
                check(source.key == folderKey) { "保存フォルダが変更されています。本体は残します。" }
                val pins = linkedMapOf<String, SaveOutputFile>()
                try {
                    check(source.entries().containsAll(files.keys)) { "保存内容が変更されています。本体は残します。" }
                    source.requirePrivate()
                    // Unknown content is never moved; PNGs remain at precisely their existing paths.
                    for (name in source.entries()) {
                        source.openFile(name).use { file ->
                            file.requirePrivate()
                            check(name in files || name.endsWith(".png", ignoreCase = true)) { "所有を確認できない内容があります。本体は残します。" }
                        }
                    }
                    files.forEach { (name, identity) ->
                        val pin = source.openFile(name)
                        pins[name] = pin
                        check(pin.key == identity.first && pin.hash() == identity.second) { "保存内容が変更されています。本体は残します。" }
                    }
                    checkActive()
                    val recoveryName = "replay-recovery-${UUID.randomUUID()}"
                    val recoveryPath = folder.parent.resolve(recoveryName)
                    parent.createDirectory(recoveryName).use { recovery ->
                        val nonce = UUID.randomUUID().toString()
                        val receipt = RecoveryReceipt(folder.fileName.toString(), folderKey.toString(), nonce, files.mapValues { it.value.second })
                        recovery.writeText(RECOVERY_RECORD, Gson().toJson(receipt))
                        recovery.writeText("復元方法.txt", "リプレイ本体の復元可能な保管場所です。自動削除はしません。\n元の場所: $folder\n" +
                            "Android Replay Buffer の「保存した動画を開く…」でこのフォルダを選び、確認後に復元できます。\nPNGは元の保存先に残しています。\n" +
                            "移動や復元が中断されて本体が両方の場所に分かれていても、由来印とハッシュが一致すれば復元できます。\n" +
                            "同名の競合や変更があれば上書きせず両方を残します。手作業で一方を削除せず、まず両方を確認してください。\n" +
                            "移動失敗後、本体が既に元の場所へ戻っている場合は追加の復元操作は不要です。\n")
                        source.writeText(RECOVERY_ORIGIN, nonce)
                        recovery.flush()
                        source.flush()
                        // Persist the new recovery entry, not only its child contents, before removing source names.
                        flushParent(parent)
                        val moved = mutableListOf<String>()
                        try {
                            for ((name, pin) in pins) {
                                checkActive()
                                source.verifyFile(name, pin)
                                source.moveFile(name, recovery, name)
                                moved += name
                                recovery.verifyFile(name, pin)
                                check(pin.hash() == files.getValue(name).second) { "移動中に保存内容が変更されました。" }
                                afterMove(moved.size)
                            }
                            checkActive()
                            recovery.flush()
                            source.flush()
                            checkActive()
                        } catch (failure: Exception) {
                            var restored = true
                            moved.asReversed().forEach { name -> if (runCatching { recovery.moveFile(name, source, name) }.isFailure) restored = false }
                            if (restored) runCatching {
                                source.flush(); recovery.flush()
                                check(source.readText(RECOVERY_ORIGIN) == nonce)
                                source.openFile(RECOVERY_ORIGIN).use { source.deleteFile(RECOVERY_ORIGIN, it) }
                                source.flush()
                            }
                            throw ReplayRecoveryFailure("本体の移動を完了できませんでした。元の場所と保管場所を確認してください。", recoveryPath, failure)
                        }
                    }
                    // Keep the durable nonce even without PNGs. Removing it before a fallible rmdir
                    // would strand a markerless directory after a crash or cleanup failure.
                    return recoveryPath
                } finally { pins.values.forEach { it.close() } }
            }
        }
    }

    companion object {
        fun capture(save: SaveSnapshot): ReplayEditOrigin? = runCatching {
            require(save.phase == SavePhase.COMPLETED && save.requestId != null && save.autoEventId == null)
            require(save.captureMode.videoRequested && save.content == SaveContent.ALL)
            val folder = checkNotNull(save.directory).toAbsolutePath().normalize()
            require(PrivateFiles.isOwnedWithoutSharedWrite(folder.parent))
            SavedReplay.open(folder).use { replay ->
                require(replay.manifest["trigger"]?.asString == "manual" && replay.manifest["save_id"]?.asString == save.requestId)
                SaveOutputDirectory.open(folder.parent).use { parent ->
                    require(parent.key == replay.parent.key)
                    parent.openChild(folder.fileName.toString()).use { source ->
                        require(source.key == replay.directoryKey)
                        source.requirePrivate()
                        require(source.entries().all { it in REPLAY_PAYLOAD })
                        val files = source.entries().associateWith { name -> source.openFile(name).use { it.requirePrivate(); it.key to it.hash() } }
                        files.forEach { (name, identity) ->
                            val expected = if (name == ReplayFiles.RECORD) replay.recordSha256
                                else replay.manifest["files_sha256"].asJsonObject[name]?.asString
                            require(identity.second == expected)
                        }
                        ReplayEditOrigin(save.requestId, folder, parent.key, source.key, files)
                    }
                }
            }
        }.getOrNull()
    }
}

private val REPLAY_PAYLOAD = setOf(ReplayFiles.SUMMARY, ReplayFiles.RECORD, ReplayFiles.VIDEO, ReplayFiles.VIDEO_FRAMES,
    ReplayFiles.VIDEO_SOURCE, ReplayFiles.LOGCAT_DEVICE, ReplayFiles.LOGCAT_APP, ReplayFiles.TOUCHES)
internal const val RECOVERY_RECORD = "recovery.json"
internal const val RECOVERY_ORIGIN = ".replay-recovery-origin"
private data class RecoveryReceipt(val originalName: String, val originalKey: String, val nonce: String, val hashes: Map<String, String>)
private const val RESTORE_KEY = ".replay-restore-key"
private const val RESTORE_STAGE = ".replay-restore-target"
private const val RESTORE_LOCK = ".replay-restore.lock"
private data class RestoreDestination(val key: String, val stage: String)
internal class ReplayRecoveryFailure(message: String, val recovery: Path, cause: Throwable) : Exception(message, cause)

/** Restart-safe restoration; paths in receipts are single names, never arbitrary destinations. */
internal fun restoreReplayRecovery(path: Path, checkActive: () -> Unit = {}, afterMove: (Int) -> Unit = {}, beforeFlush: () -> Unit = {},
    initializeStage: (SaveOutputDirectory, String) -> Unit = { stage, nonce -> stage.writeText(RECOVERY_ORIGIN, nonce); stage.flush() }): Path {
    checkActive()
    val folder = path.toAbsolutePath().normalize()
    check(PrivateFiles.isOwnedWithoutSharedWrite(folder.parent)) { "復元先の所有を確認できません。" }
    SaveOutputDirectory.open(folder.parent).use { parent ->
        parent.openChild(folder.fileName.toString()).use { recovery ->
            recovery.requirePrivate()
            recovery.withRestoreLock {
            val receipt = recovery.openFile(RECOVERY_RECORD).use { file ->
                file.requirePrivate()
                file.channel(setOf(READ)).use { channel ->
                    require(channel.size() <= 64 * 1024)
                    val bytes = ByteBuffer.allocate(channel.size().toInt())
                    while (bytes.hasRemaining()) check(channel.read(bytes) > 0)
                    bytes.flip()
                    Gson().fromJson(Charsets.UTF_8.decode(bytes).toString(), RecoveryReceipt::class.java)
                }
            }
            require(Regex("replay-[0-9]{8}-[0-9]{6}-manual-[a-z0-9]+").matches(receipt.originalName))
            require(UUID.fromString(receipt.nonce).toString() == receipt.nonce)
            require(Regex("\\(-?[0-9]+, -?[0-9]+\\)").matches(receipt.originalKey))
            require(receipt.hashes.keys.containsAll(setOf(ReplayFiles.RECORD, ReplayFiles.VIDEO, ReplayFiles.VIDEO_FRAMES)))
            require(receipt.hashes.keys.all { it in REPLAY_PAYLOAD } && receipt.hashes.values.all { Regex("[0-9a-f]{64}").matches(it) })
            val pins = linkedMapOf<String, SaveOutputFile>()
            try {
                receipt.hashes.forEach { (name, hash) ->
                    if (name in recovery.entries()) {
                        val file = recovery.openFile(name); pins[name] = file
                        file.requirePrivate(); check(file.hash() == hash) { "保管内容が変更されています。復元しません。" }
                    }
                }
                val existed = receipt.originalName in parent.entries()
                check(existed || pins.keys == receipt.hashes.keys) { "本体の一部を確認できません。保管場所に残します。" }
                if (!existed) prepareRestoreDestination(parent, recovery, receipt, initializeStage)
                val expectedKey = if (RESTORE_KEY in recovery.entries()) recovery.restoreDestination().key else receipt.originalKey
                require(Regex("\\(-?[0-9]+, -?[0-9]+\\)").matches(expectedKey))
                val original = parent.openChild(receipt.originalName)
                original.use { source ->
                    source.requirePrivate()
                    check(source.key.toString() == expectedKey) { "元の場所が別のフォルダに置き換わっています。復元しません。" }
                    check(RECOVERY_ORIGIN in source.entries() && source.readText(RECOVERY_ORIGIN) == receipt.nonce) { "元の場所の由来を確認できません。復元しません。" }
                    check((source.entries() + pins.keys).containsAll(receipt.hashes.keys)) { "本体の一部を確認できません。復元しません。" }
                    // A reused source path is never merged with replay data or unknown content.
                    for (name in source.entries()) {
                        if (name == RECOVERY_ORIGIN) continue
                        source.openFile(name).use { file ->
                            file.requirePrivate()
                            check(name.endsWith(".png", ignoreCase = true) ||
                                name !in pins && receipt.hashes[name] == file.hash()) { "元の場所が使用されています。上書きせず保管場所に残します。" }
                        }
                    }
                    val moved = mutableListOf<String>()
                    try {
                        pins.forEach { (name, pin) ->
                            checkActive()
                            recovery.verifyFile(name, pin)
                            recovery.moveFile(name, source, name); moved += name
                            source.verifyFile(name, pin)
                            afterMove(moved.size)
                        }
                        receipt.hashes.forEach { (name, hash) -> source.openFile(name).use { check(it.hash() == hash) { "復元中に内容が変更されました。" } } }
                        checkActive()
                        beforeFlush()
                        source.flush(); recovery.flush()
                        checkActive()
                    } catch (failure: Exception) {
                        moved.asReversed().forEach { name -> runCatching { source.moveFile(name, recovery, name) } }
                        runCatching { source.flush(); recovery.flush() }
                        throw ReplayRecoveryFailure("復元を完了できませんでした。保管場所と元の場所を確認してください。", folder, failure)
                    }
                    if (RECOVERY_ORIGIN in source.entries()) source.openFile(RECOVERY_ORIGIN).use { source.deleteFile(RECOVERY_ORIGIN, it) }
                }
                return folder.parent.resolve(receipt.originalName)
            } finally { pins.values.forEach { it.close() } }
            }
        }
    }
}

/** One cooperating restorer per recovery, across projects and processes; the anchor is never removed. */
private inline fun <T> SaveOutputDirectory.withRestoreLock(action: () -> T): T {
    val guard = runCatching { createFile(RESTORE_LOCK) }.getOrElse { openFile(RESTORE_LOCK) }
    guard.use { file ->
        file.requirePrivate()
        file.channel(setOf(READ)).use { check(it.size() == 0L) { "復元の排他情報を確認できません。" } }
        val owner = file.tryOwnerLock() ?: error("この保管場所は復元中です。完了後にもう一度お試しください。")
        owner.use { verifyFile(RESTORE_LOCK, file); return action() }
    }
}

/** Publish a closed, marked directory only after its identity is durable beside the receipt. */
private fun prepareRestoreDestination(parent: SaveOutputDirectory, recovery: SaveOutputDirectory, receipt: RecoveryReceipt,
    initializeStage: (SaveOutputDirectory, String) -> Unit) {
    val destination = if (RESTORE_KEY in recovery.entries()) recovery.restoreDestination() else {
        // An unpublished partial stage is untrusted. Retain it and initialize a fresh exclusive name.
        val stageName = "$RESTORE_STAGE-${UUID.randomUUID()}"
        val created = recovery.createDirectory(stageName).use {
            initializeStage(it, receipt.nonce)
            it.requirePrivate()
            check(it.entries() == setOf(RECOVERY_ORIGIN) && it.readText(RECOVERY_ORIGIN) == receipt.nonce)
            RestoreDestination(it.key.toString(), stageName)
        }
        val temporary = ".restore-key-${UUID.randomUUID()}.part"
        recovery.writeText(temporary, Gson().toJson(created))
        recovery.moveFile(temporary, recovery, RESTORE_KEY)
        recovery.flush()
        created
    }
    recovery.openChild(destination.stage).use {
        it.requirePrivate()
        check(it.key.toString() == destination.key && it.entries() == setOf(RECOVERY_ORIGIN) && it.readText(RECOVERY_ORIGIN) == receipt.nonce)
    }
    // All handles inside the staged directory are closed, including on Windows.
    recovery.moveFile(destination.stage, parent, receipt.originalName)
    parent.flush(); recovery.flush()
}

private fun SaveOutputDirectory.restoreDestination(): RestoreDestination {
    val text = readText(RESTORE_KEY)
    // Accept the earlier candidate's fixed-stage record; newly initialized stages always use UUIDs.
    val destination = if (text.startsWith("(")) RestoreDestination(text, RESTORE_STAGE)
        else Gson().fromJson(text, RestoreDestination::class.java)
    require(Regex("\\(-?[0-9]+, -?[0-9]+\\)").matches(destination.key))
    require(destination.stage == RESTORE_STAGE || destination.stage.startsWith("$RESTORE_STAGE-") &&
        runCatching { UUID.fromString(destination.stage.removePrefix("$RESTORE_STAGE-")).toString() == destination.stage.removePrefix("$RESTORE_STAGE-") }.getOrDefault(false))
    return destination
}

private fun SaveOutputDirectory.writeText(name: String, text: String) = createFile(name).use { file ->
    file.channel().use { channel -> val bytes = ByteBuffer.wrap(text.toByteArray(Charsets.UTF_8)); while (bytes.hasRemaining()) channel.write(bytes); channel.force(true) }
}

private fun SaveOutputDirectory.readText(name: String) = openFile(name).use { file ->
    file.requirePrivate()
    file.channel(setOf(READ)).use { channel ->
        require(channel.size() <= 64 * 1024)
        val bytes = ByteBuffer.allocate(channel.size().toInt())
        while (bytes.hasRemaining()) check(channel.read(bytes) > 0)
        bytes.flip()
        Charsets.UTF_8.decode(bytes).toString()
    }
}
