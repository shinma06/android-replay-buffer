package io.github.shinma06.replaybuffer.core

import com.google.gson.JsonParser
import org.jcodec.codecs.h264.H264Encoder
import org.jcodec.codecs.h264.H264Utils
import org.jcodec.common.model.ColorSpace
import org.jcodec.common.model.Picture
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.nio.ByteBuffer
import java.nio.file.Files
import java.nio.file.Path
import java.util.Base64
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertSame
import kotlin.test.assertTrue

class CrashAutosaveTest {
    @TempDir lateinit var root: Path

    @Test
    fun bothModesSaveExactlyOneCorroboratedPairAndRetainRawRowsAndImmutableFacts() {
        for (mode in CaptureMode.entries) for (reversed in listOf(false, true)) {
            CrashFixture(Files.createDirectory(root.resolve("$mode-$reversed")), mode).use { f ->
                if (mode.videoRequested) f.video()
                val java = javaCrash(opaque = byteArrayOf(0xc0.toByte(), 0x80.toByte()))
                val platform = platformCrash(first = if (reversed) 0 else 4242, second = if (reversed) 4242 else 0,
                    opaque = byteArrayOf(0xc0.toByte(), 0x80.toByte()))
                val ids = if (reversed) listOf(f.log(platform, 10_006_000_000), f.log(java, 10_005_000_000))
                    else listOf(f.log(java, 10_005_000_000), f.log(platform, 10_006_000_000))
                val pair = f.pair()
                val capture = assertNotNull(f.store.capture(f.settings, pair, f.at(10_020_000_000)))
                f.store.autosave.admitted(pair, capture.id, f.at(10_020_000_000))
                assertFailsWith<IllegalStateException> { f.store.capture(f.settings) }
                val trigger = assertNotNull(capture.autoTrigger)
                assertEquals(if (reversed) "user_pid" else "pid_user", trigger.facts["identity_interpretation"])
                val output = SaveWriter().write(capture, f.root, { false }) { Files.move(it.partial, it.complete) }
                val manifest = JsonParser.parseString(Files.readString(output.directory.resolve("session.json"))).asJsonObject
                assertEquals("platform_reported_uncaught_exception", manifest["auto_trigger"].asJsonObject["kind"].asString)
                assertEquals(mode.name, manifest["capture_mode"].asString)
                assertEquals(if (mode.videoRequested) 1L else 0L, Files.list(output.directory).use { files -> files.filter { it.toString().endsWith(".mp4") }.count() })
                assertTrue(!manifest["auto_trigger"].asJsonObject.has("terminal_outcome"))
                assertTrue(!manifest["auto_trigger"].asJsonObject.has("attempt_id"))
                val deviceRows = Files.readAllLines(output.directory.resolve("logcat-device.jsonl")).map { JsonParser.parseString(it).asJsonObject }
                val appRows = Files.readAllLines(output.directory.resolve("logcat-app.jsonl")).map { JsonParser.parseString(it).asJsonObject }
                for (source in listOf(java, platform)) {
                    val row = deviceRows.single { it["epoch_ns"].asString == source.wall.toString() }
                    assertContentEquals(source.raw, Base64.getDecoder().decode(row["payload_base64"].asString))
                    assertEquals(source.decodeStatus, row["decode_status"].asString)
                    assertTrue(row["record_id"].asString in ids)
                    assertEquals(source.lid == 4, appRows.any { it["record_id"] == row["record_id"] })
                }
                f.store.release(capture.id)
                f.log(java, 10_007_000_000); f.log(platform, 10_008_000_000)
                assertTrue(f.store.autosave.drain(f.at(10_020_000_000)).isEmpty())
                assertEquals(7, f.store.autosave.snapshot().admissionsRemaining)
                assertEquals(0, f.store.autosave.snapshot().notSavedCount)
            }
        }
    }

