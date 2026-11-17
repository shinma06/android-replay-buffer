package io.github.shinma06.replaybuffer.core

import com.google.gson.Gson
import com.google.gson.JsonObject
import io.github.shinma06.replaybuffer.ui.ReplayEditClosePolicy
import io.github.shinma06.replaybuffer.ui.confirmedReplayDiscard
import java.nio.file.Files
import java.nio.file.Path
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertFailsWith
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue
import org.junit.jupiter.api.io.TempDir

class ReplayEditRecoveryTest {
    @TempDir lateinit var root: Path
    private val name = "replay-20240101-000000-manual-feedface"
    private fun source(): Pair<Path, SaveSnapshot> {
        val folder = root.resolve(name)
        PrivateFiles.createDirectory(folder)
        for (file in listOf(ReplayFiles.SUMMARY, ReplayFiles.RECORD, ReplayFiles.VIDEO, ReplayFiles.VIDEO_FRAMES,
            ReplayFiles.VIDEO_SOURCE, ReplayFiles.LOGCAT_DEVICE, ReplayFiles.LOGCAT_APP, ReplayFiles.TOUCHES)) {
            javaClass.getResourceAsStream("/replay-trim-parity/$name/$file")!!.use { Files.copy(it, folder.resolve(file)) }
            PrivateFiles.restrict(folder.resolve(file), false)
        }
        val manifest = Gson().fromJson(Files.readString(folder.resolve(ReplayFiles.RECORD)), JsonObject::class.java)
        return folder to SaveSnapshot(SavePhase.COMPLETED, manifest["save_id"].asString, directory = folder)
    }
    private fun bytes(folder: Path) = Files.list(folder).use { files -> files.filter { Files.isRegularFile(it) && it.fileName.toString() != RECOVERY_ORIGIN }.toList()
        .associate { it.fileName.toString() to Files.readAllBytes(it).toList() } }
    private fun png(path: Path) { Files.write(path, byteArrayOf(1, 2, 3)); PrivateFiles.restrict(path, false) }

    @Test fun explicitOriginAndHealthyIdleCloseAreRequiredAndLifecycleAlwaysRetains() {
        val (_, save) = source()
        val policy = ReplayEditClosePolicy()
        assertFalse(policy.eligible(true, false, false)) // Ordinary/direct/restored editor.
        policy.origin = assertNotNull(ReplayEditOrigin.capture(save))
        assertTrue(policy.eligible(true, false, false))
        assertFalse(policy.eligible(false, false, false)) // Load/validation error.
        assertFalse(policy.eligible(true, true, false)) // In-flight trim or PNG.
        assertFalse(policy.eligible(true, false, true)) // Project/app/plugin teardown.
        policy.trimExported = true
        assertFalse(policy.eligible(true, false, false))
        policy.trimExported = false // Cancelled export without an output is not a successful trim.
        assertTrue(policy.eligible(true, false, false))
        policy.ambiguousOutput = true
        assertFalse(policy.eligible(true, false, false))
        policy.ambiguousOutput = false
        policy.origin = null // Normal/notification opening the already-open originating editor.
        assertFalse(policy.eligible(true, false, false))
    }

    @Test fun onlyTheMatchingCompletedManualSaveCanGrantAuthority() {
        val (folder, save) = source()
        assertNotNull(ReplayEditOrigin.capture(save))
        for (rejected in listOf(save.copy(requestId = "another"), save.copy(autoEventId = "crash"),
            save.copy(phase = SavePhase.FAILED), save.copy(phase = SavePhase.CANCELLED),
            save.copy(content = SaveContent.LOGS_ONLY), save.copy(captureMode = CaptureMode.LOG_ONLY)))
            assertNull(ReplayEditOrigin.capture(rejected))
        Files.writeString(folder.resolve(ReplayFiles.RECORD), Files.readString(folder.resolve(ReplayFiles.RECORD)).replace("\"manual\"", "\"crash\""))
        assertNull(ReplayEditOrigin.capture(save))
    }

