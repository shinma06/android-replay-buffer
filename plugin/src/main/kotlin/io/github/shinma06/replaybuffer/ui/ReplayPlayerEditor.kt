package io.github.shinma06.replaybuffer.ui

import androidx.compose.foundation.focusable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.toComposeImageBitmap
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.isAltPressed
import androidx.compose.ui.input.key.isCtrlPressed
import androidx.compose.ui.input.key.isMetaPressed
import androidx.compose.ui.input.key.key
import androidx.compose.ui.input.key.onPreviewKeyEvent
import androidx.compose.ui.input.key.type
import androidx.compose.ui.input.pointer.PointerEventType
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.DpSize
import com.intellij.openapi.Disposable
import com.intellij.openapi.application.PathManager
import com.intellij.openapi.fileEditor.FileEditor
import com.intellij.openapi.fileEditor.FileEditorLocation
import com.intellij.openapi.fileEditor.FileEditorPolicy
import com.intellij.openapi.fileEditor.FileEditorProvider
import com.intellij.openapi.fileEditor.FileEditorState
import com.intellij.openapi.project.DumbAware
import com.intellij.openapi.project.Project
import com.intellij.openapi.util.Disposer
import com.intellij.openapi.util.UserDataHolderBase
import com.intellij.openapi.vfs.VirtualFile
import io.github.shinma06.replaybuffer.core.ReplayFrameCursor
import io.github.shinma06.replaybuffer.core.ReplayFrameExport
import io.github.shinma06.replaybuffer.core.ReplayFrameCache
import io.github.shinma06.replaybuffer.core.ReplayVideoDecoder
import io.github.shinma06.replaybuffer.core.ReplayThumbnailDiskCache
import io.github.shinma06.replaybuffer.core.previewKeyframeUs
import io.github.shinma06.replaybuffer.core.ReplayTimeline
import io.github.shinma06.replaybuffer.core.SavedReplay
import io.github.shinma06.replaybuffer.ide.ReplayProjectService
import org.jetbrains.jewel.bridge.JewelComposePanel
import org.jetbrains.jewel.foundation.theme.JewelTheme
import org.jetbrains.jewel.ui.component.DefaultButton
import org.jetbrains.jewel.ui.component.OutlinedButton
import org.jetbrains.jewel.ui.component.Text
import org.jetbrains.jewel.ui.component.styling.ButtonMetrics
import org.jetbrains.jewel.ui.component.styling.ButtonStyle
import org.jetbrains.jewel.ui.theme.defaultButtonStyle
import org.jetbrains.jewel.ui.theme.outlinedButtonStyle
import java.awt.BorderLayout
import java.awt.RenderingHints
import java.awt.image.BufferedImage
import java.beans.PropertyChangeListener
import java.nio.file.Path
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference
import javax.swing.JPanel
import javax.swing.SwingUtilities

internal const val REPLAY_PLAYER_EDITOR_ID = "android-replay-buffer-player"

/** Opens a completed save's video.mp4 in the replay player instead of a default editor. */
internal class ReplayPlayerEditorProvider : FileEditorProvider, DumbAware {
    override fun accept(project: Project, file: VirtualFile): Boolean = isReplayVideo(file)

    override fun createEditor(project: Project, file: VirtualFile): FileEditor {
        replayVideoOpening(file)
        return ReplayPlayerEditor(project, file)
    }

    override fun getEditorTypeId(): String = REPLAY_PLAYER_EDITOR_ID

    override fun getPolicy(): FileEditorPolicy = FileEditorPolicy.PLACE_BEFORE_DEFAULT_EDITOR
}

/** Saved-replay player with QuickTime-style trim (requirements video-trim.md 3.1 / 3.2). */
internal class ReplayPlayerEditor(project: Project, private val file: VirtualFile) : UserDataHolderBase(), FileEditor {
    internal val controller = ReplayPlayerController(project,
        checkNotNull(file.parent).toNioPath().toAbsolutePath().normalize())

    private val component = JPanel(BorderLayout()).apply {
        add(JewelComposePanel { ReplayPlayerContent(controller) }, BorderLayout.CENTER)
    }

    init { Disposer.register(this, controller) }

