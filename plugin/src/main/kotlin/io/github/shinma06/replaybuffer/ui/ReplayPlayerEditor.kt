package io.github.shinma06.replaybuffer.ui

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.detectDragGestures
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.PathEffect
import androidx.compose.ui.graphics.drawscope.clipRect
import androidx.compose.ui.graphics.toComposeImageBitmap
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.intellij.openapi.Disposable
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
import io.github.shinma06.replaybuffer.core.ReplayFiles
import io.github.shinma06.replaybuffer.core.ReplayFrameExport
import io.github.shinma06.replaybuffer.core.ReplayTimeline
import io.github.shinma06.replaybuffer.core.SavedReplay
import io.github.shinma06.replaybuffer.ide.ReplayProjectService
import org.jetbrains.jewel.bridge.JewelComposePanel
import org.jetbrains.jewel.foundation.theme.JewelTheme
import org.jetbrains.jewel.ui.component.DefaultButton
import org.jetbrains.jewel.ui.component.OutlinedButton
import org.jetbrains.jewel.ui.component.Text
import java.awt.BorderLayout
import java.awt.event.KeyAdapter
import java.awt.event.KeyEvent
import java.awt.image.BufferedImage
import java.beans.PropertyChangeListener
import java.nio.file.Path
import java.util.Locale
import java.util.concurrent.Executors
import javax.swing.JPanel
import javax.swing.SwingUtilities
import javax.swing.Timer

/** Opens a completed save's video.mp4 in the replay player instead of a default editor. */
internal class ReplayPlayerEditorProvider : FileEditorProvider, DumbAware {
    override fun accept(project: Project, file: VirtualFile): Boolean = file.name == ReplayFiles.VIDEO &&
        file.parent?.let { it.findChild(ReplayFiles.RECORD) != null && it.findChild(ReplayFiles.VIDEO_FRAMES) != null } == true

    override fun createEditor(project: Project, file: VirtualFile): FileEditor = ReplayPlayerEditor(project, file)

    override fun getEditorTypeId(): String = "android-replay-buffer-player"

    override fun getPolicy(): FileEditorPolicy = FileEditorPolicy.PLACE_BEFORE_DEFAULT_EDITOR
}

/** Minimal saved-replay player: seek, frame step, In/Out marks, PNG export and trim-to-save. */
internal class ReplayPlayerEditor(project: Project, private val file: VirtualFile) : UserDataHolderBase(), FileEditor {
    private val controller = ReplayPlayerController(project,
        checkNotNull(file.parent).toNioPath().toAbsolutePath().normalize())

    private val component = JPanel(BorderLayout()).apply {
        add(JewelComposePanel { ReplayPlayerContent(controller) }, BorderLayout.CENTER)
        isFocusable = true
        addKeyListener(object : KeyAdapter() {
            override fun keyPressed(event: KeyEvent) {
                when (event.keyChar.lowercaseChar()) {
                    'i' -> controller.markIn()
                    'o' -> controller.markOut()
                    else -> return
                }
                event.consume()
            }
        })
    }

    init { Disposer.register(this, controller) }

    override fun getComponent() = component
    override fun getPreferredFocusedComponent() = component
    override fun getName() = "動画プレイヤー"
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
    val positionUs: Long = 0,
    val image: BufferedImage? = null,
    val decoding: Boolean = false,
    val inUs: Long = 0,
    val outUs: Long = 0,
    val playing: Boolean = false,
    val busy: Boolean = false,
    val message: String? = null,
    val timeline: ReplayTimeline? = null,
)

private class PlayerClosed : RuntimeException()