    @Test fun defaultKeepPromptFailureAndLifecycleChangesNeverConfirmDiscard() {
        var asks = 0
        assertFalse(confirmedReplayDiscard(false, { true }) { asks++; true })
        assertFalse(confirmedReplayDiscard(true, { false }) { asks++; true })
        assertEquals(0, asks)
        assertFalse(confirmedReplayDiscard(true, { true }) { false }) // Keep, Esc, or cancelled prompt.
        assertFalse(confirmedReplayDiscard(true, { true }) { error("Dialog unavailable") })
        var active = true
        assertFalse(confirmedReplayDiscard(true, { active }) { active = false; true })
        assertTrue(confirmedReplayDiscard(true, { true }) { true })
    }

    @Test fun recoveryLeavesInternalAndExternalPngsAndSurvivesRestart() {
        val (folder, save) = source()
        val original = bytes(folder)
        val origin = assertNotNull(ReplayEditOrigin.capture(save))
        val inside = folder.resolve("frame-inside.png"); png(inside)
        val outside = root.resolve("frame-outside.png"); png(outside)
        val recovery = origin.relocate()
        assertEquals(setOf("frame-inside.png"), bytes(folder).keys)
        assertEquals(listOf<Byte>(1, 2, 3), Files.readAllBytes(inside).toList())
        assertEquals(listOf<Byte>(1, 2, 3), Files.readAllBytes(outside).toList())
        assertTrue(Files.isRegularFile(recovery.resolve(RECOVERY_RECORD)))
        assertTrue(Files.isRegularFile(recovery.resolve("復元方法.txt")))
        // Restoration reads only the persisted receipt, with no originating session/token.
        assertEquals(folder, restoreReplayRecovery(recovery))
        assertEquals(original, bytes(folder) - "frame-inside.png")
        assertTrue(Files.exists(inside) && Files.exists(outside))
    }

    @Test fun reusedOriginalFolderIsNeverOverwritten() {
        val (folder, save) = source()
        val recovery = assertNotNull(ReplayEditOrigin.capture(save)).relocate()
        assertFalse(Files.exists(folder))
        PrivateFiles.createDirectory(folder)
        png(folder.resolve("new.png"))
        val retained = bytes(recovery)
        assertFailsWith<IllegalStateException> { restoreReplayRecovery(recovery) }
        assertEquals(setOf("new.png"), bytes(folder).keys)
        assertEquals(retained, bytes(recovery))
    }

    @Test fun restartRestoreRecreatesAnAbsentOriginalFolder() {
        val (folder, save) = source()
        val original = bytes(folder)
        val recovery = assertNotNull(ReplayEditOrigin.capture(save)).relocate()
        assertFalse(Files.exists(folder))
        assertEquals(folder, restoreReplayRecovery(recovery))
        assertEquals(original, bytes(folder))
        SavedReplay.open(folder).use { assertEquals(save.requestId, it.manifest["save_id"].asString) }
    }

    @Test fun replacedDirectoryAndUnverifiableOwnershipGrantNoAuthority() {
        val (folder, save) = source()
        val original = bytes(folder)
        val origin = assertNotNull(ReplayEditOrigin.capture(save))
        val old = root.resolve("kept-original")
        Files.move(folder, old)
        PrivateFiles.createDirectory(folder)
        png(folder.resolve("new.png"))
        assertFailsWith<IllegalStateException> { origin.relocate() }
        assertEquals(original, bytes(old))
        assertEquals(setOf("new.png"), bytes(folder).keys)
        assertNull(ReplayEditOrigin.capture(save))
    }

    @Test fun untrustedReceiptCannotChooseArbitraryTargetsOrPayloads() {
        val (folder, save) = source()
        val recovery = assertNotNull(ReplayEditOrigin.capture(save)).relocate()
        val receiptPath = recovery.resolve(RECOVERY_RECORD)
        val receipt = Files.readString(receiptPath)
        val retained = bytes(recovery) - RECOVERY_RECORD
        val mutations: List<(JsonObject) -> Unit> = listOf(
            { it.addProperty("originalName", "../outside") },
            { it.addProperty("originalName", root.resolve("outside").toString()) },
            { it["hashes"].asJsonObject.addProperty("../outside", "0".repeat(64)) },
            { it["hashes"].asJsonObject.addProperty("unknown.txt", "0".repeat(64)) },
            { it["hashes"].asJsonObject.addProperty(ReplayFiles.RECORD, "0".repeat(64)) },
            { it.addProperty("nonce", "unknown") },
        )
        for (mutate in mutations) {
            val json = Gson().fromJson(receipt, JsonObject::class.java)
            mutate(json)
            Files.writeString(receiptPath, Gson().toJson(json))
            assertFailsWith<Exception> { restoreReplayRecovery(recovery) }
            assertFalse(Files.exists(folder))
            assertFalse(Files.exists(root.resolve("outside")))
            assertEquals(retained, bytes(recovery) - RECOVERY_RECORD)
        }
    }

