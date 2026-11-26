package io.github.shinma06.arb.integration

import io.github.shinma06.arb.ipc.ArbPaths
import io.github.shinma06.arb.ipc.ArbdProcessManager
import io.github.shinma06.arb.ipc.BundleLeaseController
import io.github.shinma06.arb.model.ArbConfig
import io.github.shinma06.arb.model.ArbConfigPatch
import io.github.shinma06.arb.model.ArbdRpcException
import io.github.shinma06.arb.model.BundleFormat
import io.github.shinma06.arb.model.BundleLifecycleState
import io.github.shinma06.arb.model.DiscontinuityKind
import io.github.shinma06.arb.model.EditUpdateSessionParams
import io.github.shinma06.arb.model.ExportPngParams
import io.github.shinma06.arb.model.ExportShareMp4Params
import io.github.shinma06.arb.model.ExportTrimParams
import io.github.shinma06.arb.model.IPC_PROTOCOL_VERSION
import io.github.shinma06.arb.model.IdeDefaultSaveAction
import io.github.shinma06.arb.model.LeaseHolderKind
import io.github.shinma06.arb.model.OutputDirPatch
import io.github.shinma06.arb.model.Provenance
import io.github.shinma06.arb.model.QueryBucket
import io.github.shinma06.arb.model.RecordBucket
import io.github.shinma06.arb.model.SaveLogsParams
import io.github.shinma06.arb.model.SaveScreenshotParams
import io.github.shinma06.arb.playback.AudioSink
import io.github.shinma06.arb.playback.AudioSyncPlayer
import io.github.shinma06.arb.playback.FrameSeekResult
import io.github.shinma06.arb.playback.FrameSeeker
import io.github.shinma06.arb.playback.PcmFormat
import io.github.shinma06.arb.playback.PlaybackController
import io.github.shinma06.arb.playback.WavFilePcmSource
import io.github.shinma06.arb.shm.ShmFrameRingReader
import io.github.shinma06.arb.shm.ShmPixelFormat
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.Paths
import java.security.MessageDigest
import java.time.Duration
import java.util.HexFormat
import java.util.concurrent.TimeUnit
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertIs
import kotlin.test.assertNotNull
import kotlin.test.assertTrue

/**
 * Full headless Kotlin -> real `arbd` binary integration test over real local IPC (UDS) + SHM mmap.
 *
 * Exercises:
 *  - Lazy daemon spawn & `system.hello` / `system.status`
 *  - `bundle.open` against the real repository Legacy Schema-4 fixture AND a real WORM v2 bundle
 *  - `timeline.summary`, `timeline.discontinuities`, `timeline.query_logs` (`main` + `unanchored` separated!)
 *  - `media.open_viewport` -> `media.filmstrip` -> `media.scrub` (`isApproximate == true` via `ShmFrameRingReader`)
 *    -> `media.seek_frame` (`isApproximate == false` & exact `frameIndex` via `ShmFrameRingReader`)
 *  - `edit.update_session` / `edit.get_session`
 *  - `export.trim` (strict integer frame indices, zero `elst`, carries forward `unanchored` logs)
 *  - `export.png` (verifies PNG magic signature and dimensions)
 *  - `bundle.lease_renew`, `bundle.trash`, `bundle.gc` (lease protection!), `bundle.restore`, `bundle.close`
 *  - `media.close_viewport` & `system.shutdown`
 */
class ArbdRealIntegrationTest {
    private fun repoRoot(): Path {
        val prop = System.getProperty("arb.repo.root")
        if (!prop.isNullOrBlank()) {
            return Paths.get(prop).toAbsolutePath().normalize()
        }
        var cur: Path? = Paths.get("").toAbsolutePath().normalize()
        repeat(6) {
            val d = cur ?: error("Could not locate repository root")
            if (Files.isDirectory(d.resolve("core")) && Files.isDirectory(d.resolve("fixtures"))) {
                return d
            }
            cur = d.parent
        }
        error("Could not locate repository root containing core/ and fixtures/")
    }

    private fun sha256Hex(file: Path): String {
        val md = MessageDigest.getInstance("SHA-256")
        md.update(Files.readAllBytes(file))
        return HexFormat.of().formatHex(md.digest())
    }