/** Owns the opened replay, the coalescing decode worker, playback and the trim/PNG requests. */
internal class ReplayPlayerController(
    project: Project,
    internal val folder: Path,
) : Disposable {
    private val service = project.getService(ReplayProjectService::class.java)
    private val executor = Executors.newSingleThreadExecutor { runnable ->
        Thread(runnable, "arb-replay-player").apply { isDaemon = true }
    }
    private val decodeLock = Any()
    private var pendingUs: Long? = null
    private var decodeBusy = false
    @Volatile private var disposed = false
    @Volatile private var saved: SavedReplay? = null
    private var playTimer: Timer? = null

    var state by mutableStateOf(PlayerState())
        private set

    val folderName: String get() = folder.fileName?.toString() ?: folder.toString()

    init {
        executor.execute {
            try {
                val opened = SavedReplay.open(folder)
                val timeline = opened.timeline()
                saved = opened
                publish {
                    state = state.copy(loading = false, durationUs = timeline.durationUs,
                        outUs = timeline.durationUs, timeline = timeline)
                }
                requestDecode(0)
            } catch (failure: Throwable) {
                publish { state = state.copy(loading = false, error = failure.message ?: "保存フォルダを開けません。") }
            }
        }
    }

    private fun publish(action: () -> Unit) {
        if (SwingUtilities.isEventDispatchThread()) { if (!disposed) action() }
        else SwingUtilities.invokeLater { if (!disposed) action() }
    }

    private fun requestDecode(us: Long) {
        val run = synchronized(decodeLock) {
            pendingUs = us
            if (!decodeBusy) { decodeBusy = true; true } else false
        }
        publish { state = state.copy(decoding = true) }
        if (run) executor.execute(::drainDecode)
    }

    /** Decodes only the newest requested position; stale requests are skipped, not queued. */
    private fun drainDecode() {
        try {
            while (!disposed) {
                val target = synchronized(decodeLock) { pendingUs } ?: break
                val replay = saved ?: break
                try {
                    val frame = ReplayFrameExport.decodeAt(replay, target) { if (disposed) throw PlayerClosed() }
                    val done = synchronized(decodeLock) { if (pendingUs == target) { pendingUs = null; true } else false }
                    publish { state = state.copy(image = frame.image, positionUs = target, decoding = !done) }
                } catch (_: PlayerClosed) {
                    return
                } catch (failure: Throwable) {
                    val done = synchronized(decodeLock) { if (pendingUs == target) { pendingUs = null; true } else false }
                    publish {
                        state = state.copy(decoding = !done,
                            message = failure.message ?: "この位置のframeを復号できません。")
                    }
                }
            }
        } finally {
            val rerun = synchronized(decodeLock) { decodeBusy = false; pendingUs != null }
            if (rerun) requestDecode(synchronized(decodeLock) { pendingUs!! })
        }
    }

    fun seek(us: Long) {
        if (disposed || state.durationUs <= 0) return
        val target = us.coerceIn(0, state.durationUs - 1)
        state = state.copy(positionUs = target)
        requestDecode(target)
    }

    fun step(delta: Int) {
        val timeline = state.timeline ?: return
        if (timeline.spans.isEmpty()) return
        val current = timeline.spanAt(state.positionUs)
        val index = when {
            current >= 0 -> timeline.neighbour(current, delta)
            delta > 0 -> timeline.spans.indexOfFirst { it.startUs > state.positionUs }.takeIf { it >= 0 } ?: return
            else -> timeline.spans.indexOfLast { it.endUs <= state.positionUs }.takeIf { it >= 0 } ?: return
        }
        seek(timeline.spans[index].startUs)
    }

    fun togglePlay() {
        if (disposed) return
        if (state.playing) {
            playTimer?.stop(); playTimer = null
            state = state.copy(playing = false)
            return
        }
        if (state.durationUs <= 0 || state.error != null) return
        val timer = Timer(100) {
            val next = state.timeline?.spans?.firstOrNull { it.startUs > state.positionUs }?.startUs
            if (next == null) togglePlay() else seek(next)
        }
        playTimer = timer
        timer.start()
        state = state.copy(playing = true)
    }

    fun markIn() {
        if (disposed || state.error != null) return
        val mark = state.positionUs.coerceIn(0, state.durationUs)
        state = state.copy(inUs = mark, outUs = if (state.outUs <= mark) state.durationUs else state.outUs)
    }

    fun markOut() {
        if (disposed || state.error != null) return
        val mark = state.positionUs.coerceIn(0, state.durationUs)
        state = state.copy(outUs = mark, inUs = if (state.inUs >= mark) 0 else state.inUs)
    }

    fun exportPng() {
        if (disposed || state.busy || state.error != null) return
        state = state.copy(busy = true, message = null)
        service.exportReplayFrame(folder, state.positionUs).whenComplete { file, failure ->
            publish {
                state = state.copy(busy = false,
                    message = if (failure == null) "静止画を保存しました: $file"
                    else (failure.cause ?: failure).message ?: "静止画を保存できませんでした。")
            }
        }
    }

    fun trim() {
        if (disposed || state.busy || state.error != null || state.inUs >= state.outUs) return
        state = state.copy(busy = true, message = null)
        service.trimReplay(folder, state.inUs, state.outUs).whenComplete { output, failure ->
            publish {
                state = state.copy(busy = false,
                    message = when {
                        failure != null -> (failure.cause ?: failure).message ?: "切り取りを完了できませんでした。"
                        output == null -> "切り取りを中止しました。未完成の出力は保全します。"
                        else -> "切り取った動画を保存しました: $output"
                    })
            }
        }
    }

    override fun dispose() {
        disposed = true
        playTimer?.stop()
        executor.execute { runCatching { saved?.close() } }
        executor.shutdown()
    }
}