    override fun getComponent() = component
    override fun getPreferredFocusedComponent() = component.getComponent(0) as? javax.swing.JComponent ?: component
    override fun getName() = "動画の編集"
    override fun setState(state: FileEditorState) {}
    override fun isModified() = false
    override fun isValid() = true
    override fun addPropertyChangeListener(listener: PropertyChangeListener) {}
    override fun removePropertyChangeListener(listener: PropertyChangeListener) {}
    override fun getCurrentLocation(): FileEditorLocation? = null
    override fun getFile(): VirtualFile = file
    override fun dispose() {}
}

internal data class PlayerState(
    val loading: Boolean = true,
    val error: String? = null,
    val durationUs: Long = 0,
    val image: ImageBitmap? = null,
    /** Movie time of the frame in [image]. */
    val imageUs: Long = -1,
    val aspect: Float = 9f / 16f,
    val playing: Boolean = false,
    val message: String? = null,
    val audioNote: String? = null,
    val timeline: ReplayTimeline? = null,
    val editor: TrimEditor = TrimEditor(),
    val thumbnailVersion: Int = 0,
)

private class PlayerClosed : RuntimeException()
private class PreviewSuperseded : RuntimeException()

internal class ReplayPlaybackFrames<T : Any>(private val publish: (() -> Unit) -> Unit) {
    private var current: AtomicReference<T?>? = null

    fun session(consume: (T) -> Unit): (T) -> Unit {
        val pending = AtomicReference<T?>().also { current = it }
        return { frame ->
            if (pending.getAndSet(frame) == null) publish {
                pending.getAndSet(null)?.let(consume)
            }
        }
    }

    fun clear() { current?.set(null); current = null }
}