    /**
     * Materializes a valid SHA-256-verified WORM v2 bundle (`manifest.json`, `video.mp4`, `timeline.sqlite`)
     * reusing the real H.264 `video.mp4` from the repository fixture, with 24 anchored frames,
     * separated `main` and `unanchored` logs, taps, and a `device_sleep` discontinuity.
     */
    private fun createValidV2Bundle(bundleDir: Path, bundleId: String, sourceMp4: Path) {
        Files.createDirectories(bundleDir)
        val videoDst = bundleDir.resolve("video.mp4")
        Files.copy(sourceMp4, videoDst)
        val dbPath = bundleDir.resolve("timeline.sqlite")

        val pyScript = """
            import sqlite3, sys
            db = sqlite3.connect(sys.argv[1])
            db.executescript('''
            PRAGMA journal_mode = DELETE;
            CREATE TABLE frames (
              frame_index INTEGER PRIMARY KEY,
              boot_id TEXT NOT NULL,
              pts_device_mono_ns INTEGER NOT NULL,
              aligned_boottime_ns INTEGER,
              half_width_ns INTEGER,
              certainty TEXT NOT NULL,
              is_keyframe INTEGER NOT NULL,
              gop_id INTEGER NOT NULL,
              mp4_sample_offset INTEGER NOT NULL,
              mp4_sample_size INTEGER NOT NULL,
              pc_ingest_cont_ns INTEGER NOT NULL DEFAULT 0,
              min_boottime_ns INTEGER,
              max_boottime_ns INTEGER,
              semantics TEXT,
              uncertainty_reason TEXT
            );
            CREATE TABLE discontinuities (
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              kind TEXT NOT NULL,
              stream TEXT,
              boot_id TEXT,
              start_boottime_ns INTEGER,
              end_boottime_ns INTEGER,
              pc_cont_start_ns INTEGER NOT NULL,
              pc_cont_end_ns INTEGER,
              detail_json TEXT
            );
            CREATE TABLE logs (
              seq INTEGER PRIMARY KEY,
              boot_id TEXT,
              header_epoch_ns INTEGER,
              device_read_boottime_ns INTEGER,
              pc_ingest_cont_ns INTEGER NOT NULL,
              aligned_boottime_ns INTEGER,
              min_boottime_ns INTEGER,
              max_boottime_ns INTEGER,
              certainty TEXT NOT NULL,
              bucket TEXT NOT NULL,
              level TEXT,
              tag TEXT,
              pid INTEGER,
              tid INTEGER,
              message TEXT NOT NULL,
              half_width_ns INTEGER,
              semantics TEXT,
              uncertainty_reason TEXT
            );
            CREATE TABLE taps (
              seq INTEGER PRIMARY KEY,
              boot_id TEXT,
              event_mono_ns INTEGER,
              pc_ingest_cont_ns INTEGER NOT NULL,
              aligned_boottime_ns INTEGER,
              certainty TEXT NOT NULL,
              bucket TEXT NOT NULL,
              action TEXT NOT NULL,
              x REAL,
              y REAL,
              pointer_id INTEGER,
              half_width_ns INTEGER,
              min_boottime_ns INTEGER,
              max_boottime_ns INTEGER,
              semantics TEXT,
              uncertainty_reason TEXT
            );
            CREATE TABLE clock_anchors (
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              kind TEXT NOT NULL,
              boot_id TEXT NOT NULL,
              payload_json TEXT NOT NULL
            );
            ''')
            base = 10_000_000_000
            step = 100_000_000
            for i in range(24):
                ts = base + i * step
                is_kf = 1 if i in (0, 12) else 0
                gop = 0 if i < 12 else 1
                db.execute(
                    'INSERT INTO frames VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)',
                    (i, 'boot-v2', ts, ts, 1_000_000, 'anchored', is_kf, gop, 0, 100, ts, None, None, 'video_encoder_input_estimate', None)
                )
            db.execute(
                'INSERT INTO discontinuities (kind, stream, boot_id, start_boottime_ns, end_boottime_ns, pc_cont_start_ns, pc_cont_end_ns, detail_json) VALUES (?,?,?,?,?,?,?,?)',
                ('device_sleep', None, 'boot-v2', base + 11 * step, base + 12 * step, base + 11 * step, base + 12 * step, '{"delta_ns":500000000,"detail":"deep sleep gap"}')
            )
            for idx, frame_i in enumerate((1, 5, 14, 20)):
                ts = base + frame_i * step
                db.execute(
                    'INSERT INTO logs VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)',
                    (idx + 1, 'boot-v2', 1730000000000000000 + ts, ts, ts, ts, None, None, 'anchored', 'main', 'I', 'ArbTest', 1001, 1002, f'main-log-at-frame-{frame_i}', 1_000_000, 'logcat_device_read_boottime', None)
                )
            db.execute(
                'INSERT INTO logs VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)',
                (99, None, None, None, base + 6 * step, None, None, None, 'unanchored', 'unanchored', 'W', 'ArbUnanchored', 1001, 1002, 'unanchored-quarantine-entry', None, None, 'clock_sync_stale')
            )
            db.execute(
                'INSERT INTO taps VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)',
                (1, 'boot-v2', base + 4 * step, base + 4 * step, base + 4 * step, 'anchored', 'main', 'DOWN', 32.0, 48.0, 0, 1_000_000, None, None, 'input_event_time', None)
            )
            db.commit()
            db.close()
        """.trimIndent()

        // Pass the script via a temp file, not `-c`: Windows command-line quoting strips
        // embedded double quotes from the argument, corrupting the SQL string literals.
        val scriptFile = Files.createTempFile("arb-bundle-seed", ".py")
        try {
            Files.writeString(scriptFile, pyScript)
            val proc = ProcessBuilder("python3", scriptFile.toString(), dbPath.toString())
                .redirectErrorStream(true)
                .start()
            val output = proc.inputStream.bufferedReader().readText()
            val exitCode = proc.waitFor()
            assertEquals(0, exitCode, "Failed to create test timeline.sqlite: $output")
        } finally {
            Files.deleteIfExists(scriptFile)
        }

        val videoBytes = Files.size(videoDst)
        val videoSha = sha256Hex(videoDst)
        val dbBytes = Files.size(dbPath)
        val dbSha = sha256Hex(dbPath)

        val manifestJson = """
            {
              "schema_version": 2,
              "bundle_id": "$bundleId",
              "created_at_utc": "2026-11-23T00:00:00Z",
              "provenance": { "kind": "original" },
              "device": {
                "serial": "emulator-5554",
                "model": "Pixel 9",
                "api_level": 37,
                "is_emulator": true
              },
              "requested_window": {
                "kind": "real_elapsed_sec",
                "duration_sec": 30,
                "trigger_pc_wall_utc": "2026-11-23T00:00:00Z"
              },
              "epochs": [
                {
                  "boot_id": "boot-v2",
                  "start_boottime_ns": 10000000000,
                  "end_boottime_ns": 12300000000
                }
              ],
              "streams": {
                "video": {
                  "status": "present",
                  "frame_count": 24,
                  "tail_may_be_incomplete": false,
                  "semantics": "video_encoder_input_estimate"
                },
                "audio": {
                  "status": "partial",
                  "av_offset_status": "uncalibrated",
                  "av_offset_ms": 0,
                  "tail_may_be_incomplete": false
                },
                "logcat": {
                  "status": "present",
                  "completeness": "unverified",
                  "overflow_events": 0,
                  "tail_may_be_incomplete": false
                },
                "input": {
                  "status": "present",
                  "tail_may_be_incomplete": false
                }
              },
              "unanchored_summary": {
                "included_count": 1,
                "truncated_count": 0
              },
              "files": [
                { "path": "video.mp4", "bytes": $videoBytes, "sha256": "$videoSha" },
                { "path": "timeline.sqlite", "bytes": $dbBytes, "sha256": "$dbSha" }
              ]
            }
        """.trimIndent()
        Files.writeString(bundleDir.resolve("manifest.json"), manifestJson)
    }