    @Test
    fun knownOtherTrafficAndWrongReportersSpendNoTargetBudgetAndMalformedUnassociatedProcessIsIgnored() {
        CrashFixture(root).use { f ->
            repeat(300) { index ->
                f.log(javaCrash(process = "com.example.other", uid = 10124, wall = CRASH_WALL + 10_002_000_000 + index), 10_005_000_000)
                f.log(platformCrash(process = "com.example.other", wall = CRASH_WALL + 10_002_000_000 + index), 10_006_000_000)
                f.log(platformCrash(pid = 9000, uid = 10124, wall = CRASH_WALL + 10_002_000_000 + index), 10_006_000_000)
                f.log(platformCrash(pid = 9000, wall = CRASH_WALL + 10_002_000_000 + index), 10_006_000_000)
            }
            val malformed = platformCrash().raw.apply { ByteBuffer.wrap(this).order(java.nio.ByteOrder.LITTLE_ENDIAN).putInt(17, -1) }
            f.log(wireLog(malformed, 2, 500, 1000), 10_006_000_000)
            f.log(javaCrash(uid = 10124, wall = CRASH_WALL - 100_000_000_000), 10_006_000_000)
            assertTrue(f.store.autosave.drain(f.at(10_020_000_000)).isEmpty())
            assertEquals(0, f.store.autosave.snapshot().notSavedCount)
            assertNull(f.store.autosave.snapshot().lastOutcome)
            assertEquals(AutoSaveState.ARMED, f.store.autosave.snapshot().state)
            f.standard()
            val pair = f.pair()
            val capture = assertNotNull(f.store.capture(f.settings, pair, f.at(10_020_000_000)))
            assertEquals(1205, capture.logs.size) // Unmapped rows remain raw evidence too.
        }
    }

    @Test
    fun appPollPublishesItsReporterWithIdentityAndRejectsInvalidOwnersOrInputsTogether() {
        CrashFixture(root).use { f ->
            val poll = ReporterPoll(f.at(10_080_000_000), f.at(10_090_000_000), 1, 600)
            for ((owner, value) in listOf(0L to poll.copy(generation = 0), 1L to poll.copy(generation = 0))) {
                f.store.app("com.example.target", 10124, setOf(4243), owner, true, f.at(10_100_000_000), value)
            }
            assertFailsWith<IllegalArgumentException> {
                f.store.app("com.example.target", 10124, (1..1025).toSet(), 1, true, f.at(10_100_000_000), poll)
            }
            f.standard()
            val original = assertNotNull(f.store.capture(f.settings, f.pair(), f.at(10_020_000_000)))
            assertEquals(10123L, (original.autoTrigger!!.facts["target_identity"] as Map<*, *>)["uid"])
            assertEquals(500, (original.autoTrigger!!.facts["reporter_poll"] as Map<*, *>)["pid"])
            f.store.release(original.id)
            f.store.app("com.example.target", 10124, setOf(4243), 1, true, f.at(10_100_000_000), poll)
            f.log(javaCrash(pid = 4243, uid = 10124, wall = CRASH_WALL + 10_103_000_000), 10_105_000_000)
            f.log(platformCrash(first = 4243, pid = 600, wall = CRASH_WALL + 10_104_000_000), 10_106_000_000)
            val pair = f.store.autosave.drain(f.at(10_120_000_000)).single()
            val capture = assertNotNull(f.store.capture(f.settings, pair, f.at(10_120_000_000)))
            assertEquals(10124L, (capture.autoTrigger!!.facts["target_identity"] as Map<*, *>)["uid"])
            assertEquals(600, (capture.autoTrigger!!.facts["reporter_poll"] as Map<*, *>)["pid"])
        }
    }

    @Test
    fun historicalOtherPackageCannotSpendBudgetByClaimingTheNewTarget() {
        CrashFixture(root).use { f ->
            val settings = f.settings.copy(application = ApplicationTarget("com.example.next"))
            f.store.app("com.example.next", 10124, setOf(4243), 1, true, f.at(10_100_000_000))
            f.store.configureAuto(settings, 2, "fixture", true, f.at(10_110_000_000))
            repeat(300) { index ->
                f.log(javaCrash(process = "com.example.next", wall = CRASH_WALL + 10_003_000_000 + index), 10_120_000_000)
            }
            assertEquals(AutoSaveState.ARMED, f.store.autosave.snapshot().state)
            assertEquals(0, f.store.autosave.snapshot().notSavedCount)
            assertNull(f.store.autosave.snapshot().lastOutcome)
            assertNull(f.store.autosave.snapshot().warning)
            assertEquals(8, f.store.autosave.snapshot().admissionsRemaining)
            f.log(javaCrash(process = "com.example.next", uid = 10124, pid = 4243, wall = CRASH_WALL + 10_200_000_000), 10_210_000_000)
            f.log(platformCrash(process = "com.example.next", first = 4243, wall = CRASH_WALL + 10_201_000_000), 10_211_000_000)
            val pair = f.store.autosave.drain(f.at(10_220_000_000)).single()
            val capture = assertNotNull(f.store.capture(settings, pair, f.at(10_220_000_000)))
            assertEquals(10124L, (capture.autoTrigger!!.facts["target_identity"] as Map<*, *>)["uid"])
            assertEquals(303, capture.logs.size)
            assertEquals(301, capture.logs.count { it.source.uid == 10123L })
        }
    }