/** Owns the opened replay, the coalescing preview decoder, thumbnails, playback and trim/PNG tasks. */
internal class ReplayPlayerController(
    project: Project,
    internal val folder: Path,
    openReplay: (Path) -> SavedReplay = { SavedReplay.open(it) },
    private val decoderFactory: (SavedReplay, ReplayFrameCache) -> ReplayVideoDecoder = { replay, cache -> ReplayFrameCursor(replay, cache = cache) },
) : Disposable, PlayerBarActions {
    internal val closePolicy = ReplayEditClosePolicy()
    internal val resourcesClosed = java.util.concurrent.CompletableFuture<Boolean>()
    private val service = project.getService(ReplayProjectService::class.java)
    private val executor = Executors.newSingleThreadExecutor { runnable ->
        Thread(runnable, "arb-replay-player").apply { isDaemon = true }
    }
    private val thumbnailExecutor = Executors.newFixedThreadPool(2) { runnable ->
        Thread(runnable, "arb-replay-thumbnails").apply { isDaemon = true; priority = Thread.MIN_PRIORITY }
    }
    private val decodeLock = Any()
    private var pendingUs: Long? = null
    private var decodeBusy = false
    @Volatile private var previewGeneration = 0L
    @Volatile private var foregroundWork = false
    @Volatile private var playbackActive = false
    @Volatile private var scrubbing = false
    private val frameCache = ReplayFrameCache()
    private var decoder: ReplayVideoDecoder? = null // Confined to executor.
    private val diskThumbnails by lazy { ReplayThumbnailDiskCache(Path.of(PathManager.getSystemPath()).toAbsolutePath().normalize()) }
    @Volatile private var disposed = false
    @Volatile private var saved: SavedReplay? = null
    private var rules: TrimRules? = null
    private var playback: ReplayPlaybackSession? = null
    private val playbackClosures = java.util.concurrent.ConcurrentHashMap.newKeySet<java.util.concurrent.CompletableFuture<Boolean>>()
    private val playbackCleanupFailed = AtomicBoolean()
    private val playedFrames = ReplayPlaybackFrames<Pair<Long, ImageBitmap?>>(::publish)
    private var taskCancel: AtomicBoolean? = null
    private val thumbnails = ThumbnailCache<ImageBitmap>(maxBytes = 16L * 1024 * 1024) { it.width.toLong() * it.height * 4 }
    private val thumbnailGeneration = AtomicInteger()
    private var thumbnailSlots: List<Long?> = emptyList()

    var state by mutableStateOf(PlayerState())
        private set

    val folderName: String get() = folder.fileName?.toString() ?: folder.toString()

    init {
        replayVideoOpenRequested(folder) {
            if (!disposed) executor.execute { loadReplay(openReplay) }
        }.exceptionally { failure ->
            recordReadFailure(failure)
            publish { state = state.copy(loading = false, error = failure.message ?: "保存フォルダを開けません。") }
            null
        }
    }

    private fun loadReplay(openReplay: (Path) -> SavedReplay) {
        if (disposed) return
        try {
            val opened = openReplay(folder)
            val timeline = try { opened.previewKeyframes.at(0); opened.timeline() }
            catch (failure: Throwable) { opened.close(); throw failure }
            saved = opened
            publish {
                val created = TrimRules(FrameGrid(timeline.spans), timeline.durationUs)
                rules = created
                state = state.copy(loading = false, durationUs = timeline.durationUs, timeline = timeline,
                    editor = created.initial())
                showEditor()
            }
        } catch (failure: Throwable) {
            recordReadFailure(failure)
            publish { state = state.copy(loading = false, error = failure.message ?: "保存フォルダを開けません。") }
        }
    }

    private fun recordReadFailure(failure: Throwable) {
        val cancelled = failure is PreviewSuperseded || failure is PlayerClosed ||
            failure is InterruptedException || failure is java.nio.channels.ClosedByInterruptException
        if (!cancelled || failure.suppressed.isNotEmpty()) closePolicy.recordSourceReadFailure()
    }

    private fun publish(action: () -> Unit) {
        if (SwingUtilities.isEventDispatchThread()) { if (!disposed) action() }
        else SwingUtilities.invokeLater { if (!disposed) action() }
    }

    /** Applies an editor transition and decodes the frame it wants on screen when that changed. */
    private fun edit(transform: (TrimRules, TrimEditor) -> TrimEditor) {
        val current = rules ?: return
        if (disposed) return
        state = state.copy(editor = transform(current, state.editor))
        showEditor()
    }

    private fun showEditor() {
        val current = rules ?: return
        if (state.playing) return
        val exactUs = current.displayUs(state.editor)
        scrubbing = state.editor.drag != null
        val us = if (state.editor.drag != null) saved?.let { previewKeyframeUs(it, exactUs) } else exactUs
        if (us == null || current.grid.indexAt(us) < 0) {
            synchronized(decodeLock) { pendingUs = null; previewGeneration++ }
            state = state.copy(image = null, imageUs = -1)
        } else if (us != state.imageUs) requestDecode(us)
        else synchronized(decodeLock) { pendingUs = null; previewGeneration++ }
    }

    private fun requestDecode(us: Long) {
        val run = synchronized(decodeLock) {
            pendingUs = us
            previewGeneration++
            foregroundWork = true
            if (!decodeBusy) { decodeBusy = true; true } else false
        }
        if (run) executor.execute(::drainDecode)
    }

    /** Decodes only the newest requested position; stale requests are skipped, not queued. */
    private fun drainDecode() {
        try {
            while (!disposed) {
                val (target, generation) = synchronized(decodeLock) { pendingUs?.let { it to previewGeneration } } ?: break
                val replay = saved ?: break
                fun active() {
                    if (disposed) throw PlayerClosed()
                    if (previewGeneration != generation || playbackActive) throw PreviewSuperseded()
                }
                try {
                    active()
                    val cursor = decoder ?: decoderFactory(replay, frameCache).also { decoder = it }
                    val frame = cursor.seek(target, ::active)
                    active()
                    val bitmap = frame.image.previewBitmap()
                    val aspect = frame.image.width.toFloat() / frame.image.height
                    synchronized(decodeLock) { if (pendingUs == target && previewGeneration == generation) pendingUs = null }
                    publish {
                        val wanted = rules?.displayUs(state.editor)?.let { exact ->
                            if (state.editor.drag != null) previewKeyframeUs(replay, exact) else exact
                        }
                        if (!state.playing && wanted == target && previewGeneration == generation)
                            state = state.copy(image = bitmap, imageUs = target, aspect = aspect)
                    }
                    // Cache the two following positions after publishing. New input cancels at packet boundaries.
                    if (!scrubbing) repeat(2) { active(); cursor.next(::active) }
                } catch (failure: PreviewSuperseded) {
                    recordReadFailure(failure)
                    // A cancelled GOP is never published; the newest exact request goes next.
                } catch (failure: PlayerClosed) {
                    recordReadFailure(failure)
                    return
                } catch (failure: Throwable) {
                    recordReadFailure(failure) // Before EDT publication, including after disposal.
                    // A failed decoder may have consumed a partial packet; never reuse that reference state.
                    runCatching { decoder?.close() }.onFailure { closePolicy.recordSourceReadFailure() }; decoder = null
                    synchronized(decodeLock) { if (pendingUs == target && previewGeneration == generation) pendingUs = null }
                    publish {
                        if (!state.playing && rules?.displayUs(state.editor) == target && previewGeneration == generation)
                            state = state.copy(message = failure.message ?: "この位置のframeを復号できません。")
                    }
                }
            }
        } finally {
            synchronized(decodeLock) {
                decodeBusy = !disposed && pendingUs != null
                foregroundWork = decodeBusy
                if (decodeBusy) executor.execute(::drainDecode)
            }
        }
    }

    // --- Filmstrip -------------------------------------------------------------------------

    override fun stripResized(widthPx: Float, thumbWidthPx: Float) {
        val current = rules ?: return
        val replay = saved ?: return
        val slots = thumbnailSlots(current.grid, current.durationUs, thumbnailCount(widthPx, thumbWidthPx))
            .map { it?.let { at -> previewKeyframeUs(replay, at) } }
        if (slots == thumbnailSlots) return
        thumbnailSlots = slots
        val wanted = thumbnails.missing(slots)
        val generation = thumbnailGeneration.incrementAndGet()
        if (wanted.isEmpty()) return
        // Two independent IDR streams at most; no executor queue growing with resize events.
        (thumbnailExecutor as java.util.concurrent.ThreadPoolExecutor).queue.clear()
        wanted.chunked((wanted.size + 1) / 2).forEach { chunk ->
            thumbnailExecutor.execute { buildThumbnails(chunk, generation) }
        }
    }

    /** Independent keyframes only. Workers yield entirely while playback or interactive seeking runs. */
    private fun buildThumbnails(wanted: List<Long>, generation: Int) {
        fun active() {
            do {
                if (disposed || thumbnailGeneration.get() != generation || Thread.currentThread().isInterrupted) throw PlayerClosed()
                if (!foregroundWork && !playbackActive) break
                Thread.sleep(20)
            } while (true)
        }
        try {
            active()
            SavedReplay.open(folder) { _, _ -> active() }.use { replay ->
                active()
                val source = replay.openFile(io.github.shinma06.replaybuffer.core.ReplayFiles.VIDEO).use { "${replay.recordSha256}:${it.key}" }
                decoderFactory(replay, ReplayFrameCache(0)).use { cursor ->
                    var sinceRepaint = 0
                    wanted.forEach { target ->
                        active()
                        if (thumbnails[target] != null) return@forEach
                        val image = runCatching { diskThumbnails.read(source, target) }.getOrNull() ?: run {
                            check(previewKeyframeUs(replay, target) == target)
                            val frame = cursor.seek(target, ::active)
                            active()
                            scaledThumbnail(frame.image, THUMBNAIL_HEIGHT_PX).also { runCatching { diskThumbnails.write(source, target, it) } }
                        }
                        active()
                        thumbnails.put(target, image.previewBitmap())
                        if (++sinceRepaint >= 4) { sinceRepaint = 0; publish { state = state.copy(thumbnailVersion = state.thumbnailVersion + 1) } }
                    }
                }
            }
        } catch (failure: Throwable) {
            recordReadFailure(failure)
            // Thumbnails are decoration; a frame that cannot decode stays an empty slot.
        } finally {
            publish { state = state.copy(thumbnailVersion = state.thumbnailVersion + 1) }
        }
    }

    fun thumbnail(startUs: Long): ImageBitmap? = saved?.let { previewKeyframeUs(it, startUs) }?.let { thumbnails[it] }

    // --- Pointer and keys ------------------------------------------------------------------

    override fun press(grip: Grip, us: Long) {
        if (state.playing) stopPlayback()
        edit { r, s -> r.beginDrag(s, grip, us) }
    }

    override fun dragTo(us: Long) = edit { r, s -> r.dragTo(s, us) }

    override fun release() = edit { r, s -> r.endDrag(s) }

    fun step(delta: Int) { if (state.playing) stopPlayback(); edit { r, s -> r.step(s, delta) } }

    fun markIn() { if (!state.busy()) { stopPlayback(); edit { r, s -> r.markIn(s) } } }

    fun markOut() { if (!state.busy()) { stopPlayback(); edit { r, s -> r.markOut(s) } } }

    /** `キャンセル` / `中止` / Esc (3.2). */
    fun cancel() {
        if (disposed) return
        val current = rules ?: return
        // Resetting a playing range retains the real playback clock, as pausing does.
        if (state.editor.drag == null && !state.busy() && !current.rangeUnchanged(state.editor) && state.playing) togglePlay()
        val (next, effect) = current.cancel(state.editor)
        state = state.copy(editor = next, message = if (effect == CancelEffect.ABORT_TASK) null else state.message)
        when (effect) {
            CancelEffect.ABORT_TASK -> taskCancel?.set(true)
            CancelEffect.STOP_PLAYBACK -> { stopPlayback(); state = state.copy(message = null) }
            CancelEffect.NONE -> {}
        }
        showEditor()
    }

    override fun button(id: PlayerButton) {
        when (id) {
            PlayerButton.TRIM -> trim()
            PlayerButton.SAVE_FRAME -> exportPng()
            PlayerButton.CANCEL -> cancel()
        }
    }

    // --- Playback --------------------------------------------------------------------------

    override fun togglePlay() {
        if (disposed) return
        if (state.playing) {
            // Frame callbacks contain frame starts; pause must retain the clock within a held frame.
            val session = playback
            val current = rules
            // The natural-finish callback may still be queued when the clock reaches the end.
            val until = current?.playRange(state.editor)?.second
            if (session != null && current != null && until != null)
                state = state.copy(editor = current.played(state.editor, session.nowUs().coerceAtMost(until - 1)))
            stopPlayback()
            return
        }
        val current = rules ?: return
        if (state.error != null || state.editor.drag != null) return
        val (from, until) = current.playRange(state.editor) ?: return
        synchronized(decodeLock) { pendingUs = null; previewGeneration++ }
        playbackActive = true
        lateinit var session: ReplayPlaybackSession
        val showFrame = playedFrames.session { (position, image) ->
            if (playback === session && state.playing)
                state = state.copy(image = image, imageUs = position, editor = current.played(state.editor, position))
        }
        session = ReplayPlaybackSession(folder, from, until,
            onFrame = { frame ->
                showFrame(frame.startUs to frame.image.previewBitmap())
            },
            onFinished = { failure -> publish { if (playback === session) {
                state = state.copy(editor = current.playbackFinished(state.editor, until, failure != null))
                stopPlayback()
                failure?.let { state = state.copy(message = it) }
            } } },
            onAudioUnavailable = { reason -> publish { if (playback === session) state = state.copy(audioNote = reason) } },
            onGap = { us -> showFrame(us to null) },
            decoderFactory = { decoderFactory(it, frameCache) },
            onSourceFailure = closePolicy::recordSourceReadFailure)
        playback = session
        val closed = session.resourcesClosed
        playbackClosures.add(closed)
        closed.whenComplete { clean, failure ->
            if (clean != true || failure != null) playbackCleanupFailed.set(true)
            playbackClosures.remove(closed)
        }
        state = state.copy(playing = true, audioNote = null, message = null)
        session.start()
    }

    private fun stopPlayback() {
        val session = playback ?: return
        playback = null
        playbackActive = false
        session.stop()
        playedFrames.clear()
        state = state.copy(playing = false)
        showEditor()
    }

    // --- Tasks -----------------------------------------------------------------------------

    private fun PlayerState.busy() = editor.task != null

    fun exportPng() {
        val current = rules ?: return
        if (disposed || !playerButtons(current, state.editor, availability(current))
                .single { it.id == PlayerButton.SAVE_FRAME }.enabled) return
        stopPlayback()
        val cancel = AtomicBoolean()
        taskCancel = cancel
        edit { r, s -> r.taskStarted(s, TaskKind.FRAME) }
        state = state.copy(message = null)
        service.exportReplayFrame(folder, state.editor.playheadUs, cancel).whenComplete { file, failure ->
            if (failure != null) closePolicy.ambiguousOutput = true
            publish {
                val outcome = when { failure != null -> TaskOutcome.FAILED; file == null -> TaskOutcome.CANCELLED; else -> TaskOutcome.SUCCESS }
                taskCancel = null
                state = state.copy(editor = current.taskFinished(state.editor, TaskKind.FRAME, outcome),
                    message = when (outcome) {
                        TaskOutcome.SUCCESS -> "静止画を保存しました: $file"
                        TaskOutcome.CANCELLED -> "静止画の保存を中止しました。"
                        TaskOutcome.FAILED -> (failure.cause ?: failure).message ?: "静止画を保存できませんでした。"
                    })
            }
        }
    }

    fun trim() {
        val current = rules ?: return
        val editor = state.editor
        if (disposed || state.busy() || state.error != null || current.rangeUnchanged(editor)) return
        stopPlayback()
        val cancel = AtomicBoolean()
        taskCancel = cancel
        edit { r, s -> r.taskStarted(s, TaskKind.TRIM) }
        state = state.copy(message = null)
        service.trimReplay(folder, editor.inUs, editor.outUs, cancel).whenComplete { run, failure ->
            if (failure != null) closePolicy.ambiguousOutput = true
            if (run?.output != null) closePolicy.trimExported = true
            publish {
                val outcome = when { failure != null -> TaskOutcome.FAILED; run.output == null -> TaskOutcome.CANCELLED; else -> TaskOutcome.SUCCESS }
                taskCancel = null
                state = state.copy(editor = current.taskFinished(state.editor, TaskKind.TRIM, outcome),
                    message = when (outcome) {
                        TaskOutcome.SUCCESS -> "切り取った動画を保存しました: ${run.output}"
                        TaskOutcome.CANCELLED -> "切り取りを中止しました。完成フォルダは作っていません。" +
                            (run.retainedPartial?.let { " 未完成の出力は保全します: $it" } ?: "")
                        TaskOutcome.FAILED -> (failure.cause ?: failure).message ?: "切り取りを完了できませんでした。"
                    })
                showEditor()
            }
        }
    }

    private fun availability(current: TrimRules?): PlayerAvailability {
        val playheadFrame = current?.grid?.indexAt(state.editor.playheadUs) ?: -1
        // Playback completion uses end-1; a displayed image may be stamped with the frame start.
        val previewReady = state.image != null && playheadFrame >= 0 &&
            current?.grid?.indexAt(state.imageUs) == playheadFrame
        return PlayerAvailability(state.loading, state.error, playheadFrame < 0, previewReady)
    }

    /** The bar's derived view of the current state. */
    fun barModel(): PlayerBarModel {
        val current = rules
        val editor = state.editor
        val availability = availability(current)
        val buttons = if (current == null) listOf(
            ButtonSpec(PlayerButton.TRIM, "トリミング", true, false, availability.error?.let { "この保存は切り取れません: $it" } ?: "読み込み中です"),
            ButtonSpec(PlayerButton.SAVE_FRAME, "この位置をPNG保存", false, false),
            ButtonSpec(PlayerButton.CANCEL, "キャンセル", false, false))
        else playerButtons(current, editor, availability)
        return PlayerBarModel(editor, current, state.timeline, buttons, statusLine(state.message ?: state.audioNote, buttons),
            state.playing, current != null && state.error == null && editor.drag == null, ::thumbnail, state.thumbnailVersion, state.aspect)
    }

    override fun dispose() {
        if (disposed) return
        disposed = true
        previewGeneration++
        playback?.stop()
        // Retired sessions can still own SavedReplay leases while stop/decoder/native audio unwinds.
        val stoppedSessions = playbackClosures.toTypedArray()
        thumbnailGeneration.incrementAndGet()
        thumbnailExecutor.shutdownNow()
        executor.execute {
            val thumbnailsClosed = runCatching { thumbnailExecutor.awaitTermination(30, java.util.concurrent.TimeUnit.SECONDS) }.getOrDefault(false)
            val playbackClosed = runCatching {
                java.util.concurrent.CompletableFuture.allOf(*stoppedSessions).get(30, java.util.concurrent.TimeUnit.SECONDS)
                stoppedSessions.all { it.getNow(false) } && !playbackCleanupFailed.get()
            }.getOrDefault(false)
            val decoderClosed = runCatching { decoder?.close() }.isSuccess
            frameCache.clear()
            val replayClosed = runCatching { saved?.close() }.isSuccess
            resourcesClosed.complete(thumbnailsClosed && playbackClosed && decoderClosed && replayClosed)
        }
        executor.shutdown()
    }
}