    @Test
    fun fullRoundTripAgainstRealArbdOverUdsAndShm(@TempDir tmp: Path) {
        val root = repoRoot()
        val legacyFixture = root
            .resolve("fixtures/legacy-schema4/replay-trim-parity/replay-20240101-000000-manual-feedface")
        assertTrue(Files.isDirectory(legacyFixture), "Legacy Schema-4 fixture missing at $legacyFixture")

        // Prepare a real WORM v2 bundle alongside the temp data dir
        val v2BundlePath = tmp.resolve("data").resolve("bundles").resolve("v2-kotlin-e2e")
        createValidV2Bundle(v2BundlePath, "v2-kotlin-e2e", legacyFixture.resolve("video.mp4"))

        val paths = ArbPaths(
            dataDir = tmp.resolve("data"),
            cacheDir = tmp.resolve("cache"),
            runtimeDir = tmp.resolve("run"),
        )
        val manager = ArbdProcessManager(paths = paths, searchRoot = root)
        val daemonBin = manager.resolveDaemonBinary()
        assertNotNull(
            daemonBin,
            "arbd binary not found. Run `cargo build --manifest-path core/Cargo.toml -p arbd` before running tests.",
        )

        val connected = manager.connectOrSpawn(Duration.ofSeconds(10))
        val proc = connected.spawnedProcess
        try {
            connected.client.use { rpc ->
                // 1. Handshake & status
                assertEquals(IPC_PROTOCOL_VERSION, connected.hello.protocolVersion)
                assertEquals(2, connected.hello.schemaVersion)
                assertTrue(connected.hello.pid > 0L)

                val status = rpc.systemStatus()
                assertEquals(connected.hello.pid, status.pid)
                assertEquals(0L, status.activeViewports)

                // 2. Open real repo Schema-4 legacy fixture (strictly read-only)
                val legacyOpen = rpc.bundleOpen(
                    bundleIdOrPath = legacyFixture.toString(),
                    holderKind = LeaseHolderKind.IDE_VIEWER,
                    holderPid = ProcessHandle.current().pid(),
                )
                assertEquals(BundleFormat.LEGACY_SCHEMA4, legacyOpen.format)
                assertTrue(legacyOpen.isLegacy)
                assertEquals(24, legacyOpen.summary.frameCount)
                assertTrue(legacyOpen.leaseId.isNotBlank())

                val legacySummary = rpc.timelineSummary(legacyOpen.bundleId)
                assertEquals(24, legacySummary.frameCount)
                val legacyUnanchoredLogs = rpc.timelineQueryLogs(legacyOpen.bundleId, bucket = QueryBucket.UNANCHORED).logs
                assertTrue(legacyUnanchoredLogs.any { it.message.contains("合成ログ 4") })
                assertTrue(rpc.timelineQueryTaps(legacyOpen.bundleId, bucket = QueryBucket.ALL).taps.isNotEmpty())

                // Open SHM viewport on Legacy Schema-4 fixture -> scrub (approx) -> seek_frame (exact)
                val legacyVp = rpc.mediaOpenViewport(legacyOpen.bundleId)
                assertEquals(24L, legacyVp.frameCount)
                assertEquals("rgba8", legacyVp.pixelFormat)
                ShmFrameRingReader.open(Paths.get(legacyVp.shmPath)).use { shm ->
                    val filmstrip = rpc.mediaFilmstrip(legacyVp.viewportId)
                    assertTrue(filmstrip.count >= 2, "Expected >= 2 keyframes in Schema-4 fixture")

                    val scrubResp = rpc.mediaScrub(legacyVp.viewportId, frameIndex = 5L)
                    assertTrue(scrubResp.isApproximate, "Mid-GOP cold scrub must report isApproximate=true")
                    val shmApprox = assertNotNull(shm.readLatest())
                    assertEquals(scrubResp.generation, shmApprox.meta.generation)
                    assertTrue(shmApprox.meta.isApproximate, "SHM slot must carry FLAG_IS_APPROXIMATE=true")
                    assertEquals(ShmPixelFormat.RGBA8, shmApprox.meta.pixelFormat)
                    assertTrue(shmApprox.pixels.isNotEmpty())
                    val approxImg = shmApprox.toBufferedImage()
                    assertEquals(shmApprox.meta.width, approxImg.width)

                    val seekResp = rpc.mediaSeekFrame(legacyVp.viewportId, frameIndex = 5L)
                    assertFalse(seekResp.isApproximate, "seek_frame must report isApproximate=false")
                    assertEquals(5L, seekResp.frameIndex)
                    val shmExact = assertNotNull(shm.readLatest())
                    assertEquals(seekResp.generation, shmExact.meta.generation)
                    assertEquals(5L, shmExact.meta.frameIndex)
                    assertFalse(shmExact.meta.isApproximate, "SHM slot must clear FLAG_IS_APPROXIMATE after seek_frame")
                    assertTrue(shmExact.pixels.isNotEmpty())
                    val exactImg = shmExact.toBufferedImage()
                    assertEquals(legacyVp.width, exactImg.width)
                    assertEquals(legacyVp.height, exactImg.height)
                }
                assertTrue(rpc.mediaCloseViewport(legacyVp.viewportId).closed)
                assertTrue(rpc.bundleClose(legacyOpen.leaseId).released)

                // 3. Open the real WORM v2 bundle -> verify main + unanchored logs, discontinuities, SHM, trim & PNG!
                val v2Open = rpc.bundleOpen(
                    bundleIdOrPath = v2BundlePath.toString(),
                    holderKind = LeaseHolderKind.IDE_VIEWER,
                    holderPid = ProcessHandle.current().pid(),
                )
                assertEquals(BundleFormat.V2, v2Open.format)
                assertFalse(v2Open.isLegacy)
                assertEquals(24, v2Open.summary.frameCount)
                assertEquals(Provenance.Original, v2Open.summary.provenance)

                val v2Discontinuities = rpc.timelineDiscontinuities(v2Open.bundleId).discontinuities
                assertEquals(1, v2Discontinuities.size)
                assertEquals(DiscontinuityKind.DEVICE_SLEEP, v2Discontinuities[0].kind)

                val v2MainLogs = rpc.timelineQueryLogs(v2Open.bundleId, bucket = QueryBucket.MAIN).logs
                val v2UnanchoredLogs = rpc.timelineQueryLogs(v2Open.bundleId, bucket = QueryBucket.UNANCHORED).logs
                assertEquals(4, v2MainLogs.size)
                assertTrue(v2MainLogs.all { it.bucket == RecordBucket.MAIN })
                assertEquals(1, v2UnanchoredLogs.size)
                assertEquals(RecordBucket.UNANCHORED, v2UnanchoredLogs[0].bucket)
                assertEquals("unanchored-quarantine-entry", v2UnanchoredLogs[0].message)

                // Edit session persistence
                val savedSession = rpc.editUpdateSession(
                    EditUpdateSessionParams(
                        sessionId = "ide-v2-session",
                        bundleId = v2Open.bundleId,
                        inFrameIndex = 2L,
                        outFrameIndex = 18L,
                        playheadFrameIndex = 7L,
                        avOffsetOverrideMs = -15L,
                    ),
                )
                assertEquals(savedSession, rpc.editGetSession("ide-v2-session"))

                // SHM Viewport on v2 bundle + BundleLeaseController heartbeat
                val v2Vp = rpc.mediaOpenViewport(v2Open.bundleId)
                assertEquals(24L, v2Vp.frameCount)
                val leaseCtrl = BundleLeaseController.forClient(
                    client = rpc,
                    leaseId = v2Open.leaseId,
                    viewportId = v2Vp.viewportId,
                    heartbeatInterval = Duration.ofMillis(100),
                )
                try {
                    ShmFrameRingReader.open(Paths.get(v2Vp.shmPath)).use { shm ->
                        rpc.mediaFilmstrip(v2Vp.viewportId)

                        val v2Scrub = rpc.mediaScrub(v2Vp.viewportId, frameIndex = 7L)
                        assertTrue(v2Scrub.isApproximate)
                        val shmScrub = assertNotNull(shm.readLatest())
                        assertTrue(shmScrub.meta.isApproximate)

                        val v2Seek = rpc.mediaSeekFrame(v2Vp.viewportId, frameIndex = 7L)
                        assertFalse(v2Seek.isApproximate)
                        assertEquals(7L, v2Seek.frameIndex)
                        val shmSeek = assertNotNull(shm.readLatest())
                        assertFalse(shmSeek.meta.isApproximate)
                        assertEquals(7L, shmSeek.meta.frameIndex)
                        assertTrue(shmSeek.pixels.isNotEmpty())
                        assertEquals(v2Vp.width, shmSeek.toBufferedImage().width)
                    }

                    // 4. Frame-exact mid-GOP export.trim [2..18] -> creates a new v2 trimmed bundle!
                    val trimResp = rpc.exportTrim(
                        ExportTrimParams(
                            bundleIdOrPath = v2Open.bundleId,
                            inFrameIndex = 2L,
                            outFrameIndex = 18L,
                            holderPid = ProcessHandle.current().pid(),
                        ),
                    )
                    assertEquals(17L, trimResp.trimReport.outputFrameCount)
                    assertEquals(0, trimResp.trimReport.edtsBoxCount)
                    assertEquals(0, trimResp.trimReport.elstBoxCount)
                    assertEquals(2, trimResp.mainLogCount, "Frames 5 and 14 logs sit inside [2..18]")
                    assertEquals(1, trimResp.unanchoredLogCount, "Trim must carry forward unanchored logs")
                    assertTrue(Files.isRegularFile(Paths.get(trimResp.bundlePath).resolve("manifest.json")))

                    val trimmedSummary = rpc.timelineSummary(trimResp.bundleId)
                    val trimmedProv = assertIs<Provenance.Trimmed>(trimmedSummary.provenance)
                    assertEquals(v2Open.bundleId, trimmedProv.parentBundleId)
                    assertEquals(2L, trimmedProv.inFrameIndex)
                    assertEquals(18L, trimmedProv.outFrameIndex)

                    // 5. Exact frame export.png -> verify real PNG file signature
                    val pngOut = tmp.resolve("v2-frame-7.png")
                    val pngResp = rpc.exportPng(
                        ExportPngParams(
                            bundleIdOrPath = v2Open.bundleId,
                            frameIndex = 7L,
                            outputPath = pngOut.toString(),
                        ),
                    )
                    assertEquals(7L, pngResp.report.frameIndex)
                    assertTrue(Files.isRegularFile(pngOut))
                    val pngBytes = Files.readAllBytes(pngOut)
                    assertTrue(pngBytes.size > 64)
                    assertContentEquals(
                        byteArrayOf(0x89.toByte(), 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A),
                        pngBytes.copyOfRange(0, 8),
                    )

                    // 6. Lease renew + Trash + GC protection while tab is open -> Restore!
                    assertTrue(rpc.bundleLeaseRenew(v2Open.leaseId).renewed)
                    assertEquals(BundleLifecycleState.TRASHED, rpc.bundleTrash(v2Open.bundleId).state)
                    val gcWhileLeased = rpc.bundleGc(nowUtc = "2099-01-01T00:00:00Z")
                    assertTrue(
                        gcWhileLeased.skippedLeasedBundleIds.contains(v2Open.bundleId),
                        "Active Viewer Lease must prevent GC from purging trashed bundle",
                    )
                    assertEquals(BundleLifecycleState.ACTIVE, rpc.bundleRestore(v2Open.bundleId).state)
                } finally {
                    // Closing tab releases both viewport and lease automatically
                    leaseCtrl.close()
                }

                // Verify bundle catalog lists Legacy (RO), original v2, and trimmed v2 bundles
                val listed = rpc.bundleList().bundles
                assertTrue(listed.any { it.bundleId == legacyOpen.bundleId })
                assertTrue(listed.any { it.bundleId == v2Open.bundleId })
                assertTrue(listed.size >= 3)

                // 7. Clean shutdown
                assertTrue(rpc.systemShutdown().ok)
            }
        } finally {
            if (proc != null) {
                if (!proc.waitFor(5, TimeUnit.SECONDS)) {
                    proc.destroyForcibly()
                }
            }
        }
    }

