package io.github.shinma06.replaybuffer.ui

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.wrapContentSize
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicText
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.ClipOp
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.PathEffect
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.drawscope.clipRect
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.invisibleToUser
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import io.github.shinma06.replaybuffer.core.ReplayTimeline
import kotlin.math.roundToInt

/** Colours of the player, taken from the IDE theme by the caller (light and dark). */
internal data class PlayerPalette(
    val videoBackground: Color,
    val barBackground: Color,
    val barBorder: Color,
    val content: Color,
    val accent: Color,
    val onAccent: Color,
    val dimOutside: Color,
    val emptyThumb: Color,
    val gap: Color,
    val unknownClock: Color,
    val tap: Color,
    val textStyle: TextStyle,
)

/** Everything the bar draws, already derived from controller state. */
internal class PlayerBarModel(
    val editor: TrimEditor,
    val rules: TrimRules?,
    val timeline: ReplayTimeline?,
    val buttons: List<ButtonSpec>,
    val status: String?,
    val playing: Boolean,
    val playEnabled: Boolean,
    val thumbnail: (Long) -> ImageBitmap?,
    val thumbnailVersion: Int,
    /** Video width / height; a portrait phone default until the first frame is decoded. */
    val aspect: Float,
)

/** Callbacks from the bar to the controller. */
internal interface PlayerBarActions {
    fun togglePlay()
    fun press(grip: Grip, us: Long)
    fun dragTo(us: Long)
    fun release()
    fun button(id: PlayerButton)
    fun stripResized(widthPx: Float, thumbWidthPx: Float)
}

private val HANDLE = 12.dp
private val SLOP = 6.dp
private val STRIP_HEIGHT = 48.dp

/**
 * Video with the translucent rounded control bar overlaid at its bottom (3.1). Theme-agnostic: the
 * IDE passes a palette and Jewel buttons through [buttonContent]; tests render it headless.
 */
@Composable
internal fun PlayerSurface(
    image: ImageBitmap?,
    placeholder: String?,
    palette: PlayerPalette,
    bar: PlayerBarModel,
    actions: PlayerBarActions,
    modifier: Modifier = Modifier,
    buttonContent: @Composable (ButtonSpec, Modifier, Boolean, () -> Unit) -> Unit,
) {
    BoxWithConstraints(modifier.background(palette.videoBackground)) {
        val layout = controlLayout(maxWidth.value)
        if (image != null) {
            Image(image, "動画の表示frame", Modifier.fillMaxSize(), contentScale = ContentScale.Fit)
        } else if (placeholder != null) {
            BasicText(placeholder, Modifier.align(Alignment.Center), style = palette.textStyle.copy(color = palette.content))
        }
        Column(
            Modifier.align(Alignment.BottomCenter).padding(12.dp).fillMaxWidth()
                // No clip: the time bubble rises above the bar over the video.
                .background(palette.barBackground, RoundedCornerShape(10.dp))
                .border(1.dp, palette.barBorder, RoundedCornerShape(10.dp))
                .padding(horizontal = 10.dp, vertical = 8.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                PlayButton(bar.playing, bar.playEnabled, palette) { actions.togglePlay() }
                Spacer(Modifier.width(10.dp))
                Filmstrip(bar, palette, actions, Modifier.weight(1f))
                if (layout == ControlLayout.STACKED) {
                    Spacer(Modifier.width(12.dp))
                    Column(Modifier.width(IntrinsicSize.Max), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                        bar.buttons.forEach { spec ->
                            buttonContent(spec, Modifier.fillMaxWidth().semantics { contentDescription = spec.label }, false) { actions.button(spec.id) }
                        }
                    }
                }
            }
            if (layout == ControlLayout.ROW) {
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.End)) {
                    rowOrder(bar.buttons).forEach { spec ->
                        buttonContent(spec, Modifier.semantics { contentDescription = spec.label }, true) { actions.button(spec.id) }
                    }
                }
            }
            val status = bar.status
            BasicText(status ?: "この位置の映像を準備しています",
                Modifier.then(if (status == null) Modifier.semantics { invisibleToUser() } else Modifier),
                style = palette.textStyle.copy(color = palette.content.copy(alpha = if (status == null) 0f else 0.8f), fontSize = 12.sp),
                maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
    }
}