private const val THUMBNAIL_HEIGHT_PX = 96

/** Downscaled copy for the filmstrip (two device pixels per dp of the 48dp strip). */
internal fun scaledThumbnail(image: BufferedImage, heightPx: Int): BufferedImage {
    val height = minOf(heightPx, image.height).coerceAtLeast(1)
    val width = (image.width.toLong() * height / image.height).toInt().coerceAtLeast(1)
    return BufferedImage(width, height, BufferedImage.TYPE_INT_RGB).also { out ->
        val graphics = out.createGraphics()
        try {
            graphics.setRenderingHint(RenderingHints.KEY_INTERPOLATION, RenderingHints.VALUE_INTERPOLATION_BILINEAR)
            graphics.drawImage(image, 0, 0, width, height, null)
        } finally {
            graphics.dispose()
        }
    }
}

@Composable
internal fun jewelPlayerPalette(): PlayerPalette {
    val colors = JewelTheme.globalColors
    val dark = JewelTheme.isDark
    val panel = colors.panelBackground
    return PlayerPalette(
        videoBackground = panel,
        barBackground = panel.copy(alpha = 0.9f),
        barBorder = colors.borders.normal,
        content = JewelTheme.contentColor,
        accent = colors.outlines.focused,
        onAccent = panel,
        dimOutside = panel.copy(alpha = if (dark) 0.55f else 0.5f),
        emptyThumb = JewelTheme.contentColor.copy(alpha = 0.08f),
        gap = colors.text.error.copy(alpha = 0.6f),
        unknownClock = colors.text.warning,
        tap = colors.text.info,
        textStyle = JewelTheme.defaultTextStyle,
    )
}