    @Test
    fun t1RpcsConfigCrashesScreensSaveLogsScreenshotAndT2bFallbackAgainstRealArbd(@TempDir tmp: Path) {
        val root = repoRoot()
        val legacyFixture = root
            .resolve("fixtures/legacy-schema4/replay-trim-parity/replay-20240101-000000-manual-feedface")
        assertTrue(Files.isDirectory(legacyFixture), "Legacy Schema-4 fixture missing at $legacyFixture")

        val v2BundlePath = tmp.resolve("data").resolve("bundles").resolve("v2-t1-analytics")
        createAnalyticsV2Bundle(v2BundlePath, "v2-t1-analytics", legacyFixture.resolve("video.mp4"))

        val paths = ArbPaths(
            dataDir = tmp.resolve("data"),
            cacheDir = tmp.resolve("cache"),
            runtimeDir = tmp.resolve("run"),
        )
        val manager = ArbdProcessManager(paths = paths, searchRoot = root)
        val connected = manager.connectOrSpawn(Duration.ofSeconds(10))
        val proc = connected.spawnedProcess
        try {
            connected.client.use { rpc ->
                // 1. config.get & config.update round-trip (including output_dir_override set & explicit null clear)
                val initialCfg = rpc.configGet()
                assertEquals(ArbConfig.DEFAULT_SAVE_WINDOW_SECS, initialCfg.config.saveWindowSecs)
                assertEquals(IdeDefaultSaveAction.SAVE_AND_OPEN, initialCfg.config.ideDefaultSaveAction)
                assertTrue(initialCfg.configPath.endsWith("config.json"))

                val overrideDir = tmp.resolve("custom-exports").toAbsolutePath().normalize()
                val updatedCfg = rpc.configUpdate(
                    ArbConfigPatch(
                        saveWindowSecs = 120,
                        ideDefaultSaveAction = IdeDefaultSaveAction.SAVE_ONLY,
                        crashAutosaveEnabled = false,
                        outputDirOverride = OutputDirPatch.Set(overrideDir.toString()),
                    ),
                )
                assertEquals(120, updatedCfg.config.saveWindowSecs)
                assertEquals(IdeDefaultSaveAction.SAVE_ONLY, updatedCfg.config.ideDefaultSaveAction)
                assertFalse(updatedCfg.config.crashAutosaveEnabled)
                assertEquals(overrideDir.toString(), updatedCfg.config.outputDirOverride)

                // Clearing output_dir_override via explicit JSON null
                val clearedCfg = rpc.configUpdate(ArbConfigPatch(outputDirOverride = OutputDirPatch.Clear))
                assertEquals(null, clearedCfg.config.outputDirOverride)
                assertEquals(120, clearedCfg.config.saveWindowSecs)

                // 2. Open analytics bundle -> timeline.crashes & timeline.screens round-trip!
                val opened = rpc.bundleOpen(
                    bundleIdOrPath = v2BundlePath.toString(),
                    holderKind = LeaseHolderKind.IDE_VIEWER,
                    holderPid = ProcessHandle.current().pid(),
                )
                try {
                    val crashes = rpc.timelineCrashes(opened.bundleId).crashes
                    assertEquals(1, crashes.size, "Expected 1 am_crash marker")
                    assertEquals("com.example.shop", crashes[0].packageName)
                    assertEquals("java.lang.IllegalStateException", crashes[0].exceptionClass)
                    assertNotNull(crashes[0].frameIndexHint)

                    val screens = rpc.timelineScreens(opened.bundleId).screens
                    assertEquals(2, screens.size, "Expected 2 contiguous Activity screen spans")
                    assertEquals("LoginActivity", screens[0].shortName)
                    assertEquals("CheckoutActivity", screens[1].shortName)
                    assertEquals("com.example.shop.ui.CheckoutActivity", screens[1].activityName)

                    // 3. Forward-compatible T-2B optional RPCs degrade cleanly when not yet exposed by arbd
                    val shareFallback = rpc.exportShareMp4OrNull(
                        ExportShareMp4Params(
                            bundleIdOrPath = opened.bundleId,
                            inFrameIndex = 0L,
                            outFrameIndex = 5L,
                        ),
                    )
                    // Returns null on pre-T-2B daemon (-32601) or a valid response once T-2B lands.
                    if (shareFallback != null) {
                        assertTrue(shareFallback.shareMp4Path.isNotBlank())
                    }
                    val audioFallback = rpc.mediaDecodeAudioPcmOrNull(opened.bundleId)
                    if (audioFallback != null) {
                        assertTrue(!audioFallback.available || !audioFallback.wavPath.isNullOrBlank())
                    }
                } finally {
                    assertTrue(rpc.bundleClose(opened.leaseId).released)
                }

                // 4. save.logs & save.screenshot wire contract against live arbd
                // Without an active capture session / connected ADB device, arbd returns a structured RPC error
                // (never a transport/serialization/protocol failure).
                val logsErr = runCatching { rpc.saveLogs(SaveLogsParams(windowSecs = 60L)) }.exceptionOrNull()
                if (logsErr != null) {
                    assertIs<ArbdRpcException>(logsErr)
                    assertFalse(logsErr.isMethodNotFound, "save.logs must be registered in arbd")
                }

                val shotErr = runCatching {
                    rpc.saveScreenshot(SaveScreenshotParams(serial = "nonexistent-test-serial-000"))
                }.exceptionOrNull()
                if (shotErr != null) {
                    assertIs<ArbdRpcException>(shotErr)
                    assertFalse(shotErr.isMethodNotFound, "save.screenshot must be registered in arbd")
                }

                assertTrue(rpc.systemShutdown().ok)
            }
        } finally {
            if (proc != null) {
                if (!proc.waitFor(5, TimeUnit.SECONDS)) {
                    proc.destroyForcibly()
                }
            }
        }
    }

