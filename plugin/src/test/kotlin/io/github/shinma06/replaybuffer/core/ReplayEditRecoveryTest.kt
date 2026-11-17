package io.github.shinma06.replaybuffer.core

import com.google.gson.Gson
import com.google.gson.JsonObject
import io.github.shinma06.replaybuffer.ui.ReplayEditClosePolicy
import io.github.shinma06.replaybuffer.ui.ReplayEditCloseLifecycle
import io.github.shinma06.replaybuffer.ui.ReplayEditCloseWork
import io.github.shinma06.replaybuffer.ui.withSingleReplayOwner
import io.github.shinma06.replaybuffer.ui.replayEditPendingCloses
import io.github.shinma06.replaybuffer.ui.replayVideoOpening
import com.intellij.testFramework.LightVirtualFile
import com.intellij.openapi.extensions.PluginId
import io.github.shinma06.replaybuffer.ui.confirmedReplayDiscard
import java.nio.file.Files
import java.nio.file.Path
import java.util.concurrent.CompletableFuture
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertFailsWith
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue
import org.junit.jupiter.api.BeforeEach
import org.junit.jupiter.api.io.TempDir

class ReplayEditRecoveryTest {
    @TempDir lateinit var root: Path
    @BeforeEach fun canonicalFixtureParent() { root = root.toRealPath() }
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
    private fun absentOriginal(folder: Path) {
        assertTrue(bytes(folder).isEmpty())
        Files.delete(folder.resolve(RECOVERY_ORIGIN))
        Files.delete(folder)
    }

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