    @Test
    fun delayedCrashUsesHistoricalUidAfterReinstallAndDoesNotBorrowCurrentIdentity() {
        for (exclusive in listOf(true, false)) {
            CrashFixture(Files.createDirectory(root.resolve("historical-$exclusive")), exclusive = exclusive).use { f ->
                f.store.app("com.example.target", 10124, setOf(4243), 1, true, f.at(10_100_000_000))
                if (exclusive) {
                    f.log(javaCrash(uid = 10124), 10_200_000_000)
                    assertEquals(0, f.store.autosave.snapshot().notSavedCount)
                }
                val java = javaCrash()
                val platform = platformCrash()
                val ids = listOf(f.log(java, 10_201_000_000), f.log(platform, 10_202_000_000))
                val pairs = f.store.autosave.drain(f.at(10_220_000_000))
                if (!exclusive) {
                    assertTrue(pairs.isEmpty())
                    assertEquals(AutoSaveReason.UNKNOWN_IDENTITY, f.store.autosave.snapshot().lastOutcome?.reason)
                    assertEquals(8, f.store.autosave.snapshot().admissionsRemaining)
                    assertNotNull(f.store.capture(f.settings, host = f.at(10_220_000_000)))
                } else {
                    val capture = assertNotNull(f.store.capture(f.settings, pairs.single(), f.at(10_220_000_000)))
                    assertEquals(10123L, (capture.autoTrigger!!.facts["target_identity"] as Map<*, *>)["uid"])
                    val output = SaveWriter().write(capture, f.root, { false }) { Files.move(it.partial, it.complete) }
                    val rows = Files.readAllLines(output.directory.resolve("logcat-device.jsonl")).map { JsonParser.parseString(it).asJsonObject }
                    for ((id, source) in ids.zip(listOf(java, platform))) {
                        val row = rows.single { it["record_id"].asString == id }
                        assertContentEquals(source.raw, Base64.getDecoder().decode(row["payload_base64"].asString))
                        assertEquals(source.decodeStatus, row["decode_status"].asString)
                    }
                }
            }
        }
        CrashFixture(Files.createDirectory(root.resolve("historical-boundary"))).use { f ->
            f.store.app("com.example.target", 10124, setOf(4243), 1, true, f.at(10_004_500_000))
            f.log(javaCrash(), 10_201_000_000)
            f.log(platformCrash(), 10_202_000_000)
            val pair = f.store.autosave.drain(f.at(10_220_000_000)).single()
            assertEquals(AutoSaveReason.UNKNOWN_IDENTITY,
                assertFailsWith<AutoSaveRejected> { f.store.capture(f.settings, pair, f.at(10_220_000_000)) }.reason)
            assertNotNull(f.store.capture(f.settings, host = f.at(10_220_000_000)))
        }
    }

    @Test
    fun finalMapperRefinesTriggerAndRowsTogetherAndRejectsBoundaryCrossingBeforePin() {
        for (start in listOf(9_900_000_000, 10_001_500_000)) {
            CrashFixture(Files.createDirectory(root.resolve("s$start")), start = start).use { f ->
                f.standard()
                f.anchor(10_010_000_000, sourceDelta = -1_000_000)
                val pair = f.pair()
                if (start == 10_001_500_000L) {
                    assertEquals(AutoSaveReason.NOT_SAVED_WINDOW_UNKNOWN,
                        assertFailsWith<AutoSaveRejected> { f.store.capture(f.settings, pair, f.at(10_020_000_000)) }.reason)
                    assertNotNull(f.store.capture(f.settings, host = f.at(10_020_000_000))) // No probe pin was created.
                } else {
                    val capture = assertNotNull(f.store.capture(f.settings, pair, f.at(10_020_000_000)))
                    val facts = capture.autoTrigger!!.facts
                    assertEquals(10_019_000_000, capture.end)
                    for ((key, source, expected) in listOf(Triple("java_evidence", 4, 10_002_700_000L), Triple("platform_evidence", 2, 10_003_600_000L))) {
                        val evidence = facts[key] as Map<*, *>
                        assertEquals(expected.toString(), evidence["frozen_sequence_ns"])
                        assertEquals("2000000", evidence["uncertainty_ns"])
                        assertEquals(expected, capture.logs.single { it.source.lid == source }.time.sequence)
                    }
                    f.anchor(10_020_000_000, sourceDelta = -1_000_000)
                    assertEquals("10003600000", (facts["platform_evidence"] as Map<*, *>)["frozen_sequence_ns"])
                }
            }
        }
    }