internal fun playerTime(us: Long): String = String.format(Locale.ROOT, "%d:%06.3f", us / 60_000_000, us % 60_000_000 / 1e6)

@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun ReplayPlayerContent(controller: ReplayPlayerController) {
    val state = controller.state
    val timeline = state.timeline
    Column(Modifier.fillMaxSize().padding(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
            Text(controller.folderName, fontWeight = FontWeight.Bold, modifier = Modifier.weight(1f))
            state.error?.let { Text(it, color = Color(0xFFF06A6A)) }
        }
        Box(Modifier.fillMaxWidth().weight(1f).background(Color.Black), contentAlignment = Alignment.Center) {
            when {
                state.image != null -> Image(state.image.toComposeImageBitmap(), "動画の表示frame",
                    modifier = Modifier.fillMaxSize(), contentScale = ContentScale.Fit)
                state.loading -> Text("読み込み中…", color = Color.White)
                else -> Text("表示するframeがありません", color = Color.White)
            }
            if (state.decoding) Text("復号中…", color = Color(0xFF35AAF0),
                modifier = Modifier.align(Alignment.TopEnd).padding(4.dp))
        }
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
            Text("位置 ${playerTime(state.positionUs)} / ${playerTime(state.durationUs)}")
            if (timeline?.clockKnown == false) Text("時刻不明", color = Color(0xFFE0A03D))
            if (timeline != null && timeline.spanAt(state.positionUs) < 0 && state.error == null)
                Text("欠落", color = Color(0xFFF06A6A))
        }
        SeekBar(state) { controller.seek(it) }
        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            val idle = !state.busy && state.error == null && !state.loading
            OutlinedButton({ controller.step(-1) }, enabled = idle,
                modifier = Modifier.semantics { contentDescription = "1コマ戻る" }) { Text("1コマ戻る") }
            OutlinedButton({ controller.togglePlay() }, enabled = idle,
                modifier = Modifier.semantics { contentDescription = if (state.playing) "停止" else "再生" }) {
                Text(if (state.playing) "停止" else "再生")
            }
            OutlinedButton({ controller.step(1) }, enabled = idle,
                modifier = Modifier.semantics { contentDescription = "1コマ進む" }) { Text("1コマ進む") }
            OutlinedButton({ controller.markIn() }, enabled = idle,
                modifier = Modifier.semantics { contentDescription = "ここを開始にする" }) { Text("ここを開始にする（I）") }
            OutlinedButton({ controller.markOut() }, enabled = idle,
                modifier = Modifier.semantics { contentDescription = "ここを終了にする" }) { Text("ここを終了にする（O）") }
            OutlinedButton({ controller.exportPng() }, enabled = idle && !state.decoding,
                modifier = Modifier.semantics { contentDescription = "この画面をPNG保存" }) { Text("この画面をPNG保存") }
        }
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
            Text("${playerTime(state.inUs)}〜${playerTime(state.outUs)}（${"%.3f".format(Locale.ROOT, (state.outUs - state.inUs) / 1e6)}秒）を残す",
                modifier = Modifier.weight(1f))
            val trimEnabled = !state.busy && state.error == null && !state.loading && state.inUs < state.outUs
            DefaultButton({ controller.trim() }, enabled = trimEnabled,
                modifier = Modifier.semantics { contentDescription = "切り取って保存" }) {
                Text(if (state.busy) "保存中…" else "切り取って保存")
            }
        }
        state.message?.let { Text(it, color = JewelTheme.globalColors.text.info) }
        if (state.busy) Text("保存を実行中です。中止は実行中タスクから行えます。", color = JewelTheme.globalColors.text.info)
    }
}