    @Test fun missingPayloadAndChangedMarkerRefuseRestorationWithoutOverwriting() {
        val (folder, save) = source()
        val origin = assertNotNull(ReplayEditOrigin.capture(save))
        png(folder.resolve("inside.png"))
        val recovery = origin.relocate()
        val retained = bytes(recovery)
        val marker = Files.readString(folder.resolve(RECOVERY_ORIGIN))
        Files.writeString(folder.resolve(RECOVERY_ORIGIN), "changed")
        assertFailsWith<IllegalStateException> { restoreReplayRecovery(recovery) }
        assertEquals(retained, bytes(recovery))
        Files.writeString(folder.resolve(RECOVERY_ORIGIN), marker)
        Files.move(recovery.resolve(ReplayFiles.VIDEO), root.resolve("preserved-video.mp4"))
        assertFailsWith<IllegalStateException> { restoreReplayRecovery(recovery) }
        assertEquals(setOf("inside.png"), bytes(folder).keys)
        assertTrue(Files.exists(root.resolve("preserved-video.mp4")))
    }

    @Test fun unknownMembersAndChangedPayloadKeepEverything() {
        val (folder, save) = source()
        val origin = assertNotNull(ReplayEditOrigin.capture(save))
        png(folder.resolve("unknown.txt"))
        val original = bytes(folder)
        assertFailsWith<IllegalStateException> { origin.relocate() }
        assertEquals(original, bytes(folder))
        Files.delete(folder.resolve("unknown.txt"))
        Files.write(folder.resolve(ReplayFiles.VIDEO), byteArrayOf(9))
        val changed = bytes(folder)
        assertFailsWith<IllegalStateException> { origin.relocate() }
        assertEquals(changed, bytes(folder))
    }

    @Test fun failureRollsBackAndRollbackConflictPreservesBothVersions() {
        val (folder, save) = source()
        val original = bytes(folder)
        val origin = assertNotNull(ReplayEditOrigin.capture(save))
        assertFailsWith<ReplayRecoveryFailure> { origin.relocate(afterMove = { error("Injected move failure") }) }
        assertEquals(original, bytes(folder))
        lateinit var replaced: String
        val failure = assertFailsWith<ReplayRecoveryFailure> {
            origin.relocate(afterMove = {
                replaced = (original.keys - bytes(folder).keys).single()
                png(folder.resolve(replaced)) // A new object prevents rollback, never overwrite it.
                error("Injected rollback conflict")
            })
        }
        assertEquals(original.getValue(replaced), Files.readAllBytes(failure.recovery.resolve(replaced)).toList())
        assertEquals(listOf<Byte>(1, 2, 3), Files.readAllBytes(folder.resolve(replaced)).toList())
        assertFailsWith<IllegalStateException> { restoreReplayRecovery(failure.recovery) }
        assertTrue(Files.exists(failure.recovery.resolve(replaced)))
        val replacement = root.resolve("replacement-kept")
        Files.move(folder.resolve(replaced), replacement)
        assertEquals(folder, restoreReplayRecovery(failure.recovery))
        assertEquals(original, bytes(folder))
        assertEquals(listOf<Byte>(1, 2, 3), Files.readAllBytes(replacement).toList())
    }

    @Test fun closingDuringMoveRollsBackAndPngNamedDirectoryIsRefused() {
        val (folder, save) = source()
        val origin = assertNotNull(ReplayEditOrigin.capture(save))
        val original = bytes(folder)
        var active = true
        assertFailsWith<ReplayRecoveryFailure> { origin.relocate(checkActive = { check(active) }, afterMove = { active = false }) }
        assertEquals(original, bytes(folder))
        PrivateFiles.createDirectory(folder.resolve("not-a-file.png"))
        assertFailsWith<Exception> { origin.relocate() }
        assertEquals(original, bytes(folder))
    }
}