    @Test
    fun receivedPairsDrainBeforeExpiryAt26SecondsButLateReceiptsAndExpiredWindowDoNotSave() {
        for ((delay, seconds) in listOf(4_000_000_000L to 30, 5_000_000_000L to 30, 5_000_000_001L to 30, 4_000_000_000L to 1)) {
            CrashFixture(Files.createDirectory(root.resolve("d$delay-$seconds")), seconds = seconds).use { f ->
                for (time in listOf(12L, 16, 20)) f.anchor(time * 1_000_000_000)
                f.log(javaCrash(wall = CRASH_WALL + 20_003_000_000), 20_005_000_000)
                f.anchor(24_000_000_000)
                f.reporter(20_005_000_000 + delay - 500_000_000, 20_005_000_000 + delay - 499_000_000)
                f.log(platformCrash(wall = CRASH_WALL + 20_004_000_000), 20_005_000_000 + delay)
                f.anchor(26_000_000_000)
                val pairs = f.store.autosave.drain(f.at(26_010_000_000))
                if (delay > 5_000_000_000) {
                    assertTrue(pairs.isEmpty())
                    assertEquals(AutoSaveReason.UNKNOWN_UNCORROBORATED, f.store.autosave.snapshot().lastOutcome?.reason)
                } else if (seconds == 1) {
                    assertEquals(AutoSaveReason.NOT_SAVED_EVENT_OUTSIDE_WINDOW,
                        assertFailsWith<AutoSaveRejected> { f.store.capture(f.settings, pairs.single(), f.at(26_010_000_000)) }.reason)
                } else assertNotNull(f.store.capture(f.settings, pairs.single(), f.at(26_010_000_000)))
            }
        }
    }

    @Test
    fun earlierJavaRowCanBeOutsideOutputWithoutClaimingFullPreReportCoverage() {
        CrashFixture(root, seconds = 1, start = 8_000_000_000).use { f ->
            f.log(javaCrash(wall = CRASH_WALL + 9_000_000_000), 10_005_000_000)
            f.log(platformCrash(wall = CRASH_WALL + 10_010_000_000), 10_100_000_000)
            val pair = f.store.autosave.drain(f.at(10_201_000_000)).single()
            val capture = assertNotNull(f.store.capture(f.settings, pair, f.at(10_201_000_000)))
            val facts = capture.autoTrigger!!.facts
            assertEquals(false, (facts["java_evidence"] as Map<*, *>)["in_output"])
            assertEquals(true, (facts["platform_evidence"] as Map<*, *>)["in_output"])
            assertEquals("190000000", facts["pre_report_shortfall_nominal_ns"])
            assertEquals("187500000", facts["pre_report_shortfall_min_ns"])
            assertEquals("192500000", facts["pre_report_shortfall_max_ns"])
        }
    }

    @Test
    fun evictedPlatformEvidenceRejectsBeforePinEvenWithUnrelatedLogsOrVideo() {
        for (mode in CaptureMode.entries) {
            val directory = Files.createDirectory(root.resolve(mode.name))
            val store = CaptureStore(directory.resolve("ring"), logLimit = 1024, minFree = 0, captureMode = mode)
            CrashFixture(directory, mode, suppliedStore = store).use { f ->
                if (mode.videoRequested) f.video()
                f.standard()
                val ordinary = wireLog(byteArrayOf(4, 84, 0) + ByteArray(700) { 120 } + byteArrayOf(0),
                    0, 4242, 10123, CRASH_WALL + 10_007_000_000)
                val retained = f.log(ordinary, 10_008_000_000)
                val pair = f.pair()
                assertEquals(AutoSaveReason.NOT_SAVED_EVIDENCE_LOST,
                    assertFailsWith<AutoSaveRejected> { f.store.capture(f.settings, pair, f.at(10_020_000_000)) }.reason)
                assertEquals(8, f.store.autosave.snapshot().admissionsRemaining)
                val manual = assertNotNull(f.store.capture(f.settings, host = f.at(10_020_000_000)))
                assertEquals(listOf(retained), manual.logs.map { it.id })
                assertEquals(mode.videoRequested, manual.video.isNotEmpty())
                assertNull(manual.autoTrigger)
            }
        }
    }