    @Test
    fun t2bShareMp4WithTrashOriginalAndDecodeAudioPcmToWavPcmSource(@TempDir tmp: Path) {
        val root = repoRoot()
        val legacyFixture = root
            .resolve("fixtures/legacy-schema4/replay-trim-parity/replay-20240101-000000-manual-feedface")
        val audioFixture = root.resolve("fixtures/aac-playback-sync/stereo-44100.m4a")
        assertTrue(Files.isDirectory(legacyFixture), "Legacy Schema-4 fixture missing at $legacyFixture")
        assertTrue(Files.isRegularFile(audioFixture), "AAC-LC fixture missing at $audioFixture")

        val v2AvBundlePath = tmp.resolve("data").resolve("bundles").resolve("v2-t2b-av")
        createValidV2BundleWithAudio(
            bundleDir = v2AvBundlePath,
            bundleId = "v2-t2b-av",
            sourceMp4 = legacyFixture.resolve("video.mp4"),
            sourceM4a = audioFixture,
            avOffsetMs = 42L,
        )
        val v2VideoOnlyPath = tmp.resolve("data").resolve("bundles").resolve("v2-t2b-no-audio")
        createValidV2Bundle(v2VideoOnlyPath, "v2-t2b-no-audio", legacyFixture.resolve("video.mp4"))

        val paths = ArbPaths(
            dataDir = tmp.resolve("data"),
            cacheDir = tmp.resolve("cache"),
            runtimeDir = tmp.resolve("run"),
        )
        val manager = ArbdProcessManager(paths = paths, searchRoot = root)
        val connected = manager.connectOrSpawn(Duration.ofSeconds(10))
        val proc = connected.spawnedProcess
        try {
            connected.client.use { rpc ->
                val opened = rpc.bundleOpen(
                    bundleIdOrPath = v2AvBundlePath.toString(),
                    holderKind = LeaseHolderKind.IDE_VIEWER,
                    holderPid = ProcessHandle.current().pid(),
                )
                try {
                    // 1. media.decode_audio_pcm decodes real AAC-LC -> cached s16le RIFF/WAV
                    val decoded = rpc.mediaDecodeAudioPcm(opened.bundleId)
                    assertTrue(decoded.available, "audio track must decode cleanly")
                    assertFalse(decoded.cached, "first decode should populate cache")
                    assertEquals(44_100, decoded.sampleRateHz)
                    assertEquals(2, decoded.channels)
                    assertEquals(42L, decoded.avOffsetMs)
                    assertTrue(decoded.durationNs > 0L)
                    val wavPath = Paths.get(assertNotNull(decoded.wavPath))
                    assertTrue(Files.isRegularFile(wavPath), "decoded WAV must exist on disk")

                    // Second call hits cache
                    val cachedResp = rpc.mediaDecodeAudioPcm(opened.bundleId)
                    assertTrue(cachedResp.available)
                    assertTrue(cachedResp.cached, "second call must hit WAV cache")
                    assertEquals(decoded.wavPath, cachedResp.wavPath)

                    // 2. Wire returned WAV into T-3 WavFilePcmSource -> AudioSyncPlayer -> PlaybackController!
                    val fakeSink = object : AudioSink {
                        override val isAvailable: Boolean = true
                        var bytesWritten: Int = 0
                        override fun open(format: PcmFormat): Boolean = true
                        override fun start() = Unit
                        override fun stop() = Unit
                        override fun flush() = Unit
                        override fun write(pcm: ByteArray, offset: Int, length: Int): Int {
                            bytesWritten += length
                            return length
                        }
                        override fun close() = Unit
                    }
                    WavFilePcmSource.open(wavPath).use { pcmSource ->
                        assertEquals(44_100, pcmSource.sampleRateHz)
                        assertEquals(2, pcmSource.channels)
                        val slice = pcmSource.readPcmSlice(0L, 100_000_000L)
                        assertTrue(slice.isNotEmpty(), "decoded WAV must yield real PCM samples")

                        val seeker = object : FrameSeeker {
                            override fun seekExact(frameIndex: Long) = FrameSeekResult(frameIndex, frameIndex, false)
                            override fun scrubApprox(frameIndex: Long) = FrameSeekResult(frameIndex, frameIndex, true)
                        }
                        val controller = PlaybackController(
                            totalFrames = 24L,
                            frameSeeker = seeker,
                            autoRunLoop = false,
                        )
                        val audioPlayer = AudioSyncPlayer(
                            source = pcmSource,
                            sink = fakeSink,
                            avOffsetMs = decoded.avOffsetMs,
                            autoRunPump = false,
                            closeSourceOnClose = false,
                        )
                        try {
                            controller.attachAudioPlayer(audioPlayer, decoded.avOffsetMs)
                            assertEquals(42L, controller.state.avOffsetMs)
                            controller.play()
                            val written = audioPlayer.pumpNextSlice(50_000_000L)
                            assertTrue(written > 0 && fakeSink.bytesWritten > 0, "AudioSyncPlayer must stream decoded WAV PCM")
                        } finally {
                            controller.close()
                        }
                    }

                    // 3. Video-only bundle degrades cleanly (`available == false`)
                    val noAudioOpen = rpc.bundleOpen(
                        bundleIdOrPath = v2VideoOnlyPath.toString(),
                        holderKind = LeaseHolderKind.IDE_VIEWER,
                        holderPid = ProcessHandle.current().pid(),
                    )
                    try {
                        val noAudio = rpc.mediaDecodeAudioPcm(noAudioOpen.bundleId)
                        assertFalse(noAudio.available)
                        assertEquals(null, noAudio.wavPath)
                    } finally {
                        assertTrue(rpc.bundleClose(noAudioOpen.leaseId).released)
                    }

                    // 4. export.share_mp4 (trimmed [2..18] + audio + trash_original=true on v2 bundle!)
                    val shareOut = tmp.resolve("exports").resolve("trimmed-share.mp4")
                    val shareResp = rpc.exportShareMp4(
                        ExportShareMp4Params(
                            bundleIdOrPath = opened.bundleId,
                            inFrameIndex = 2L,
                            outFrameIndex = 18L,
                            outputPath = shareOut.toString(),
                            keepBundle = true,
                            trashOriginal = true,
                            holderPid = ProcessHandle.current().pid(),
                        ),
                    )
                    assertTrue(Files.isRegularFile(Paths.get(shareResp.shareMp4Path)))
                    assertTrue(shareResp.audioIncluded, "share.mp4 must mux AAC-LC audio track")
                    assertEquals(17L, shareResp.videoFrameCount)
                    assertEquals(0, shareResp.edtsBoxCount)
                    assertEquals(0, shareResp.elstBoxCount)
                    assertTrue(shareResp.outputBytes > 0L)
                    assertNotNull(shareResp.trimmedBundleId, "narrowed range must produce a trimmed WORM bundle")
                    assertTrue(shareResp.originalTrashed, "v2 source bundle must be logically trashed after commit")

                    val trashedEntry = rpc.bundleList().bundles.first { it.bundleId == opened.bundleId }
                    assertEquals(BundleLifecycleState.TRASHED, trashedEntry.state)
                    assertEquals(BundleLifecycleState.ACTIVE, rpc.bundleRestore(opened.bundleId).state)
                } finally {
                    assertTrue(rpc.bundleClose(opened.leaseId).released)
                }

                // 5. Legacy (RO) bundle must refuse trash_original=true!
                val legacyOpen = rpc.bundleOpen(
                    bundleIdOrPath = legacyFixture.toString(),
                    holderKind = LeaseHolderKind.IDE_VIEWER,
                    holderPid = ProcessHandle.current().pid(),
                )
                try {
                    val legacyTrashErr = runCatching {
                        rpc.exportShareMp4(
                            ExportShareMp4Params(
                                bundleIdOrPath = legacyOpen.bundleId,
                                outputPath = tmp.resolve("exports").resolve("legacy-refused.mp4").toString(),
                                trashOriginal = true,
                                holderPid = ProcessHandle.current().pid(),
                            ),
                        )
                    }.exceptionOrNull()
                    assertIs<ArbdRpcException>(legacyTrashErr)
                    assertFalse(legacyTrashErr.isMethodNotFound)
                } finally {
                    assertTrue(rpc.bundleClose(legacyOpen.leaseId).released)
                }

                assertTrue(rpc.systemShutdown().ok)
            }
        } finally {
            if (proc != null) {
                if (!proc.waitFor(5, TimeUnit.SECONDS)) {
                    proc.destroyForcibly()
                }
            }
        }
    }