    @Test fun unrelatedPluginUnloadDoesNotDisableLaterSaveAndEditCloses() {
        val (_, save) = source()
        val lifecycle = ReplayEditCloseLifecycle()
        lifecycle.beforePluginUnload(PluginId.getId("unrelated.plugin"))
        val policy = ReplayEditClosePolicy().also { it.origin = assertNotNull(ReplayEditOrigin.capture(save)) }
        assertTrue(policy.eligible(true, false, lifecycle.closing))
        lifecycle.beforePluginUnload(PluginId.getId("io.github.shinma06.android-replay-buffer"))
        assertFalse(policy.eligible(true, false, lifecycle.closing))
        lifecycle.beforePluginUnload(PluginId.getId("unrelated.plugin"))
        assertTrue(lifecycle.closing)
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
        Files.move(folder, root.resolve("previous-origin"))
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
        absentOriginal(folder) // Simulates a receipt from the version that removed empty originals.
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
        absentOriginal(folder)
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

    @Test fun emptyDirectoryCleanupFailureCannotRemoveRestoreProvenance() {
        val (folder, save) = source()
        val original = bytes(folder)
        val recovery = assertNotNull(ReplayEditOrigin.capture(save)).relocate()
        // Even an external/failed rmdir cannot strand a markerless original: no eager cleanup.
        SaveOutputDirectory.open(root).use { parent -> assertFailsWith<Exception> { parent.deleteEmptyDirectory(name) } }
        assertTrue(Files.isRegularFile(folder.resolve(RECOVERY_ORIGIN)))
        assertEquals(folder, restoreReplayRecovery(recovery))
        assertEquals(original, bytes(folder))
    }

    private fun retryAbsentRestore(interrupt: (Path, Int) -> Unit = { _, _ -> }, flushFailure: Boolean = false) {
        val (folder, save) = source()
        val original = bytes(folder)
        val recovery = assertNotNull(ReplayEditOrigin.capture(save)).relocate()
        absentOriginal(folder)
        var moves = 0
        var flushReached = false
        assertFailsWith<Throwable> {
            restoreReplayRecovery(recovery, afterMove = { moves++; interrupt(recovery, it) }, beforeFlush = { flushReached = true; if (flushFailure) error("Injected flush failure") })
        }
        assertTrue(if (flushFailure) flushReached else moves > 0)
        // A fresh call knows neither the originating token nor the previous restore's handles/key.
        assertEquals(folder, restoreReplayRecovery(recovery))
        assertEquals(original, bytes(folder))
        SavedReplay.open(folder).use { assertEquals(save.requestId, it.manifest["save_id"].asString) }
    }

    @Test fun failedMoveToRecreatedOriginalCanRetryAfterRestart() = retryAbsentRestore({ _, _ -> error("Injected move failure") })
    @Test fun failedFlushOfRecreatedOriginalCanRetryAfterRestart() = retryAbsentRestore(flushFailure = true)
    @Test fun interruptedPartialRestoreCanRetryAfterRestart() = retryAbsentRestore({ _, count -> if (count == 1) throw AssertionError("Process interrupted") })
    @Test fun interruptedFullyMovedRestoreCanRetryAfterRestart() = retryAbsentRestore({ recovery, _ ->
        val receipt = Gson().fromJson(Files.readString(recovery.resolve(RECOVERY_RECORD)), JsonObject::class.java)
        if (receipt["hashes"].asJsonObject.keySet().none { Files.exists(recovery.resolve(it)) }) throw AssertionError("Process interrupted")
    })

    @Test fun cancelledRestoreToRecreatedOriginalCanRetryAfterRestart() {
        val (folder, save) = source()
        val original = bytes(folder)
        val recovery = assertNotNull(ReplayEditOrigin.capture(save)).relocate()
        absentOriginal(folder)
        var active = true
        assertFailsWith<ReplayRecoveryFailure> {
            restoreReplayRecovery(recovery, checkActive = { check(active) }, afterMove = { active = false })
        }
        assertEquals(folder, restoreReplayRecovery(recovery))
        assertEquals(original, bytes(folder))
    }

    @Test fun reopenedFileCancelsExtractedDecisionWhileDecoderCleanupWaits() {
        val (folder, save) = source()
        val original = bytes(folder)
        val origin = assertNotNull(ReplayEditOrigin.capture(save))
        val pending = ReplayEditCloseWork<Path>()
        val ticket = pending.begin(folder)
        val closed = CompletableFuture<Boolean>()
        var moved = false
        ticket.afterResourcesClosed(closed, { true }) { valid ->
            origin.relocate(checkActive = { check(valid()) }); moved = true
        }
        pending.clear(folder) // beforeFileOpened / ordinary reopen after fileClosed extracted it.
        closed.complete(true)
        assertFalse(moved)
        assertEquals(original, bytes(folder))
    }

    @Test fun reopenedFileCancelsQueuedAndMidMoveWorkAndRetainsBody() {
        val (folder, save) = source()
        val original = bytes(folder)
        val origin = assertNotNull(ReplayEditOrigin.capture(save))
        val pending = ReplayEditCloseWork<Path>()
        for (midMove in listOf(false, true)) {
            val ticket = pending.begin(folder)
            var queued: (() -> Unit)? = null
            ticket.afterResourcesClosed(CompletableFuture.completedFuture(true), { true }) { valid ->
                queued = { origin.relocate(checkActive = { check(valid()) }, afterMove = { if (midMove) pending.clear(folder) }) }
            }
            if (!midMove) pending.clear(folder)
            assertFailsWith<Exception> { assertNotNull(queued).invoke() }
            assertEquals(original, bytes(folder))
        }
    }

    @Test fun ambiguousProjectsRetainBothAndNeverPromptForAnotherProjectsOrdinaryEditor() {
        val (_, save) = source()
        val policies = mapOf("A" to ReplayEditClosePolicy(), "B" to ReplayEditClosePolicy())
        var prompts = 0
        val confirm: (String) -> Unit = { if (policies.getValue(it).eligible(true, false, false)) prompts++ }
        val retain = { policies.values.forEach { it.origin = null } }
        for (owners in listOf(listOf("A", "B"), emptyList(), null)) {
            policies.getValue("A").origin = assertNotNull(ReplayEditOrigin.capture(save))
            withSingleReplayOwner(owners, confirm, retain)
            assertEquals(0, prompts)
            assertFalse(policies.getValue("A").eligible(true, false, false))
        }
        withSingleReplayOwner(listOf("B"), confirm, retain)
        assertEquals(0, prompts)
        policies.getValue("A").origin = assertNotNull(ReplayEditOrigin.capture(save))
        withSingleReplayOwner(listOf("A"), confirm, retain)
        assertEquals(1, prompts)
    }

    @Test fun anotherProjectsProviderCancelsPendingAndMovingTicketsWithoutACloseManager() {
        val (folder, save) = source()
        val original = bytes(folder)
        val origin = assertNotNull(ReplayEditOrigin.capture(save))
        val file = LightVirtualFile("video.mp4")
        try {
            for (midMove in listOf(false, true)) {
                // A has closed and extracted its decision. B has no project service/manager.
                val ticket = replayEditPendingCloses.begin(file)
                val resources = CompletableFuture<Boolean>()
                var relocated = false
                var providerOpened = false
                ticket.afterResourcesClosed(resources, { true }) { valid ->
                    origin.relocate(checkActive = { check(valid()) }, afterMove = { providerOpened = true; replayVideoOpening(file) })
                    relocated = true
                }
                if (midMove) {
                    // Provider creation in B during A's first file move invalidates A's ticket.
                    resources.complete(true)
                    // CompletionStage captures the relocation's rollback exception.
                    assertTrue(providerOpened)
                } else {
                    replayVideoOpening(file) // The provider's first operation, before any controller.
                    resources.complete(true)
                }
                assertFalse(relocated)
                assertEquals(original, bytes(folder))
            }
        } finally { replayEditPendingCloses.clear(file) }
    }

    @Test fun parentDurabilityBarrierPrecedesEveryPayloadMoveAndFailureRetainsSource() {
        val (folder, save) = source()
        val original = bytes(folder)
        var moved = false
        var parentFlushed = false
        assertFailsWith<java.io.IOException> {
            assertNotNull(ReplayEditOrigin.capture(save)).relocate(afterMove = { moved = true }, flushParent = { parent ->
                parentFlushed = true
                assertEquals(original, bytes(folder))
                val recovery = parent.entries().single { it.startsWith("replay-recovery-") }
                assertTrue(Files.isRegularFile(root.resolve(recovery).resolve(RECOVERY_RECORD)))
                throw java.io.IOException("Injected parent fsync failure")
            })
        }
        assertTrue(parentFlushed)
        assertFalse(moved)
        assertEquals(original, bytes(folder))
        assertNull(ReplayEditOrigin.capture(save)) // Retained recovery metadata never grants a fresh origin.
    }

    private fun retryUninitializedStage(initialize: (SaveOutputDirectory, String) -> Unit) {
        val (folder, save) = source()
        val original = bytes(folder)
        val recovery = assertNotNull(ReplayEditOrigin.capture(save)).relocate()
        absentOriginal(folder)
        var initialized = false
        assertFailsWith<java.io.IOException> {
            restoreReplayRecovery(recovery, initializeStage = { stage, nonce -> initialized = true; initialize(stage, nonce) })
        }
        assertTrue(initialized)
        assertFalse(Files.exists(folder))
        val abandoned = Files.list(recovery).use { it.filter { child -> Files.isDirectory(child) }.toList().single() }
        // A failed stage may acquire unknown content. Never adopt, overwrite, or delete it.
        png(abandoned.resolve("unknown.txt"))
        val retained = bytes(abandoned)
        assertEquals(folder, restoreReplayRecovery(recovery))
        assertEquals(original, bytes(folder))
        assertEquals(retained, bytes(abandoned))
    }

    @Test fun failedFirstNonceCreateUsesFreshStageOnRestart() = retryUninitializedStage { _, _ -> throw java.io.IOException("Injected marker create failure") }
    @Test fun partialNonceWriteUsesFreshStageOnRestart() = retryUninitializedStage { stage, nonce ->
        stage.createFile(RECOVERY_ORIGIN).use { file -> file.channel().use { it.write(java.nio.ByteBuffer.wrap(nonce.take(4).toByteArray())) } }
        throw java.io.IOException("Injected partial marker write/force failure")
    }
    @Test fun nonceFlushFailureUsesFreshStageOnRestart() = retryUninitializedStage { stage, nonce ->
        stage.createFile(RECOVERY_ORIGIN).use { file -> file.channel().use { it.write(java.nio.ByteBuffer.wrap(nonce.toByteArray())); it.force(true) } }
        throw java.io.IOException("Injected marker directory flush failure")
    }

    @Test fun recreatedDestinationLedgerCannotSupplyArbitraryNamesOrChangedIdentity() {
        val (folder, save) = source()
        val original = bytes(folder)
        val recovery = assertNotNull(ReplayEditOrigin.capture(save)).relocate()
        absentOriginal(folder)
        assertFailsWith<ReplayRecoveryFailure> { restoreReplayRecovery(recovery, afterMove = { error("Injected failure") }) }
        val record = recovery.resolve(".replay-restore-key")
        val json = Files.readString(record)
        val retained = bytes(recovery) - record.fileName.toString()
        val mutations: List<(JsonObject) -> Unit> = listOf(
            { it.addProperty("stage", "../outside") },
            { it.addProperty("stage", root.resolve("outside").toString()) },
            { it.addProperty("key", "(0, 0)") },
        )
        for (mutate in mutations) {
            val changed = Gson().fromJson(json, JsonObject::class.java).also(mutate)
            Files.writeString(record, Gson().toJson(changed))
            assertFailsWith<Exception> { restoreReplayRecovery(recovery) }
            assertFalse(Files.exists(root.resolve("outside")))
            assertTrue(bytes(folder).isEmpty())
            assertEquals(retained, bytes(recovery) - record.fileName.toString())
        }
        Files.writeString(record, json)
        assertEquals(folder, restoreReplayRecovery(recovery))
        assertEquals(original, bytes(folder))
    }
}