    @Test
    fun mismatchedPlatformDoesNotConsumeJavaBeforeCorrectReportOrItsDeadline() {
        for (tickBetween in listOf(false, true)) {
            CrashFixture(Files.createDirectory(root.resolve("tick-$tickBetween"))).use { f ->
                val java = f.log(javaCrash(), 10_005_000_000)
                f.log(platformCrash(first = 7777), 10_006_000_000)
                if (tickBetween) assertTrue(f.store.autosave.drain(f.at(10_007_000_000)).isEmpty())
                val platform = f.log(platformCrash(wall = CRASH_WALL + 10_008_000_000), 10_009_000_000)
                val pair = f.pair()
                assertEquals(java, pair.java.row.id)
                assertEquals(platform, pair.platform.row.id)
                assertEquals(1, f.store.autosave.snapshot().notSavedCount)
                assertNotNull(f.store.capture(f.settings, pair, f.at(10_020_000_000)))
            }
        }
        CrashFixture(Files.createDirectory(root.resolve("expiry"))).use { f ->
            f.log(javaCrash(), 10_005_000_000)
            f.log(platformCrash(first = 7777), 10_006_000_000)
            assertTrue(f.store.autosave.drain(f.at(10_007_000_000)).isEmpty())
            assertEquals(1, f.store.autosave.snapshot().notSavedCount)
            assertTrue(f.store.autosave.drain(f.at(15_005_000_001)).isEmpty())
            assertEquals(2, f.store.autosave.snapshot().notSavedCount)
            assertEquals(AutoSaveReason.UNKNOWN_UNCORROBORATED, f.store.autosave.snapshot().lastOutcome?.reason)
        }
    }

    @Test
    fun equivalentPidUserOrderIsOneIdentityWhileMismatchesSharedUidAndAmbiguityReject() {
        CrashFixture(root, uid = 1010123, pid = 10).use { f ->
            f.log(javaCrash(pid = 10, uid = 1010123), 10_005_000_000)
            f.log(platformCrash(first = 10, second = 10), 10_006_000_000)
            assertEquals("equivalent_pid_user_and_user_pid", f.pair().let { identityOrder(it.java, it.platform) })
        }
        for (condition in listOf("mismatch", "shared", "ambiguous")) {
            CrashFixture(Files.createDirectory(root.resolve(condition)), exclusive = condition != "shared").use { f ->
                f.log(javaCrash(), 10_005_000_000)
                if (condition == "ambiguous") f.log(javaCrash(wall = CRASH_WALL + 10_003_500_000), 10_005_500_000)
                f.log(platformCrash(first = if (condition == "mismatch") 500 else 4242), 10_006_000_000)
                assertTrue(f.store.autosave.drain(f.at(10_020_000_000)).isEmpty())
                assertEquals(if (condition == "ambiguous") AutoSaveReason.UNKNOWN_AMBIGUOUS else AutoSaveReason.UNKNOWN_IDENTITY,
                    f.store.autosave.snapshot().lastOutcome?.reason)
            }
        }
    }

    @Test
    fun boundsSuspendWithoutEvictionAndExplicitOffOnClearsTheSessionBudget() {
        for (large in listOf(false, true)) {
            CrashFixture(Files.createDirectory(root.resolve("mail-$large"))).use { f ->
                repeat(17) { index -> f.log(javaCrash(wall = CRASH_WALL + 10_002_000_000 + index,
                    opaque = if (large) ByteArray(4400) { 65 } else byteArrayOf()), 10_005_000_000) }
                assertEquals(AutoSaveState.SUSPENDED, f.store.autosave.snapshot().state)
                assertTrue(f.store.autosave.drain(f.at(10_020_000_000)).isEmpty())
                f.store.configureAuto(f.settings.copy(autoSaveOnCrash = false), 2, "fixture", true, f.at(10_020_000_000))
                f.store.configureAuto(f.settings, 3, "fixture", true, f.at(10_021_000_000))
                assertEquals(AutoSaveState.ARMED, f.store.autosave.snapshot().state)
                assertEquals(8, f.store.autosave.snapshot().admissionsRemaining)
            }
        }
        CrashFixture(Files.createDirectory(root.resolve("terminal"))).use { f ->
            repeat(257) { index -> f.log(javaCrash(textPid = 4243, wall = CRASH_WALL + 10_002_000_000 + index), 10_005_000_000) }
            assertEquals(AutoSaveState.SUSPENDED, f.store.autosave.snapshot().state)
            assertEquals(256, f.store.autosave.snapshot().notSavedCount)
        }
    }