    private fun createValidV2BundleWithAudio(
        bundleDir: Path,
        bundleId: String,
        sourceMp4: Path,
        sourceM4a: Path,
        avOffsetMs: Long,
    ) {
        createValidV2Bundle(bundleDir, bundleId, sourceMp4)
        val audioDst = bundleDir.resolve("audio.m4a")
        Files.copy(sourceM4a, audioDst)

        val videoDst = bundleDir.resolve("video.mp4")
        val dbPath = bundleDir.resolve("timeline.sqlite")
        val videoBytes = Files.size(videoDst)
        val videoSha = sha256Hex(videoDst)
        val audioBytes = Files.size(audioDst)
        val audioSha = sha256Hex(audioDst)
        val dbBytes = Files.size(dbPath)
        val dbSha = sha256Hex(dbPath)

        val manifestJson = """
            {
              "schema_version": 2,
              "bundle_id": "$bundleId",
              "created_at_utc": "2026-11-23T00:00:00Z",
              "provenance": { "kind": "original" },
              "device": {
                "serial": "emulator-5554",
                "model": "Pixel 9",
                "api_level": 37,
                "is_emulator": true
              },
              "requested_window": {
                "kind": "real_elapsed_sec",
                "duration_sec": 30,
                "trigger_pc_wall_utc": "2026-11-23T00:00:00Z"
              },
              "epochs": [
                {
                  "boot_id": "boot-v2",
                  "start_boottime_ns": 10000000000,
                  "end_boottime_ns": 12300000000
                }
              ],
              "streams": {
                "video": {
                  "status": "present",
                  "frame_count": 24,
                  "tail_may_be_incomplete": false,
                  "semantics": "video_encoder_input_estimate"
                },
                "audio": {
                  "status": "present",
                  "av_offset_status": "calibrated",
                  "av_offset_ms": $avOffsetMs,
                  "tail_may_be_incomplete": false
                },
                "logcat": {
                  "status": "present",
                  "completeness": "verified_no_overflow",
                  "overflow_events": 0,
                  "tail_may_be_incomplete": false
                },
                "input": {
                  "status": "present",
                  "tail_may_be_incomplete": false
                }
              },
              "unanchored_summary": {
                "included_count": 1,
                "truncated_count": 0
              },
              "files": [
                { "path": "video.mp4", "bytes": $videoBytes, "sha256": "$videoSha" },
                { "path": "audio.m4a", "bytes": $audioBytes, "sha256": "$audioSha" },
                { "path": "timeline.sqlite", "bytes": $dbBytes, "sha256": "$dbSha" }
              ]
            }
        """.trimIndent()
        Files.writeString(bundleDir.resolve("manifest.json"), manifestJson)
    }