@Composable
internal fun ReplayPlayerContent(controller: ReplayPlayerController) {
    val state = controller.state
    val focus = remember { FocusRequester() }
    LaunchedEffect(Unit) { runCatching { focus.requestFocus() } }
    Column(Modifier.fillMaxSize()
        .focusRequester(focus).focusable()
        .pointerInput(Unit) {
            awaitPointerEventScope {
                while (true) { if (awaitPointerEvent().type == PointerEventType.Press) runCatching { focus.requestFocus() } }
            }
        }
        .onPreviewKeyEvent { event ->
            if (event.type != KeyEventType.KeyDown || event.isCtrlPressed || event.isMetaPressed || event.isAltPressed) return@onPreviewKeyEvent false
            when (event.key) {
                Key.I -> controller.markIn()
                Key.O -> controller.markOut()
                Key.DirectionLeft -> controller.step(-1)
                Key.DirectionRight -> controller.step(1)
                Key.Spacebar -> controller.togglePlay()
                Key.Escape -> controller.cancel()
                else -> return@onPreviewKeyEvent false
            }
            true
        }) {
        Row(Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 6.dp), horizontalArrangement = Arrangement.spacedBy(8.dp),
            verticalAlignment = Alignment.CenterVertically) {
            Text(controller.folderName, fontWeight = FontWeight.Bold, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f))
            state.error?.let { Text(it, color = JewelTheme.globalColors.text.error) }
        }
        PlayerSurface(
            image = state.image,
            placeholder = when { state.loading -> "読み込み中…"; state.image == null -> "表示するframeがありません"; else -> null },
            palette = jewelPlayerPalette(),
            bar = controller.barModel(),
            actions = controller,
            modifier = Modifier.fillMaxWidth().weight(1f),
        ) { spec, modifier, compact, onClick ->
            val normal = if (spec.accent) JewelTheme.defaultButtonStyle else JewelTheme.outlinedButtonStyle
            val style = if (compact) remember(normal) {
                ButtonStyle(normal.colors, compactPlayerButtonMetrics(normal.metrics), normal.focusOutlineAlignment)
            } else normal
            if (spec.accent) DefaultButton(onClick, enabled = spec.enabled, modifier = modifier, style = style) { Text(spec.label, maxLines = 1, softWrap = false) }
            else OutlinedButton(onClick, enabled = spec.enabled, modifier = modifier, style = style) { Text(spec.label, maxLines = 1, softWrap = false) }
        }
    }
}

internal fun compactPlayerButtonMetrics(metrics: ButtonMetrics) = ButtonMetrics(
    cornerSize = metrics.cornerSize,
    padding = PaddingValues(start = 4.dp, top = metrics.padding.calculateTopPadding(),
        end = 4.dp, bottom = metrics.padding.calculateBottomPadding()),
    minSize = DpSize(40.dp, metrics.minSize.height),
    borderWidth = metrics.borderWidth,
    focusOutlineExpand = metrics.focusOutlineExpand,
)