@Composable
private fun SeekBar(state: PlayerState, onSeek: (Long) -> Unit) {
    val duration = state.durationUs.coerceAtLeast(1L)
    val gaps = state.timeline?.gaps.orEmpty()
    val taps = state.timeline?.taps.orEmpty()
    val clockKnown = state.timeline?.clockKnown != false
    Canvas(Modifier.fillMaxWidth().height(26.dp)
        .semantics { contentDescription = "シークバー" }
        .pointerInput(duration) { detectTapGestures { onSeek((it.x / size.width * duration).toLong()) } }
        .pointerInput(duration) {
            detectDragGestures { change, _ ->
                onSeek((change.position.x / size.width * duration).toLong()); change.consume()
            }
        }
    ) {
        val width = size.width
        fun x(us: Long) = (us.toDouble() / duration * width).toFloat().coerceIn(0f, width)
        val centerY = size.height / 2f
        if (clockKnown) {
            drawLine(Color(0xFF6E6E6E), Offset(0f, centerY), Offset(width, centerY), strokeWidth = 4f)
        } else {
            drawLine(Color(0xFF6E6E6E), Offset(0f, centerY), Offset(width, centerY), strokeWidth = 4f,
                pathEffect = PathEffect.dashPathEffect(floatArrayOf(8f, 6f)))
        }
        gaps.forEach { gap ->
            val left = x(gap.startUs)
            val right = x(gap.endUs).coerceAtLeast(left + 2f)
            drawRect(Color(0x66C62828), Offset(left, centerY - 8f), Size(right - left, 16f))
            clipRect(left, centerY - 8f, right, centerY + 8f) {
                var stripe = -16f
                while (stripe < right - left + 16f) {
                    drawLine(Color(0xAAC62828), Offset(left + stripe, centerY + 8f),
                        Offset(left + stripe + 16f, centerY - 8f), strokeWidth = 1.5f)
                    stripe += 6f
                }
            }
        }
        taps.forEach { tap ->
            drawLine(Color(0xFF35AAF0), Offset(x(tap), centerY - 7f), Offset(x(tap), centerY + 7f), strokeWidth = 1.5f)
        }
        fun flag(us: Long, color: Color) {
            val flagX = x(us)
            drawLine(color, Offset(flagX, centerY - 11f), Offset(flagX, centerY + 11f), strokeWidth = 2f)
            drawPath(androidx.compose.ui.graphics.Path().apply {
                moveTo(flagX, centerY - 11f); lineTo(flagX + 8f, centerY - 7f); lineTo(flagX, centerY - 3f); close()
            }, color)
        }
        flag(state.inUs, Color(0xFF1F7A33))
        flag(state.outUs, Color(0xFFA36200))
        val thumb = Offset(x(state.positionUs), centerY)
        drawCircle(Color.White, 6.5f, thumb)
        drawCircle(Color(0xFF3574F0), 5f, thumb)
    }
}