    @Test
    fun cooldownUsesAdmissionTimeAndEightAdmissionsSuspendWithoutBlockingManualSave() {
        CrashFixture(root).use { f ->
            var anchor = 10_000_000_000L
            repeat(8) { index ->
                val time = 10_000_000_000L + index * 31_000_000_000L
                while (anchor < time) { anchor = minOf(anchor + 4_000_000_000, time); f.anchor(anchor) }
                f.reporter(time - 500_000_000, time - 499_000_000)
                f.log(javaCrash(wall = CRASH_WALL + time + 3_000_000), time + 5_000_000)
                f.log(platformCrash(wall = CRASH_WALL + time + 4_000_000), time + 6_000_000)
                val host = f.at(time + 20_000_000)
                val pair = f.store.autosave.drain(host).single()
                assertNull(f.store.autosave.admissionProblem(pair, host))
                val capture = assertNotNull(f.store.capture(f.settings, pair, host))
                f.store.autosave.admitted(pair, capture.id, host)
                f.store.release(capture.id)
                if (index < 7) {
                    assertEquals(AutoSaveReason.NOT_SAVED_COOLDOWN, f.store.autosave.admissionProblem(pair, host + 29_999_999_999))
                    assertNull(f.store.autosave.admissionProblem(pair, host + 30_000_000_000))
                }
            }
            assertEquals(0, f.store.autosave.snapshot().admissionsRemaining)
            assertEquals(AutoSaveState.SUSPENDED, f.store.autosave.snapshot().state)
            val suspended = f.store.autosave.snapshot()
            val manual = assertNotNull(f.store.capture(f.settings, host = f.at(anchor + 21_000_000)))
            assertNull(manual.autoTrigger)
            f.store.configureAuto(f.settings.copy(replaySeconds = 60), 2, "fixture", true, f.at(anchor + 22_000_000))
            assertEquals(suspended, f.store.autosave.snapshot())
        }
    }

    @Test
    fun applyInvalidatesOnlyUnadmittedReportsAndFreshPairsUseTheNewRevision() {
        for (stage in listOf("java-only", "pair-before-drain", "pair-after-drain")) {
            CrashFixture(Files.createDirectory(root.resolve(stage))).use { f ->
                f.log(javaCrash(), 10_005_000_000)
                if (stage != "java-only") f.log(platformCrash(), 10_006_000_000)
                val oldPair = if (stage == "pair-after-drain") f.pair() else null
                val applied = if (stage == "java-only") f.settings.copy(saveDirectory = root.resolve("new-destination"))
                    else f.settings.copy(replaySeconds = 60)
                f.store.configureAuto(applied, 2, "fixture", true, f.at(10_020_000_000))
                if (stage == "java-only") f.log(platformCrash(), 10_025_000_000)
                assertTrue(f.store.autosave.drain(f.at(10_030_000_000)).isEmpty(), stage)
                if (oldPair != null) {
                    assertEquals(AutoSaveReason.NOT_SAVED_PRE_ARM_EVENT,
                        assertFailsWith<AutoSaveRejected> { f.store.capture(applied, oldPair, f.at(10_030_000_000)) }.reason)
                } else {
                    assertEquals(AutoSaveReason.NOT_SAVED_PRE_ARM_EVENT, f.store.autosave.snapshot().lastOutcome?.reason, stage)
                    assertEquals(if (stage == "java-only") 1 else 2, f.store.autosave.snapshot().notSavedCount)
                }
                assertEquals(8, f.store.autosave.snapshot().admissionsRemaining)

                val time = if (stage == "java-only") {
                    f.anchor(12_000_000_000)
                    f.anchor(16_000_000_000)
                    f.reporter(15_500_000_000, 15_501_000_000)
                    assertTrue(f.store.autosave.drain(f.at(16_020_000_000)).isEmpty())
                    assertEquals(AutoSaveReason.UNKNOWN_UNCORROBORATED, f.store.autosave.snapshot().lastOutcome?.reason)
                    16_040_000_000
                } else 10_040_000_000
                // Source/receipt/identity/window stay valid; only the Apply boundary separates these pairs.
                val java = javaCrash(wall = CRASH_WALL + time)
                val platform = platformCrash(wall = CRASH_WALL + time + 1_000_000)
                f.log(java, time + 2_000_000)
                f.store.configureAuto(applied, 2, "fixture", true, f.at(time + 3_000_000))
                f.log(platform, time + 4_000_000)
                val host = f.at(time + 20_000_000)
                val fresh = f.store.autosave.drain(host).single()
                val capture = assertNotNull(f.store.capture(applied, fresh, host))
                f.store.autosave.admitted(fresh, capture.id, host)
                val trigger = assertNotNull(capture.autoTrigger)
                assertEquals(2L, trigger.facts["option_revision"])
                assertEquals(f.at(10_020_000_000).toString(), trigger.facts["arming_host_ns"])
                assertEquals(7, f.store.autosave.snapshot().admissionsRemaining)
                f.store.configureAuto(applied.copy(replaySeconds = 90), 3, "fixture", true, f.at(time + 30_000_000))
                assertSame(trigger, capture.autoTrigger)
                assertEquals(2L, trigger.facts["option_revision"])
                assertFailsWith<IllegalStateException> { f.store.capture(applied, host = f.at(time + 40_000_000)) }
                f.store.release(capture.id)
                f.log(java, time + 42_000_000)
                f.log(platform, time + 44_000_000)
                assertTrue(f.store.autosave.drain(f.at(time + 50_000_000)).isEmpty())
                assertEquals(7, f.store.autosave.snapshot().admissionsRemaining)
            }
        }
    }