@Composable
private fun PlayButton(playing: Boolean, enabled: Boolean, palette: PlayerPalette, onClick: () -> Unit) {
    val colour = palette.content.copy(alpha = if (enabled) 0.9f else 0.35f)
    Canvas(Modifier.size(36.dp).clip(RoundedCornerShape(6.dp))
        .clickable(enabled = enabled, onClick = onClick)
        .semantics { contentDescription = if (playing) "一時停止" else "再生" }) {
        val s = size.minDimension
        if (playing) {
            val w = s * 0.14f
            drawRect(colour, Offset(s * 0.3f, s * 0.25f), Size(w, s * 0.5f))
            drawRect(colour, Offset(s * 0.56f, s * 0.25f), Size(w, s * 0.5f))
        } else {
            drawPath(Path().apply { moveTo(s * 0.32f, s * 0.22f); lineTo(s * 0.78f, s * 0.5f); lineTo(s * 0.32f, s * 0.78f); close() }, colour)
        }
    }
}

@Composable
private fun Filmstrip(bar: PlayerBarModel, palette: PlayerPalette, actions: PlayerBarActions, modifier: Modifier) {
    val density = LocalDensity.current
    val handlePx = with(density) { HANDLE.toPx() }
    val slopPx = with(density) { SLOP.toPx() }
    val stripHeightPx = with(density) { STRIP_HEIGHT.toPx() }
    val duration = bar.rules?.durationUs ?: 0L
    val current by rememberUpdatedState(bar)
    val thumbAspect = bar.aspect
    val editor = bar.editor
    val description = "フィルムストリップ。残す範囲 ${bubbleTime(editor.inUs)}〜${bubbleTime(editor.outUs)}、再生位置 ${bubbleTime(editor.playheadUs)}"
    BoxWithConstraints(modifier.height(STRIP_HEIGHT)) {
        val widthPx = constraints.maxWidth.toFloat()
        val loaded = bar.rules != null
        LaunchedEffect(widthPx, thumbAspect, loaded) { actions.stripResized(widthPx, stripHeightPx * thumbAspect) }
        Canvas(Modifier.fillMaxSize()
            .semantics { contentDescription = description }
            .pointerInput(duration) {
                awaitEachGesture {
                    val down = awaitFirstDown()
                    val geometry = StripGeometry(this.size.width.toFloat(), duration)
                    val pressedEditor = current.editor
                    val grip = geometry.hitTest(pressedEditor, down.position.x, handlePx, slopPx)
                    actions.press(grip, geometry.dragUs(pressedEditor, grip, down.position.x, down.position.x))
                    down.consume()
                    while (true) {
                        val event = awaitPointerEvent()
                        val change = event.changes.firstOrNull() ?: break
                        if (!change.pressed) break
                        actions.dragTo(geometry.dragUs(pressedEditor, grip, down.position.x, change.position.x))
                        change.consume()
                    }
                    actions.release()
                }
            }) {
            drawStrip(bar, palette, handlePx)
        }
        val bubble = bar.rules?.bubble(editor)
        if (bubble != null && widthPx > 0f) {
            val geometry = StripGeometry(widthPx, duration)
            // Over the middle of the held handle (handles sit inside the frame edges), or on the playhead line.
            val anchor = geometry.x(bubble.us) + when (bubble.grip) { Grip.IN -> handlePx / 2; Grip.OUT -> -handlePx / 2; Grip.PLAYHEAD -> 0f }
            TimeBubbleView(bubble, palette, anchor)
        }
    }
}

@Composable
private fun TimeBubbleView(bubble: TimeBubble, palette: PlayerPalette, x: Float) {
    var bubbleSize by remember { mutableStateOf(IntSize.Zero) }
    val gapPx = with(LocalDensity.current) { 6.dp.toPx() }
    Column(
        Modifier.wrapContentSize(unbounded = true)
            .onSizeChanged { bubbleSize = it }
            .offset { IntOffset((x - bubbleSize.width / 2f).roundToInt(), (-bubbleSize.height - gapPx).roundToInt()) },
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        BasicText(bubble.text,
            Modifier.clip(RoundedCornerShape(6.dp)).background(palette.barBackground.copy(alpha = 1f))
                .border(1.dp, palette.barBorder, RoundedCornerShape(6.dp)).padding(horizontal = 8.dp, vertical = 2.dp),
            style = palette.textStyle.copy(color = palette.content, fontSize = 12.sp), maxLines = 1, softWrap = false)
        Canvas(Modifier.size(10.dp, 5.dp)) {
            drawPath(Path().apply { moveTo(0f, 0f); lineTo(size.width, 0f); lineTo(size.width / 2, size.height); close() }, palette.barBorder)
        }
    }
}