    private fun createAnalyticsV2Bundle(bundleDir: Path, bundleId: String, sourceMp4: Path) {
        Files.createDirectories(bundleDir)
        val videoDst = bundleDir.resolve("video.mp4")
        Files.copy(sourceMp4, videoDst)
        val dbPath = bundleDir.resolve("timeline.sqlite")

        val pyScript = """
            import sqlite3, sys
            db = sqlite3.connect(sys.argv[1])
            db.executescript('''
            PRAGMA journal_mode = DELETE;
            CREATE TABLE frames (
              frame_index INTEGER PRIMARY KEY,
              boot_id TEXT NOT NULL,
              pts_device_mono_ns INTEGER NOT NULL,
              aligned_boottime_ns INTEGER,
              half_width_ns INTEGER,
              certainty TEXT NOT NULL,
              is_keyframe INTEGER NOT NULL,
              gop_id INTEGER NOT NULL,
              mp4_sample_offset INTEGER NOT NULL,
              mp4_sample_size INTEGER NOT NULL,
              pc_ingest_cont_ns INTEGER NOT NULL DEFAULT 0,
              min_boottime_ns INTEGER,
              max_boottime_ns INTEGER,
              semantics TEXT,
              uncertainty_reason TEXT
            );
            CREATE TABLE discontinuities (
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              kind TEXT NOT NULL,
              stream TEXT,
              boot_id TEXT,
              start_boottime_ns INTEGER,
              end_boottime_ns INTEGER,
              pc_cont_start_ns INTEGER NOT NULL,
              pc_cont_end_ns INTEGER,
              detail_json TEXT
            );
            CREATE TABLE logs (
              seq INTEGER PRIMARY KEY,
              boot_id TEXT,
              header_epoch_ns INTEGER,
              device_read_boottime_ns INTEGER,
              pc_ingest_cont_ns INTEGER NOT NULL,
              aligned_boottime_ns INTEGER,
              min_boottime_ns INTEGER,
              max_boottime_ns INTEGER,
              certainty TEXT NOT NULL,
              bucket TEXT NOT NULL,
              level TEXT,
              tag TEXT,
              pid INTEGER,
              tid INTEGER,
              message TEXT NOT NULL,
              half_width_ns INTEGER,
              semantics TEXT,
              uncertainty_reason TEXT
            );
            CREATE TABLE taps (
              seq INTEGER PRIMARY KEY,
              boot_id TEXT,
              event_mono_ns INTEGER,
              pc_ingest_cont_ns INTEGER NOT NULL,
              aligned_boottime_ns INTEGER,
              certainty TEXT NOT NULL,
              bucket TEXT NOT NULL,
              action TEXT NOT NULL,
              x REAL,
              y REAL,
              pointer_id INTEGER,
              half_width_ns INTEGER,
              min_boottime_ns INTEGER,
              max_boottime_ns INTEGER,
              semantics TEXT,
              uncertainty_reason TEXT
            );
            CREATE TABLE clock_anchors (
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              kind TEXT NOT NULL,
              boot_id TEXT NOT NULL,
              payload_json TEXT NOT NULL
            );
            ''')
            base = 10_000_000_000
            step = 100_000_000
            for i in range(24):
                ts = base + i * step
                is_kf = 1 if i in (0, 12) else 0
                gop = 0 if i < 12 else 1
                db.execute(
                    'INSERT INTO frames VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)',
                    (i, 'boot-t1', ts, ts, 1_000_000, 'anchored', is_kf, gop, 0, 100, ts, None, None, 'video_encoder_input_estimate', None)
                )
            # Screen 1: LoginActivity at frame 1
            db.execute(
                'INSERT INTO logs VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)',
                (1, 'boot-t1', 1730000000000000000 + base + step, base + step, base + step, base + step, None, None, 'anchored', 'main', 'I', 'wm_on_resume_called', 2000, 2000, '[123,com.example.shop/.ui.LoginActivity,RESUME_ACTIVITY]', 1_000_000, 'logcat_device_read_boottime', None)
            )
            # Screen 2: CheckoutActivity at frame 10
            db.execute(
                'INSERT INTO logs VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)',
                (2, 'boot-t1', 1730000000000000000 + base + 10 * step, base + 10 * step, base + 10 * step, base + 10 * step, None, None, 'anchored', 'main', 'I', 'ActivityTaskManager', 1000, 1000, 'Displayed com.example.shop/.ui.CheckoutActivity: +84ms', 1_000_000, 'logcat_device_read_boottime', None)
            )
            # Fatal am_crash at frame 16
            db.execute(
                'INSERT INTO logs VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)',
                (3, 'boot-t1', 1730000000000000000 + base + 16 * step, base + 16 * step, base + 16 * step, base + 16 * step, None, None, 'anchored', 'main', 'E', 'am_crash', 1000, 1000, '[0,2000,com.example.shop,891234,java.lang.IllegalStateException,Cart empty,CheckoutViewModel.kt,84,0]', 1_000_000, 'logcat_device_read_boottime', None)
            )
            db.commit()
            db.close()
        """.trimIndent()

        val scriptFile = Files.createTempFile("arb-t1-analytics-seed", ".py")
        try {
            Files.writeString(scriptFile, pyScript)
            val proc = ProcessBuilder("python3", scriptFile.toString(), dbPath.toString())
                .redirectErrorStream(true)
                .start()
            val output = proc.inputStream.bufferedReader().readText()
            val exitCode = proc.waitFor()
            assertEquals(0, exitCode, "Failed to create analytics timeline.sqlite: $output")
        } finally {
            Files.deleteIfExists(scriptFile)
        }

        val videoBytes = Files.size(videoDst)
        val videoSha = sha256Hex(videoDst)
        val dbBytes = Files.size(dbPath)
        val dbSha = sha256Hex(dbPath)

        val manifestJson = """
            {
              "schema_version": 2,
              "bundle_id": "$bundleId",
              "created_at_utc": "2026-11-23T00:00:00Z",
              "provenance": { "kind": "original" },
              "device": {
                "serial": "emulator-5554",
                "model": "Pixel 9",
                "api_level": 37,
                "is_emulator": true
              },
              "requested_window": {
                "kind": "real_elapsed_sec",
                "duration_sec": 300,
                "trigger_pc_wall_utc": "2026-11-23T00:00:00Z"
              },
              "epochs": [
                {
                  "boot_id": "boot-t1",
                  "start_boottime_ns": 10000000000,
                  "end_boottime_ns": 12300000000
                }
              ],
              "streams": {
                "video": {
                  "status": "present",
                  "frame_count": 24,
                  "tail_may_be_incomplete": false,
                  "semantics": "video_encoder_input_estimate"
                },
                "logcat": {
                  "status": "present",
                  "completeness": "verified_no_overflow",
                  "overflow_events": 0,
                  "tail_may_be_incomplete": false
                }
              },
              "unanchored_summary": {
                "included_count": 0,
                "truncated_count": 0
              },
              "files": [
                { "path": "video.mp4", "bytes": $videoBytes, "sha256": "$videoSha" },
                { "path": "timeline.sqlite", "bytes": $dbBytes, "sha256": "$dbSha" }
              ]
            }
        """.trimIndent()
        Files.writeString(bundleDir.resolve("manifest.json"), manifestJson)
    }
}