    @Test
    fun sameRevisionPollingAndOrdinaryApplyPreserveHistoryCooldownAndWarning() {
        CrashFixture(root).use { f ->
            f.log(javaCrash(), 10_005_000_000)
            f.store.configureAuto(f.settings, 1, "fixture", true, f.at(10_005_500_000))
            f.log(platformCrash(), 10_006_000_000)
            val pair = f.pair()
            assertEquals(f.at(100_000_000), pair.java.barrier.host)
            val capture = assertNotNull(f.store.capture(f.settings, pair, f.at(10_020_000_000)))
            f.store.autosave.admitted(pair, capture.id, f.at(10_020_000_000))
            f.store.autosave.outcome(pair.id, AutoSaveReason.SAVE_FAILED, capture.id)
            f.store.release(capture.id)
            val before = f.store.autosave.snapshot()
            val applied = f.settings.copy(replaySeconds = 60)
            f.store.configureAuto(applied, 2, "fixture", true, f.at(10_030_000_000))
            assertEquals(before, f.store.autosave.snapshot())
            f.log(javaCrash(wall = CRASH_WALL + 10_050_000_000), 10_052_000_000)
            f.log(platformCrash(wall = CRASH_WALL + 10_051_000_000), 10_053_000_000)
            val next = f.store.autosave.drain(f.at(10_070_000_000)).single()
            assertEquals(AutoSaveReason.NOT_SAVED_COOLDOWN, f.store.autosave.admissionProblem(next, f.at(10_070_000_000)))
            assertEquals(7, f.store.autosave.snapshot().admissionsRemaining)
            assertEquals(before.warning, f.store.autosave.snapshot().warning)
        }
    }

    @Test
    fun staleReporterPreArmUnknownClockTargetChangeAndUncorroboratedReportsNeverPin() {
        for (condition in listOf("slow", "unfinished", "pre-arm", "boundary", "target", "uncorroborated", "native")) {
            CrashFixture(Files.createDirectory(root.resolve(condition))).use { f ->
                when (condition) {
                    "slow" -> f.reporter(7_000_000_000, 10_004_000_000)
                    "unfinished" -> f.reporter(10_000_000_000, 10_007_000_000)
                    "pre-arm" -> {
                        f.store.configureAuto(f.settings.copy(autoSaveOnCrash = false), 2, "fixture", true, f.at(10_010_000_000))
                        f.store.configureAuto(f.settings, 3, "fixture", true, f.at(10_011_000_000))
                    }
                }
                f.log(javaCrash(), if (condition == "pre-arm") 10_015_000_000 else 10_005_000_000)
                if (condition !in listOf("uncorroborated")) f.log(platformCrash(exception = if (condition == "native") "Native crash" else "java.lang.RuntimeException"),
                    if (condition == "pre-arm") 10_016_000_000 else 10_006_000_000)
                if (condition == "boundary") { f.store.clock.boundary(); f.store.configureAuto(f.settings, 1, "fixture", true, f.at(10_020_000_000)) }
                if (condition == "target") f.store.configureAuto(f.settings.copy(application = ApplicationTarget("com.example.other")), 2, "fixture", true, f.at(10_020_000_000))
                val pairs = f.store.autosave.drain(f.at(if (condition == "uncorroborated") 16_000_000_000 else 10_020_000_000))
                if (condition == "pre-arm") {
                    assertEquals(AutoSaveReason.NOT_SAVED_PRE_ARM_EVENT,
                        assertFailsWith<AutoSaveRejected> { f.store.capture(f.settings, pairs.single(), f.at(10_020_000_000)) }.reason)
                } else assertTrue(pairs.isEmpty(), condition)
                assertNotNull(f.store.capture(f.settings, host = f.at(10_020_000_000)))
            }
        }
    }
}