private fun DrawScope.drawStrip(bar: PlayerBarModel, palette: PlayerPalette, handlePx: Float) {
    val rules = bar.rules ?: run {
        drawRoundRect(palette.emptyThumb, cornerRadius = CornerRadius(4.dp.toPx())); return
    }
    val width = size.width
    val height = size.height
    val geometry = StripGeometry(width, rules.durationUs)
    val aspect = bar.aspect
    val count = thumbnailCount(width, height * aspect)
    val slots = thumbnailSlots(rules.grid, rules.durationUs, count)
    val slotWidth = if (count > 0) width / count else width
    val radius = CornerRadius(4.dp.toPx())
    clipRect(0f, 0f, width, height) {
        drawRoundRect(palette.emptyThumb, cornerRadius = radius)
        slots.forEachIndexed { index, start ->
            val left = index * slotWidth
            val image = start?.let(bar.thumbnail) ?: return@forEachIndexed
            // Centre-crop the frame into its slot.
            val scale = maxOf(slotWidth / image.width, height / image.height)
            val srcW = (slotWidth / scale).roundToInt().coerceIn(1, image.width)
            val srcH = (height / scale).roundToInt().coerceIn(1, image.height)
            drawImage(image, srcOffset = IntOffset((image.width - srcW) / 2, (image.height - srcH) / 2),
                srcSize = IntSize(srcW, srcH), dstOffset = IntOffset(left.roundToInt(), 0),
                dstSize = IntSize((left + slotWidth).roundToInt() - left.roundToInt(), height.roundToInt()))
        }
        bar.timeline?.gaps?.forEach { gap ->
            val left = geometry.x(gap.startUs)
            val right = geometry.x(gap.endUs).coerceAtLeast(left + 2f)
            drawRect(palette.videoBackground, Offset(left, 0f), Size(right - left, height))
            clipRect(left, 0f, right, height) {
                var stripe = -height
                while (stripe < right - left + height) {
                    drawLine(palette.gap, Offset(left + stripe, height), Offset(left + stripe + height, 0f), strokeWidth = 1.5f)
                    stripe += 7f
                }
            }
        }
        if (bar.timeline?.clockKnown == false) {
            drawLine(palette.unknownClock, Offset(0f, height - 1.5f), Offset(width, height - 1.5f), strokeWidth = 2f,
                pathEffect = PathEffect.dashPathEffect(floatArrayOf(6f, 5f)))
        }
        bar.timeline?.taps?.forEach { tap ->
            val x = geometry.x(tap)
            drawLine(palette.tap, Offset(x, height - 7f), Offset(x, height), strokeWidth = 1.5f)
        }
    }
    val editor = bar.editor
    val range = geometry.handleRange(editor, handlePx)
    run {
        val left = range.start
        val right = range.endInclusive
        drawRect(palette.dimOutside, Offset(0f, 0f), Size(left, height))
        drawRect(palette.dimOutside, Offset(right, 0f), Size(width - right, height))
        val border = 3.dp.toPx()
        drawRoundRect(palette.accent, Offset(left + border / 2, border / 2), Size(right - left - border, height - border),
            cornerRadius = CornerRadius(5.dp.toPx()), style = Stroke(border))
        fun handle(x: Float) {
            drawRoundRect(palette.accent, Offset(x, 0f), Size(handlePx, height), cornerRadius = CornerRadius(4.dp.toPx()))
            val cx = x + handlePx / 2
            listOf(-1.5f, 1.5f).forEach { dx ->
                drawLine(palette.onAccent, Offset(cx + dx.dp.toPx() / 1.5f, height * 0.35f), Offset(cx + dx.dp.toPx() / 1.5f, height * 0.65f), strokeWidth = 1.dp.toPx())
            }
        }
        handle(left)
        handle(right - handlePx)
    }
    drawPlayerPlayhead(geometry.x(editor.playheadUs), palette.content, palette.barBackground, range, handlePx)
}

internal fun DrawScope.drawPlayerPlayhead(x: Float, foreground: Color, outline: Color,
    range: ClosedFloatingPointRange<Float>, handlePx: Float) {
    val ink = foreground.copy(alpha = 1f)
    val edge = outline.copy(alpha = 1f)
    val inGrip = range.start + handlePx / 2
    val outGrip = range.endInclusive - handlePx / 2
    val pad = 3.dp.toPx()
    val top = size.height * 0.35f - 1.dp.toPx()
    val bottom = size.height * 0.65f + 1.dp.toPx()
    // Keep the handle grips visible where they win pointer input.
    clipRect {
        clipRect(inGrip - pad, top, inGrip + pad, bottom, ClipOp.Difference) {
            clipRect(outGrip - pad, top, outGrip + pad, bottom, ClipOp.Difference) {
                drawLine(edge, Offset(x, 0f), Offset(x, size.height), strokeWidth = 8.dp.toPx())
                drawLine(ink, Offset(x, 0f), Offset(x, size.height), strokeWidth = 4.dp.toPx())
                val centre = Offset(x, 8.dp.toPx())
                drawCircle(edge, radius = 8.dp.toPx(), center = centre)
                drawCircle(ink, radius = 5.dp.toPx(), center = centre)
            }
        }
    }
}