internal class CrashFixture(
    val root: Path,
    mode: CaptureMode = CaptureMode.LOG_ONLY,
    start: Long = 9_900_000_000,
    seconds: Int = 30,
    uid: Long = 10123,
    pid: Int = 4242,
    exclusive: Boolean = true,
    suppliedStore: CaptureStore? = null,
    val host: Long = System.nanoTime() + 1_000_000_000,
    revision: Long = 1,
) : AutoCloseable {
    val store = suppliedStore ?: CaptureStore(root.resolve("ring"), minFree = 0, captureMode = mode)
    val settings = ReplaySettings(saveDirectory = root, replaySeconds = seconds,
        application = ApplicationTarget("com.example.target"), captureMode = mode, autoSaveOnCrash = true)
    init {
        store.generation(1)
        anchor(0)
        store.app("com.example.target", uid, setOf(pid), 1, exclusive, at(1_000_000))
        store.configureAuto(settings, revision, "fixture", true, at(100_000_000))
        for (time in listOf(4L, 8)) anchor(time * 1_000_000_000)
        if (start >= 10_000_000_000) anchor(10_000_000_000)
        // Establish the actual ring start; ordinary rows are not detector candidates.
        log(wireLog(byteArrayOf(4, 84, 0, 120, 0), 0, pid, uid, CRASH_WALL + start), start + 1_000_000)
        if (start < 10_000_000_000) anchor(10_000_000_000)
        reporter(9_500_000_000, 9_501_000_000)
    }
    fun at(relative: Long): Long = host + relative
    fun anchor(relative: Long, sourceDelta: Long = 0) {
        val elapsed = 100_000_000_000 + relative + sourceDelta
        assertTrue(store.clock.add("7", listOf(elapsed, elapsed, CRASH_WALL + relative, elapsed),
            at(relative), at(relative + 1_000_000), CRASH_WALL + relative))
    }
    fun reporter(start: Long, end: Long) = store.reporter(ReporterPoll(at(start), at(end), 1, 500))
    fun log(source: DeviceLog, receipt: Long): String = assertNotNull(store.log(source, 1, at(receipt)))
    fun standard() { log(javaCrash(), 10_005_000_000); log(platformCrash(), 10_006_000_000) }
    fun pair(): CrashPair = store.autosave.drain(at(10_020_000_000)).single()
    fun video() {
        val picture = Picture.create(32, 32, ColorSpace.YUV420J).apply { fill(15) }
        val encoded = H264Encoder.createH264Encoder().encodeFrame(picture, ByteBuffer.allocate(65536)).data
        val bytes = ByteArray(encoded.remaining()).also { encoded.get(it) }
        val config = H264Utils.splitFrame(ByteBuffer.wrap(bytes)).filter { it.get(0).toInt() and 31 in setOf(7, 8) }
            .fold(byteArrayOf()) { acc, nal -> acc + byteArrayOf(0, 0, 0, 1) + ByteArray(nal.remaining()).also { nal.get(it) } }
        store.session(VideoPacket.Session(32, 32), 1)
        store.frame(VideoPacket.Frame(0, true, false, config), 1, at(9_900_000_000))
        for (time in listOf(9_900_000_000, 10_010_000_000))
            store.frame(VideoPacket.Frame((100_000_000_000 + time) / 1000, false, true, bytes), 1, at(time + 1_000_000))
    }
    override fun close() = store.close()
}
