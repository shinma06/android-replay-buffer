package io.github.shinma06.arb.ui

import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.Image
import androidx.compose.foundation.VerticalScrollbar
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.defaultScrollbarStyle
import androidx.compose.foundation.focusable
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.detectHorizontalDragGestures
import androidx.compose.foundation.hoverable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsHoveredAsState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollbarAdapter
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.PathEffect
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.drawscope.clipRect
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.isAltPressed
import androidx.compose.ui.input.key.isCtrlPressed
import androidx.compose.ui.input.key.isMetaPressed
import androidx.compose.ui.input.key.key
import androidx.compose.ui.input.key.onPreviewKeyEvent
import androidx.compose.ui.input.key.type
import androidx.compose.ui.input.pointer.PointerEventType
import androidx.compose.ui.input.pointer.PointerIcon
import androidx.compose.ui.input.pointer.pointerHoverIcon
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.dp
import io.github.shinma06.arb.model.DiscontinuityEvent
import io.github.shinma06.arb.model.DiscontinuityKind
import io.github.shinma06.arb.model.GestureKind
import io.github.shinma06.arb.model.LogRecord
import io.github.shinma06.arb.model.TimeAlignment
import io.github.shinma06.arb.state.APPROXIMATE_PREVIEW_BADGE_TEXT
import io.github.shinma06.arb.state.ArbFormatters
import io.github.shinma06.arb.state.DiscontinuityFormatter
import io.github.shinma06.arb.state.EventStreamFilter
import io.github.shinma06.arb.state.AuxStream
import io.github.shinma06.arb.state.StreamLoadState
import io.github.shinma06.arb.state.StudioLoadMessages
import io.github.shinma06.arb.state.ExportCompletionInfo
import io.github.shinma06.arb.state.HonestySeverity
import io.github.shinma06.arb.state.ProvenanceFormatter
import io.github.shinma06.arb.state.SyncedEventKind
import io.github.shinma06.arb.state.SyncedEventRow
import io.github.shinma06.arb.state.TimelineViewerState
import org.jetbrains.jewel.foundation.theme.JewelTheme
import org.jetbrains.jewel.ui.Orientation
import org.jetbrains.jewel.ui.component.DefaultButton
import org.jetbrains.jewel.ui.component.Divider
import org.jetbrains.jewel.ui.component.IconActionButton
import org.jetbrains.jewel.ui.component.ListComboBox
import org.jetbrains.jewel.ui.component.Text
import org.jetbrains.jewel.ui.component.TextField
import org.jetbrains.jewel.ui.icons.AllIconsKeys
import org.jetbrains.jewel.ui.theme.scrollbarStyle
import java.awt.Cursor
import java.awt.Rectangle
import kotlin.math.abs
import kotlin.math.atan2
import kotlin.math.cos
import kotlin.math.roundToInt
import kotlin.math.roundToLong
import kotlin.math.sin

/**
 * Native JetBrains Jewel Compose UI for the Integrated Viewer & Trim Studio (`ArbBundleFileEditor`).
 *
 * Strictly uses `JewelTheme` tokens (no hardcoded dark-mode surface hexes), borderless subtle
 * toolbar actions, QuickTime-inspired warm gold dual trim handles, vector tap ripples normalized by
 * `display_width`/`display_height`, synced event stream with hover `[In]`/`[Out]` snap and PSI
 * source jump, and keyboard shortcuts (`Space`, `Left`/`Right`, `I`, `O`).
 */
@Composable
fun ArbStudioContent(controller: ArbStudioController) {
    val state = controller.viewerState
    val focusRequester = remember { FocusRequester() }
    var searchHasFocus by remember { mutableStateOf(false) }
    var splitFraction by remember { mutableFloatStateOf(0.58f) }

    LaunchedEffect(Unit) { runCatching { focusRequester.requestFocus() } }
    LaunchedEffect(controller.searchFieldState) {
        snapshotFlow { controller.searchFieldState.text.toString() }.collect { q ->
            controller.updateSearchQuery(q)
        }
    }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .background(ArbJewelPalette.panelBackground())
            .focusRequester(focusRequester)
            .focusable()
            .onPreviewKeyEvent { event ->
                if (searchHasFocus) return@onPreviewKeyEvent false
                if (event.type != KeyEventType.KeyDown || event.isCtrlPressed || event.isMetaPressed || event.isAltPressed) {
                    return@onPreviewKeyEvent false
                }
                when (event.key) {
                    Key.Spacebar -> controller.togglePlayPause()
                    Key.DirectionLeft -> controller.stepFrame(-1)
                    Key.DirectionRight -> controller.stepFrame(1)
                    Key.I -> controller.markInAtPlayhead()
                    Key.O -> controller.markOutAtPlayhead()
                    else -> return@onPreviewKeyEvent false
                }
                true
            },
    ) {
        // 1. Clean single-row Studio Header (~38dp)
        StudioHeaderBar(controller = controller, state = state)

        Divider(
            orientation = Orientation.Horizontal,
            modifier = Modifier.fillMaxWidth(),
            color = ArbJewelPalette.hairlineDivider(),
        )

        // Conditional Export Completion Bar (height 0 until export succeeds)
        state?.exportCompletion?.let { completion ->
            ExportCompletionStrip(
                info = completion,
                onReveal = { controller.revealExportedInFinder() },
                onCopy = { controller.copyExportedPath() },
                onOpenTab = { controller.openExportedBundleInNewTab() },
                onCloseTab = { controller.closeStudioTab() },
                onDismiss = { controller.dismissExportCompletion() },
            )
        }

        // Conditional Error Strip
        (controller.timelineWarnings + listOfNotNull(controller.errorMessage, controller.playbackError)).forEach { err ->
            ArbSoftBanner(
                background = ArbJewelPalette.recRedSoft(),
                accent = ArbJewelPalette.recRed(),
                modifier = Modifier.padding(horizontal = 10.dp, vertical = 6.dp),
            ) {
                Text(
                    text = err,
                    style = ArbJewelPalette.bodyStyle(sizeSp = 11.5f, color = ArbJewelPalette.primaryText()),
                    modifier = Modifier.weight(1f),
                )
            }
        }

        // Inline (non-modal) notices: a failed stream is "not loaded", never "0 items"; the video
        // and the other streams keep working while these are shown.
        controller.failedAuxStreams.forEach { failure ->
            ArbSoftBanner(
                background = ArbJewelPalette.warningAmberSoft(),
                accent = ArbJewelPalette.warningAmber(),
                modifier = Modifier.padding(horizontal = 10.dp, vertical = 4.dp),
            ) {
                Text(
                    text = StudioLoadMessages.streamFailureNotice(failure),
                    style = ArbJewelPalette.bodyStyle(sizeSp = 11.5f, color = ArbJewelPalette.primaryText()),
                    modifier = Modifier.weight(1f),
                )
                ArbSubtleAction(
                    label = "再読込",
                    iconKey = AllIconsKeys.Actions.Refresh,
                    compact = true,
                    tooltip = "${failure.stream.label}を再取得します",
                    onClick = { controller.retryAuxStream(failure.stream) },
                )
            }
        }
        controller.editSaveFailure?.let { reason ->
            ArbSoftBanner(
                background = ArbJewelPalette.recRedSoft(),
                accent = ArbJewelPalette.recRed(),
                modifier = Modifier.padding(horizontal = 10.dp, vertical = 4.dp),
            ) {
                Text(
                    text = StudioLoadMessages.editSaveFailureNotice(reason),
                    style = ArbJewelPalette.bodyStyle(sizeSp = 11.5f, color = ArbJewelPalette.primaryText()),
                    modifier = Modifier.weight(1f),
                )
                ArbSubtleAction(
                    label = "再保存",
                    enabled = !controller.editRetryBusy,
                    iconKey = AllIconsKeys.Actions.Refresh,
                    compact = true,
                    tooltip = "IN/OUT 範囲をもう一度保存します",
                    onClick = { controller.retryEditSave() },
                )
            }
        }
        controller.logLimitNotice?.let { notice ->
            ArbSoftBanner(
                background = ArbJewelPalette.warningAmberSoft(),
                accent = ArbJewelPalette.warningAmber(),
                modifier = Modifier.padding(horizontal = 10.dp, vertical = 4.dp),
            ) {
                Text(
                    text = notice,
                    style = ArbJewelPalette.bodyStyle(sizeSp = 11.5f, color = ArbJewelPalette.primaryText()),
                    modifier = Modifier.weight(1f),
                )
            }
        }

        // 2. Resizable Horizontal Main Split (Video Stage | Synchronized Event & Log Stream)
        BoxWithConstraints(
            modifier = Modifier
                .fillMaxWidth()
                .weight(1f),
        ) {
            val totalWidthPx = constraints.maxWidth.toFloat().coerceAtLeast(1f)
            Row(modifier = Modifier.fillMaxSize()) {
                // Left Pane: Neutral Video Viewport + Borderless Transport Bar
                Column(
                    modifier = Modifier
                        .weight(splitFraction.coerceIn(0.30f, 0.78f))
                        .fillMaxHeight()
                        .pointerInput(Unit) {
                            awaitPointerEventScope {
                                while (true) {
                                    if (awaitPointerEvent().type == PointerEventType.Press) {
                                        runCatching { focusRequester.requestFocus() }
                                    }
                                }
                            }
                        },
                ) {
                    VideoStageViewport(
                        controller = controller,
                        state = state,
                        modifier = Modifier
                            .fillMaxWidth()
                            .weight(1f),
                    )
                    Divider(
                        orientation = Orientation.Horizontal,
                        modifier = Modifier.fillMaxWidth(),
                        color = ArbJewelPalette.hairlineDivider(),
                    )
                    TransportBar(controller = controller)
                }

                // Subtle draggable vertical splitter handle
                Box(
                    modifier = Modifier
                        .width(5.dp)
                        .fillMaxHeight()
                        .pointerHoverIcon(PointerIcon(Cursor.getPredefinedCursor(Cursor.E_RESIZE_CURSOR)))
                        .pointerInput(totalWidthPx) {
                            detectHorizontalDragGestures { change, dragAmount ->
                                change.consume()
                                splitFraction = (splitFraction + dragAmount / totalWidthPx).coerceIn(0.30f, 0.78f)
                            }
                        },
                    contentAlignment = Alignment.Center,
                ) {
                    Box(
                        Modifier
                            .width(1.dp)
                            .fillMaxHeight()
                            .background(ArbJewelPalette.hairlineDivider()),
                    )
                }

                // Right Pane: Synchronized Events / Unanchored / Tier-3 Details
                RightEventsAndLogsPane(
                    controller = controller,
                    state = state,
                    onSearchFocusChanged = { searchHasFocus = it },
                    modifier = Modifier
                        .weight((1f - splitFraction).coerceIn(0.22f, 0.70f))
                        .fillMaxHeight(),
                )
            }
        }

        Divider(
            orientation = Orientation.Horizontal,
            modifier = Modifier.fillMaxWidth(),
            color = ArbJewelPalette.hairlineDivider(),
        )

        // 3. Full-Width Bottom Multi-Track Timeline & Trim Dock
        BottomMultiTrackDock(controller = controller, state = state)
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun StudioHeaderBar(
    controller: ArbStudioController,
    state: TimelineViewerState?,
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 10.dp, vertical = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        // Left: Bundle title + muted device/created subtitle
        Column(
            modifier = Modifier.weight(1f),
            verticalArrangement = Arrangement.spacedBy(1.dp),
        ) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Text(
                    text = controller.headerTitle,
                    style = ArbJewelPalette.bodyStyle(sizeSp = 12.5f, bold = true),
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                if (controller.headerSubtitle.isNotBlank()) {
                    Text(
                        text = controller.headerSubtitle,
                        style = ArbJewelPalette.tabularMonoStyle(sizeSp = 10.5f, color = ArbJewelPalette.mutedText()),
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
            // Conditional Tier-2 Honesty Chips (zero height when healthy!)
            val chips = state?.tier2HonestyChips.orEmpty()
            if (chips.isNotEmpty()) {
                FlowRow(
                    horizontalArrangement = Arrangement.spacedBy(4.dp),
                    verticalArrangement = Arrangement.spacedBy(3.dp),
                ) {
                    for (chip in chips) {
                        val (bg, fg) = when (chip.severity) {
                            HonestySeverity.ERROR -> ArbJewelPalette.recRedSoft() to ArbJewelPalette.recRed()
                            HonestySeverity.WARNING -> ArbJewelPalette.warningAmberSoft() to ArbJewelPalette.warningAmber()
                            HonestySeverity.INFO -> ArbJewelPalette.subtleTrackFill() to ArbJewelPalette.mutedText()
                        }
                        ArbSemanticPill(
                            label = chip.label,
                            background = bg,
                            foreground = fg,
                            tooltip = "${chip.detail}（クリックで詳細タブを表示）",
                            onClick = { controller.selectDetailsTab() },
                        )
                    }
                }
            }
        }

        // Right: Borderless PNG / reveal-folder actions + format selector + Single Jewel DefaultButton
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            ArbSubtleAction(
                label = "PNG",
                iconKey = AllIconsKeys.Actions.Dump,
                enabled = controller.hasVideo,
                tooltip = "現在フレームを静止画 PNG として保存",
                compact = true,
                onClick = { controller.requestPngExport() },
            )
            ArbSubtleAction(
                label = "フォルダ",
                iconKey = AllIconsKeys.Nodes.Folder,
                enabled = state != null,
                tooltip = "バンドルフォルダをファイルマネージャで表示",
                compact = true,
                onClick = { controller.revealBundleInFinder() },
            )
            val formats = StudioExportFormat.entries
            key(controller.selectedExportFormat, controller.hasVideo, controller.isExporting) {
                ListComboBox(
                    items = formats.map { it.label },
                    selectedIndex = formats.indexOf(controller.selectedExportFormat).coerceAtLeast(0),
                    onSelectedItemChange = { controller.selectExportFormat(formats[it]) },
                    enabled = controller.hasVideo && !controller.isExporting,
                    modifier = Modifier.width(114.dp),
                )
            }
            DefaultButton(
                onClick = { controller.triggerExport() },
                enabled = controller.isExportButtonEnabled,
            ) {
                Text(
                    text = controller.exportButtonLabel,
                    maxLines = 1,
                    softWrap = false,
                )
            }
        }
    }
}

@Composable
private fun ExportCompletionStrip(
    info: ExportCompletionInfo,
    onReveal: () -> Unit,
    onCopy: () -> Unit,
    onOpenTab: () -> Unit,
    onCloseTab: () -> Unit,
    onDismiss: () -> Unit,
) {
    ArbSoftBanner(
        background = ArbJewelPalette.successGreenSoft(),
        accent = ArbJewelPalette.successGreen(),
        modifier = Modifier.padding(horizontal = 10.dp, vertical = 5.dp),
    ) {
        Text(
            text = info.summaryText,
            style = ArbJewelPalette.bodyStyle(sizeSp = 11.5f, bold = true),
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f),
        )
        ArbSubtleAction(label = "フォルダを開く", iconKey = AllIconsKeys.Nodes.Folder, compact = true, onClick = onReveal)
        ArbSubtleAction(label = "パスをコピー", iconKey = AllIconsKeys.Actions.Copy, compact = true, onClick = onCopy)
        if (info.isTrimBundle && !info.bundleId.isNullOrBlank()) {
            ArbSubtleAction(label = "結果を開く", iconKey = AllIconsKeys.Actions.Execute, compact = true, onClick = onOpenTab)
        }
        ArbSubtleAction(
            label = "タブを閉じる",
            iconKey = AllIconsKeys.Actions.Cancel,
            compact = true,
            tooltip = "このスタジオタブを閉じます",
            onClick = onCloseTab,
        )
        IconActionButton(
            key = AllIconsKeys.Actions.Close,
            contentDescription = "バナーを隠す",
            onClick = onDismiss,
            modifier = Modifier.size(20.dp),
        )
    }
}

@Composable
private fun VideoStageViewport(
    controller: ArbStudioController,
    state: TimelineViewerState?,
    modifier: Modifier = Modifier,
) {
    val stageBg = if (JewelTheme.isDark) {
        ArbJewelPalette.panelBackground().copy(alpha = 0.85f)
    } else {
        ArbJewelPalette.subtleTrackFill()
    }
    val bitmap = controller.frameBitmap
    val activeTaps = controller.activeTapOverlays
    val showTaps = controller.isTapOverlayEnabled
    val displayW = controller.displayWidth
    val displayH = controller.displayHeight
    val frameW = state?.currentFrameMeta?.width?.takeIf { it > 0 } ?: controller.frameImageWidth
    val frameH = state?.currentFrameMeta?.height?.takeIf { it > 0 } ?: controller.frameImageHeight
    val cyanColor = ArbJewelPalette.infoCyan()
    val amberColor = ArbJewelPalette.warningAmber()

    BoxWithConstraints(
        modifier = modifier
            .background(stageBg)
            .padding(8.dp),
        contentAlignment = Alignment.Center,
    ) {
        val availW = constraints.maxWidth.toFloat().coerceAtLeast(1f)
        val availH = constraints.maxHeight.toFloat().coerceAtLeast(1f)

        if (bitmap != null) {
            Image(
                bitmap = bitmap,
                contentDescription = "端末録画フレーム",
                contentScale = ContentScale.Fit,
                modifier = Modifier.fillMaxSize(),
            )

            // Vector Tap/Swipe Overlay normalized by display_width / display_height (T-6H item 2:
            // Tap renders a ripple, Swipe renders a polyline + arrowhead + animated travel point).
            if (showTaps && activeTaps.isNotEmpty() && frameW > 0 && frameH > 0) {
                val scale = minOf(availW / bitmap.width.toFloat(), availH / bitmap.height.toFloat())
                val drawW = (bitmap.width * scale).roundToInt().coerceAtLeast(1)
                val drawH = (bitmap.height * scale).roundToInt().coerceAtLeast(1)
                val drawX = ((availW - drawW) / 2f).roundToInt()
                val drawY = ((availH - drawH) / 2f).roundToInt()
                val drawRect = Rectangle(drawX, drawY, drawW, drawH)
                fun mapPoint(x: Double, y: Double): Pair<Int, Int>? =
                    ArbStudioController.mapTapToCanvasPoint(
                        tapX = x,
                        tapY = y,
                        drawRect = drawRect,
                        videoW = frameW,
                        videoH = frameH,
                        displayW = displayW,
                        displayH = displayH,
                    )
                val mappedTaps = activeTaps.mapNotNull { tap ->
                    mapPoint(tap.x, tap.y)?.let { pt -> tap to pt }
                }
                val mappedSwipeEnds = mappedTaps.associate { (tap, _) ->
                    val endX = tap.endX
                    val endY = tap.endY
                    val endPt = if (tap.gestureKind == GestureKind.SWIPE && endX != null && endY != null) {
                        mapPoint(endX, endY)
                    } else {
                        null
                    }
                    tap.tapIndex1Based to endPt
                }
                val swipeTravel by rememberInfiniteTransition(label = "swipeTravel").animateFloat(
                    initialValue = 0f,
                    targetValue = 1f,
                    animationSpec = infiniteRepeatable(
                        animation = tween(durationMillis = 900),
                        repeatMode = RepeatMode.Restart,
                    ),
                    label = "swipeTravel",
                )

                Canvas(modifier = Modifier.fillMaxSize()) {
                    for ((tap, pt) in mappedTaps) {
                        val (cx, cy) = pt
                        val start = Offset(cx.toFloat(), cy.toFloat())
                        val proximity = (1.0f - (abs(tap.deltaMs).toFloat() / 250f)).coerceIn(0.2f, 1.0f)
                        val endPt = mappedSwipeEnds[tap.tapIndex1Based]

                        if (tap.gestureKind == GestureKind.SWIPE && endPt != null) {
                            val end = Offset(endPt.first.toFloat(), endPt.second.toFloat())
                            val lineAlpha = (proximity * 0.85f).coerceIn(0.25f, 0.9f)
                            drawLine(
                                color = cyanColor.copy(alpha = lineAlpha),
                                start = start,
                                end = end,
                                strokeWidth = 2.5.dp.toPx(),
                            )
                            // Arrowhead at the swipe endpoint.
                            val angle = atan2((end.y - start.y).toDouble(), (end.x - start.x).toDouble())
                            val headLen = 9.dp.toPx()
                            val headSpread = Math.PI / 7.0
                            val left = Offset(
                                (end.x - headLen * cos(angle - headSpread)).toFloat(),
                                (end.y - headLen * sin(angle - headSpread)).toFloat(),
                            )
                            val right = Offset(
                                (end.x - headLen * cos(angle + headSpread)).toFloat(),
                                (end.y - headLen * sin(angle + headSpread)).toFloat(),
                            )
                            val arrowPath = Path().apply {
                                moveTo(end.x, end.y)
                                lineTo(left.x, left.y)
                                moveTo(end.x, end.y)
                                lineTo(right.x, right.y)
                            }
                            drawPath(arrowPath, color = cyanColor.copy(alpha = lineAlpha), style = Stroke(width = 2.5.dp.toPx()))
                            drawCircle(color = cyanColor.copy(alpha = proximity * 0.9f), radius = 4.5.dp.toPx(), center = start)
                            drawCircle(color = cyanColor, radius = 5.dp.toPx(), center = end, style = Stroke(width = 1.5.dp.toPx()))
                            // Animated point traveling start -> end along the swipe.
                            val travel = Offset(
                                start.x + (end.x - start.x) * swipeTravel,
                                start.y + (end.y - start.y) * swipeTravel,
                            )
                            drawCircle(color = Color.White.copy(alpha = proximity), radius = 3.5.dp.toPx(), center = travel)
                        } else {
                            val outerR = (14.dp.toPx() + (1f - proximity) * 14.dp.toPx())
                            val innerR = 4.5.dp.toPx()

                            drawCircle(color = cyanColor.copy(alpha = proximity * 0.28f), radius = outerR, center = start)
                            drawCircle(
                                color = cyanColor.copy(alpha = (proximity * 0.85f).coerceIn(0.25f, 0.9f)),
                                radius = outerR,
                                center = start,
                                style = Stroke(width = 1.75.dp.toPx()),
                            )
                            drawCircle(color = Color.White.copy(alpha = proximity * 0.92f), radius = innerR, center = start)
                            drawCircle(
                                color = cyanColor,
                                radius = innerR,
                                center = start,
                                style = Stroke(width = 1.25.dp.toPx()),
                            )
                        }
                    }
                }

                // Compact index tag (#1, #2) next to each mapped tap ripple
                val tagShape = RoundedCornerShape(4.dp)
                val offsetPx = with(LocalDensity.current) { 18.dp.toPx() }.roundToInt()
                for ((tap, pt) in mappedTaps) {
                    val (cx, cy) = pt
                    Box(
                        modifier = Modifier
                            .align(Alignment.TopStart)
                            .offset {
                                IntOffset(
                                    (cx + offsetPx).coerceIn(drawRect.x, (drawRect.x + drawRect.width - 36).coerceAtLeast(drawRect.x)),
                                    (cy - 10).coerceIn(drawRect.y, (drawRect.y + drawRect.height - 20).coerceAtLeast(drawRect.y)),
                                )
                            }
                            .clip(tagShape)
                            .background(ArbJewelPalette.panelBackground().copy(alpha = 0.85f), tagShape)
                            .padding(horizontal = 4.dp, vertical = 1.dp),
                    ) {
                        Text(
                            text = tap.label,
                            style = ArbJewelPalette.tabularMonoStyle(sizeSp = 9.5f, bold = true, color = cyanColor),
                            maxLines = 1,
                        )
                    }
                }
            }
        } else {
            Text(
                text = if (state == null) "バンドルを読み込み中..." else "表示するフレームがありません",
                style = ArbJewelPalette.bodyStyle(sizeSp = 12f, color = ArbJewelPalette.mutedText()),
            )
        }

        // Mandatory SPEC-03 Approximate Preview Pill (visible ONLY while scrubbing / approximate!)
        if (controller.approximatePreviewVisible) {
            val pillShape = RoundedCornerShape(6.dp)
            Box(
                modifier = Modifier
                    .align(Alignment.TopStart)
                    .padding(6.dp)
                    .clip(pillShape)
                    .background(ArbJewelPalette.panelBackground().copy(alpha = 0.88f), pillShape)
                    .padding(horizontal = 10.dp, vertical = 4.dp),
            ) {
                Text(
                    text = APPROXIMATE_PREVIEW_BADGE_TEXT,
                    style = ArbJewelPalette.bodyStyle(sizeSp = 11f, bold = true, color = amberColor),
                    maxLines = 1,
                )
            }
        }
    }
}

@Composable
private fun TransportBar(controller: ArbStudioController) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 8.dp, vertical = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.SpaceBetween,
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(2.dp),
        ) {
            ArbSubtleAction(
                label = controller.playPauseLabel,
                iconKey = if (controller.isPlaying) AllIconsKeys.Actions.Pause else AllIconsKeys.Actions.Execute,
                enabled = controller.hasVideo,
                tooltip = "再生 / 一時停止 (Space)",
                compact = true,
                onClick = { controller.togglePlayPause() },
            )
            ArbSubtleAction(
                label = "-1f",
                iconKey = AllIconsKeys.Actions.Back,
                enabled = controller.hasVideo,
                tooltip = "1 フレーム戻る (←)",
                compact = true,
                onClick = { controller.stepFrame(-1) },
            )
            ArbSubtleAction(
                label = "+1f",
                iconKey = AllIconsKeys.Actions.Forward,
                enabled = controller.hasVideo,
                tooltip = "1 フレーム進む (→)",
                compact = true,
                onClick = { controller.stepFrame(1) },
            )
            ArbSubtleAction(
                label = controller.playbackState.speed.label,
                enabled = controller.hasVideo,
                tooltip = "再生速度を切替 (0.5x / 1x / 2x)",
                compact = true,
                onClick = { controller.cycleSpeed() },
            )
            ArbSubtleAction(
                label = controller.muteLabel,
                enabled = controller.muteEnabled,
                tooltip = "音声ミュート切替",
                compact = true,
                onClick = { controller.toggleMute() },
            )
            ArbSubtleAction(
                label = "ループ",
                iconKey = AllIconsKeys.Actions.Refresh,
                active = controller.isLoopEnabled,
                enabled = controller.hasVideo,
                tooltip = controller.loopTooltip,
                compact = true,
                onClick = { controller.toggleLoop() },
            )
            ArbSubtleAction(
                label = "タップ表示",
                active = controller.isTapOverlayEnabled,
                enabled = controller.hasVideo,
                tooltip = "映像上のタップ波紋オーバーレイを表示 / 非表示",
                compact = true,
                onClick = { controller.toggleTapOverlay() },
            )
        }

        PlaybackTimecode(controller)
    }
}

@Composable
private fun PlaybackTimecode(controller: ArbStudioController) {
    var playingTime by remember(controller, controller.isPlaying) { mutableStateOf(controller.timecodeText) }
    LaunchedEffect(controller, controller.isPlaying) {
        while (controller.isPlaying) {
            playingTime = controller.timecodeText
            kotlinx.coroutines.delay(50L)
        }
    }
    Text(
        text = if (controller.isPlaying) playingTime else controller.timecodeText,
        style = ArbJewelPalette.tabularMonoStyle(sizeSp = 11f, bold = true),
        maxLines = 1,
        softWrap = false,
    )
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun RightEventsAndLogsPane(
    controller: ArbStudioController,
    state: TimelineViewerState?,
    onSearchFocusChanged: (Boolean) -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.background(ArbJewelPalette.panelBackground())) {
        // Tab strip: 操作＆ログ / 時刻不明 (N) / 詳細
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 8.dp, vertical = 5.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            ArbFilterTab(
                label = "操作＆ログ",
                selected = controller.activeTab == StudioRightTab.EVENTS_AND_LOGS,
                onClick = { controller.selectTab(StudioRightTab.EVENTS_AND_LOGS) },
            )
            ArbFilterTab(
                label = controller.unanchoredTabLabel,
                selected = controller.activeTab == StudioRightTab.UNANCHORED,
                onClick = { controller.selectTab(StudioRightTab.UNANCHORED) },
            )
            ArbFilterTab(
                label = "詳細",
                selected = controller.activeTab == StudioRightTab.DETAILS,
                onClick = { controller.selectTab(StudioRightTab.DETAILS) },
            )
            if (controller.activeTab == StudioRightTab.EVENTS_AND_LOGS) {
                ArbSpacerH(width = 4.dp)
                Box(modifier = Modifier.weight(1f))
                ArbTooltip("再生位置に合わせてログを自動スクロール") {
                    ArbLabeledCheckbox(
                        text = "自動追従",
                        checked = controller.followPlayhead,
                        onCheckedChange = { controller.selectFollowPlayhead(it) },
                        muted = true,
                    )
                }
            }
        }

        Divider(
            orientation = Orientation.Horizontal,
            modifier = Modifier.fillMaxWidth(),
            color = ArbJewelPalette.hairlineDivider(),
        )

        when (controller.activeTab) {
            StudioRightTab.EVENTS_AND_LOGS -> {
                // Filter pills + search TextField
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(horizontal = 8.dp, vertical = 5.dp),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(4.dp),
                ) {
                    for (filter in EventStreamFilter.entries) {
                        ArbFilterTab(
                            label = filter.label,
                            selected = controller.eventFilter == filter,
                            onClick = { controller.selectEventFilter(filter) },
                        )
                    }
                    TextField(
                        state = controller.searchFieldState,
                        placeholder = { Text("検索...", style = ArbJewelPalette.bodyStyle(11f, color = ArbJewelPalette.mutedText())) },
                        modifier = Modifier
                            .weight(1f)
                            .onFocusChanged { onSearchFocusChanged(it.isFocused) },
                    )
                }

                // Per-app extracted log filter (`app-logs/<package>.jsonl`); hidden for legacy
                // bundles and bundles without foreground/crashed app extraction.
                if (controller.isAppPackageFilterVisible) {
                    AppPackageFilterRow(controller)
                }

                Divider(
                    orientation = Orientation.Horizontal,
                    modifier = Modifier.fillMaxWidth(),
                    color = ArbJewelPalette.hairlineDivider(),
                )

                val rows = controller.syncedRows
                Box(modifier = Modifier.weight(1f).fillMaxWidth()) {
                    if (rows.isEmpty()) {
                        Box(Modifier.fillMaxSize().padding(16.dp), contentAlignment = Alignment.Center) {
                            Text(
                                text = "該当する操作・ログがありません",
                                style = ArbJewelPalette.bodyStyle(11.5f, color = ArbJewelPalette.mutedText()),
                            )
                        }
                    } else {
                        val listState = rememberLazyListState()
                        val jewelSb = JewelTheme.scrollbarStyle
                        val rowKeys = remember(rows) { ArbStudioController.uniqueKeys(rows, ArbStudioController::syncedRowKey) }
                        val activeIndex = controller.activeTimelineItemIndex
                        val activeRowKey = activeIndex?.let { rowKeys.getOrNull(it) }
                        val followPlayhead = controller.followPlayhead

                        // Key on `(followPlayhead, activeIndex, activeRowKey)` rather than `rows`
                        // so per-frame `isAtPlayhead` / trim-handle flag rebuilds don't cancel and
                        // restart `animateScrollToItem` while the active row itself hasn't changed.
                        LaunchedEffect(followPlayhead, activeIndex, activeRowKey) {
                            if (followPlayhead && activeIndex != null && activeIndex in rows.indices) {
                                // Small upper-third offset keeps a bit of recent context above the
                                // active row while still showing upcoming entries below it.
                                val viewportHeight = listState.layoutInfo.viewportSize.height
                                val offsetPx = -(viewportHeight / 3)
                                listState.animateScrollToItem(activeIndex, scrollOffset = offsetPx)
                            }
                        }

                        LazyColumn(
                            state = listState,
                            modifier = Modifier.fillMaxSize().padding(horizontal = 4.dp, vertical = 2.dp),
                            verticalArrangement = Arrangement.spacedBy(1.dp),
                        ) {
                            items(rows.size, key = { rowKeys[it] }) { rowIndex ->
                                val row = rows[rowIndex]
                                SyncedEventItemRow(
                                    row = row,
                                    isAutoFollowActive = followPlayhead && rowIndex == activeIndex,
                                    onSeek = { row.frameIndex?.let(controller::seekExactFrame) },
                                    onSnapIn = { row.frameIndex?.let { controller.applyInFrame(it, settled = true) } },
                                    onSnapOut = { row.frameIndex?.let { controller.applyOutFrame(it, settled = true) } },
                                    onSourceJump = { row.sourceJumpTarget?.let(controller::jumpToSource) },
                                )
                            }
                        }
                        VerticalScrollbar(
                            adapter = rememberScrollbarAdapter(listState),
                            modifier = Modifier.align(Alignment.CenterEnd).fillMaxHeight(),
                            style = defaultScrollbarStyle().copy(
                                thickness = jewelSb.scrollbarVisibility.trackThickness,
                                shape = RoundedCornerShape(jewelSb.metrics.thumbCornerSize),
                                unhoverColor = jewelSb.colors.thumbOpaqueBackground.copy(alpha = 0.45f),
                                hoverColor = jewelSb.colors.thumbBackgroundActive,
                            ),
                        )
                    }
                }
            }

            StudioRightTab.UNANCHORED -> {
                state?.unanchoredTruncatedNotice?.let { notice ->
                    ArbSoftBanner(
                        background = ArbJewelPalette.warningAmberSoft(),
                        accent = ArbJewelPalette.warningAmber(),
                        modifier = Modifier.padding(8.dp),
                    ) {
                        Text(
                            text = notice,
                            style = ArbJewelPalette.bodyStyle(11f, bold = true),
                        )
                    }
                }
                val records = state?.unanchoredLogs.orEmpty()
                if (records.isEmpty()) {
                    Box(Modifier.fillMaxSize().padding(16.dp), contentAlignment = Alignment.Center) {
                        Text(
                            text = "時刻不明の記録はありません",
                            style = ArbJewelPalette.bodyStyle(11.5f, color = ArbJewelPalette.mutedText()),
                        )
                    }
                } else {
                    val listState = rememberLazyListState()
                    val recordKeys = remember(records) { ArbStudioController.uniqueKeys(records) { "u-${it.seq}" } }
                    LazyColumn(
                        state = listState,
                        modifier = Modifier.fillMaxSize().padding(horizontal = 6.dp, vertical = 4.dp),
                        verticalArrangement = Arrangement.spacedBy(2.dp),
                    ) {
                        items(records.size, key = { recordKeys[it] }) { recIndex ->
                            UnanchoredLogRow(records[recIndex])
                        }
                    }
                }
            }

            StudioRightTab.DETAILS -> {
                Tier3DetailsTab(state = state)
            }
        }
    }
}

@Composable
private fun AppPackageFilterRow(controller: ArbStudioController) {
    val options = controller.appPackageFilterOptions
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(start = 10.dp, end = 8.dp, bottom = 5.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        Text(
            text = "アプリ",
            style = ArbJewelPalette.bodyStyle(sizeSp = 11f, color = ArbJewelPalette.mutedText()),
        )
        val labels = options.map { it.label }
        val selectedIdx = controller.selectedAppPackageIndex
        key(labels, selectedIdx) {
            ListComboBox(
                items = labels,
                selectedIndex = selectedIdx,
                onSelectedItemChange = { controller.selectAppPackageByIndex(it) },
                modifier = Modifier
                    .weight(1f)
                    .semantics { contentDescription = "アプリ別ログフィルタ" },
            )
        }
        if (controller.appPackageFilterLoading) {
            Text(
                text = "読み込み中...",
                style = ArbJewelPalette.bodyStyle(sizeSp = 10.5f, color = ArbJewelPalette.mutedText()),
                maxLines = 1,
            )
        }
    }
}

@Composable
private fun SyncedEventItemRow(
    row: SyncedEventRow,
    onSeek: () -> Unit,
    onSnapIn: () -> Unit,
    onSnapOut: () -> Unit,
    onSourceJump: () -> Unit,
    isAutoFollowActive: Boolean = false,
) {
    val interactionSource = remember { MutableInteractionSource() }
    val hovered by interactionSource.collectIsHoveredAsState()
    val isCorrelatedLog = row.kind == SyncedEventKind.LOG && row.correlatedTapIndex != null

    // Owner trial feedback T-6H item 4: the auto-follow active row gets its own wider accent bar
    // so the video <-> log sync position is unmistakable even when `isAtPlayhead` (exact frame
    // match) never fires between discrete events.
    val stripeColor = when {
        isAutoFollowActive -> ArbJewelPalette.accent()
        row.kind == SyncedEventKind.CRASH -> ArbJewelPalette.recRed()
        isCorrelatedLog && row.isInTrimRange -> ArbJewelPalette.infoCyan()
        row.isInTrimRange -> ArbJewelPalette.accent()
        else -> Color.Transparent
    }
    val stripeWidth = if (isAutoFollowActive) 3.5.dp else 2.5.dp
    val rowBg = when {
        isAutoFollowActive -> ArbJewelPalette.accentSoft()
        row.isAtPlayhead -> ArbJewelPalette.accentSoft()
        hovered -> ArbJewelPalette.subtleHoverFill()
        isCorrelatedLog && row.isInTrimRange -> ArbJewelPalette.infoCyanSoft().copy(alpha = 0.45f)
        else -> Color.Transparent
    }
    val primaryColor = if (row.isInTrimRange) ArbJewelPalette.primaryText() else ArbJewelPalette.dimmedText()
    val secondaryColor = if (row.isInTrimRange) ArbJewelPalette.mutedText() else ArbJewelPalette.dimmedText()
    val shape = RoundedCornerShape(5.dp)

    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clip(shape)
            .background(rowBg, shape)
            .hoverable(interactionSource)
            .clickable(
                interactionSource = interactionSource,
                indication = null,
                role = Role.Button,
                onClick = onSeek,
            )
            .padding(start = if (isCorrelatedLog) 10.dp else 4.dp, end = 6.dp, top = 4.dp, bottom = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        // Subtle vertical trim-range / crash / auto-follow-active indicator bar
        Box(
            Modifier
                .width(stripeWidth)
                .height(24.dp)
                .clip(RoundedCornerShape(2.dp))
                .background(stripeColor),
        )

        Text(
            text = row.relativeMs?.let(ArbFormatters::formatTimecode) ?: "--:--.--",
            style = ArbJewelPalette.tabularMonoStyle(sizeSp = 10f, color = secondaryColor),
            maxLines = 1,
            softWrap = false,
        )

        val (badgeText, badgeBg, badgeFg) = when (row.kind) {
            SyncedEventKind.TAP -> Triple("TAP", ArbJewelPalette.infoCyanSoft(), ArbJewelPalette.infoCyan())
            SyncedEventKind.CRASH -> Triple("CRASH", ArbJewelPalette.recRedSoft(), ArbJewelPalette.recRed())
            SyncedEventKind.SCREEN_TRANSITION -> Triple("SCR", ArbJewelPalette.accentSoft(), ArbJewelPalette.accent())
            SyncedEventKind.LOG -> {
                val lvl = row.level?.uppercase()
                when (lvl) {
                    "E", "F" -> Triple(lvl, ArbJewelPalette.recRedSoft(), ArbJewelPalette.recRed())
                    "W" -> Triple("W", ArbJewelPalette.warningAmberSoft(), ArbJewelPalette.warningAmber())
                    else -> Triple(lvl ?: "I", ArbJewelPalette.subtleTrackFill(), secondaryColor)
                }
            }
        }
        ArbSemanticPill(
            label = badgeText,
            background = badgeBg,
            foreground = if (row.isInTrimRange) badgeFg else ArbJewelPalette.dimmedText(),
        )

        if (isCorrelatedLog) {
            ArbSemanticPill(
                label = "Tap #${row.correlatedTapIndex}",
                background = ArbJewelPalette.infoCyanSoft(),
                foreground = if (row.isInTrimRange) ArbJewelPalette.infoCyan() else ArbJewelPalette.dimmedText(),
            )
        }

        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = row.title,
                style = ArbJewelPalette.bodyStyle(sizeSp = 11.5f, bold = row.kind != SyncedEventKind.LOG, color = primaryColor),
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            if (row.detail.isNotBlank()) {
                Text(
                    text = row.detail,
                    style = ArbJewelPalette.tabularMonoStyle(sizeSp = 10.5f, color = secondaryColor),
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }

        // Clickable PSI Source Jump chip
        row.sourceJumpTarget?.let { target ->
            ArbSemanticPill(
                label = target.displayLabel,
                background = ArbJewelPalette.accentSoft(),
                foreground = ArbJewelPalette.accent(),
                tooltip = "Android Studio で該当ソースへジャンプ (${target.displayLabel})",
                onClick = onSourceJump,
            )
        }

        // Hover [In] / [Out] snap buttons
        if (hovered && row.frameIndex != null) {
            ArbSubtleAction(label = "[In]", compact = true, tooltip = "このイベントを開始点 (In) に設定", onClick = onSnapIn)
            ArbSubtleAction(label = "[Out]", compact = true, tooltip = "このイベントを終了点 (Out) に設定", onClick = onSnapOut)
        }
    }
}

@Composable
private fun UnanchoredLogRow(rec: LogRecord) {
    val certainty = when (val a = rec.alignment) {
        is TimeAlignment.Unanchored -> "時刻不明 (${a.reason.wire})"
        is TimeAlignment.Bounded -> "区間 (${a.reason.wire})"
        is TimeAlignment.Anchored -> "確定"
    }
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 6.dp, vertical = 3.dp),
        horizontalArrangement = Arrangement.spacedBy(6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text("#${rec.seq}", style = ArbJewelPalette.tabularMonoStyle(10f, color = ArbJewelPalette.mutedText()))
        ArbSemanticPill(
            label = certainty,
            background = ArbJewelPalette.warningAmberSoft(),
            foreground = ArbJewelPalette.warningAmber(),
        )
        Text(
            text = "${rec.level ?: "-"} ${rec.tag ?: "-"}: ${rec.message}",
            style = ArbJewelPalette.tabularMonoStyle(10.5f),
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f),
        )
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun Tier3DetailsTab(state: TimelineViewerState?) {
    if (state == null) {
        Box(Modifier.fillMaxSize().padding(16.dp), contentAlignment = Alignment.Center) {
            Text("読み込み中...", style = ArbJewelPalette.bodyStyle(11.5f, color = ArbJewelPalette.mutedText()))
        }
        return
    }
    val prov = ProvenanceFormatter.format(state.summary)
    val epSummary = if (state.summary.epochs.isEmpty()) {
        "boottime_ns: 情報なし"
    } else {
        state.summary.epochs.joinToString("\n") { ep ->
            "boot=${ep.bootId} [${ep.startBoottimeNs}..${ep.endBoottimeNs} ns]"
        }
    }

    SelectionContainer {
        ArbScrollableColumn(modifier = Modifier.fillMaxSize()) {
            Column(
                modifier = Modifier.fillMaxWidth().padding(12.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Text("来歴・ストリーム誠実性 (Tier 3)", style = ArbJewelPalette.bodyStyle(11.5f, bold = true))
                TechDetailLine(prov.provenanceLine)
                TechDetailLine(prov.videoSemanticsLine)
                TechDetailLine(prov.audioOffsetLine)
                TechDetailLine(prov.logcatCompletenessLine)
                TechDetailLine(prov.unanchoredLine)

                Divider(orientation = Orientation.Horizontal, modifier = Modifier.fillMaxWidth())

                Text("内部メタデータ (boottime_ns / Lease / Path)", style = ArbJewelPalette.bodyStyle(11.5f, bold = true))
                TechDetailLine(epSummary)
                TechDetailLine("Lease=${state.leaseId}")
                TechDetailLine("Path=${state.path}")

                Divider(orientation = Orientation.Horizontal, modifier = Modifier.fillMaxWidth())

                Text("不連続・欠損イベント", style = ArbJewelPalette.bodyStyle(11.5f, bold = true))
                if (state.discontinuities.isEmpty()) {
                    Text(
                        text = when (state.streamState(AuxStream.DISCONTINUITIES)) {
                            StreamLoadState.Loaded -> "不連続・欠損イベントなし"
                            StreamLoadState.Pending -> "不連続・欠損イベントを取得中"
                            is StreamLoadState.Failed -> "不連続・欠損イベントは未取得"
                        },
                        style = ArbJewelPalette.bodyStyle(11f, color = ArbJewelPalette.mutedText()),
                    )
                } else {
                    FlowRow(
                        horizontalArrangement = Arrangement.spacedBy(4.dp),
                        verticalArrangement = Arrangement.spacedBy(4.dp),
                    ) {
                        for (ev in state.discontinuities) {
                            DiscontinuityPill(ev)
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun TechDetailLine(text: String) {
    Text(
        text = text,
        style = ArbJewelPalette.tabularMonoStyle(sizeSp = 10.5f, color = ArbJewelPalette.mutedText()),
    )
}

@Composable
private fun DiscontinuityPill(event: DiscontinuityEvent) {
    val (bg, fg) = when (event.kind) {
        DiscontinuityKind.DEVICE_REBOOT,
        DiscontinuityKind.STORAGE_PRESSURE_STOP,
        -> ArbJewelPalette.recRedSoft() to ArbJewelPalette.recRed()
        DiscontinuityKind.CONNECTION_LOST,
        DiscontinuityKind.STREAM_GAP,
        DiscontinuityKind.DEVICE_WALL_JUMP,
        DiscontinuityKind.CLOCK_SYNC_STALE,
        -> ArbJewelPalette.warningAmberSoft() to ArbJewelPalette.warningAmber()
        DiscontinuityKind.DEVICE_SLEEP,
        DiscontinuityKind.PC_SLEEP,
        DiscontinuityKind.STATIC_HOLD,
        -> ArbJewelPalette.infoCyanSoft() to ArbJewelPalette.infoCyan()
    }
    ArbSemanticPill(
        label = DiscontinuityFormatter.label(event.kind),
        background = bg,
        foreground = fg,
        tooltip = DiscontinuityFormatter.describe(event),
    )
}

// ---- Bottom Multi-Track Timeline & Trim Dock ----

private enum class TimelineDragTarget {
    NONE,
    PLAYHEAD,
    IN_HANDLE,
    OUT_HANDLE,
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun BottomMultiTrackDock(
    controller: ArbStudioController,
    state: TimelineViewerState?,
) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .background(ArbJewelPalette.panelBackground()),
    ) {
        MultiTrackTimelineCanvas(
            controller = controller,
            state = state,
        )

        Divider(
            orientation = Orientation.Horizontal,
            modifier = Modifier.fillMaxWidth(),
            color = ArbJewelPalette.hairlineDivider(),
        )

        // Compact Bottom Control Strip
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 10.dp, vertical = 5.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.SpaceBetween,
        ) {
            FlowRow(
                horizontalArrangement = Arrangement.spacedBy(2.dp),
                verticalArrangement = Arrangement.spacedBy(2.dp),
            ) {
                ArbSubtleAction(
                    label = "[I] 開始点",
                    enabled = controller.hasVideo,
                    compact = true,
                    tooltip = "現在の再生位置をトリム開始点に設定 (I)",
                    onClick = { controller.markInAtPlayhead() },
                )
                ArbSubtleAction(
                    label = "[O] 終了点",
                    enabled = controller.hasVideo,
                    compact = true,
                    tooltip = "現在の再生位置をトリム終了点に設定 (O)",
                    onClick = { controller.markOutAtPlayhead() },
                )
                if (controller.isSelectTapRangeVisible) {
                    ArbSubtleAction(
                        label = ArbFormatters.PRESET_TAP_RANGE_LABEL,
                        enabled = controller.hasVideo,
                        compact = true,
                        tooltip = "最初〜最後のタップ -1s/+2s を自動選択",
                        onClick = { controller.selectTapRange() },
                    )
                }
                if (controller.isSelectAroundCrashVisible) {
                    ArbSubtleAction(
                        label = ArbFormatters.PRESET_CRASH_RANGE_LABEL,
                        iconKey = AllIconsKeys.General.Error,
                        enabled = controller.hasVideo,
                        compact = true,
                        tint = ArbJewelPalette.recRed(),
                        tooltip = "am_crash の前後 -5s/+2s を自動選択",
                        onClick = { controller.selectAroundCrash() },
                    )
                }
                ArbSubtleAction(
                    label = ArbFormatters.PRESET_RESET_RANGE_LABEL,
                    enabled = controller.hasVideo,
                    compact = true,
                    onClick = { controller.resetFullRange() },
                )
            }

            FlowRow(
                horizontalArrangement = Arrangement.spacedBy(12.dp),
                verticalArrangement = Arrangement.spacedBy(2.dp),
            ) {
                if (controller.isBurnTapsVisible) {
                    ArbTooltip("共有用 MP4 (share-taps.mp4) にタップ波紋とスワイプ軌跡を焼き込みます（元の video.mp4 は無劣化のまま保持）") {
                        ArbLabeledCheckbox(
                            text = ArbStudioController.BURN_TAPS_TOGGLE_LABEL,
                            checked = controller.burnTapsOnExport,
                            onCheckedChange = { controller.toggleBurnTapsOnExport(it) },
                            enabled = controller.hasVideo && !controller.isExporting,
                        )
                    }
                }
                ArbTooltip(
                    if (state?.isLegacy == true) {
                        "Legacy (RO) バンドルは読取専用のためゴミ箱へ移動できません"
                    } else {
                        "書き出しの検証完了後に元のバンドルを非破壊でゴミ箱へ移動します"
                    },
                ) {
                    ArbLabeledCheckbox(
                        text = ArbFormatters.TRASH_ORIGINAL_TOGGLE_LABEL,
                        checked = controller.isTrashOriginalSelected,
                        onCheckedChange = { controller.toggleTrashOriginal(it) },
                        enabled = controller.isTrashOriginalEnabled && !controller.isExporting,
                    )
                }
            }
        }
    }
}

@Composable
private fun MultiTrackTimelineCanvas(
    controller: ArbStudioController,
    state: TimelineViewerState?,
) {
    val currentState by rememberUpdatedState(state)
    val density = LocalDensity.current

    val padHPx = with(density) { 14.dp.toPx() }
    val rulerHeightPx = with(density) { 20.dp.toPx() }
    val filmstripTopPx = rulerHeightPx + with(density) { 4.dp.toPx() }
    val filmstripHeightPx = with(density) { 46.dp.toPx() }
    val markerTopPx = filmstripTopPx + filmstripHeightPx + with(density) { 6.dp.toPx() }
    val markerHeightPx = with(density) { 34.dp.toPx() }
    val handleWidthPx = with(density) { 10.dp.toPx() }
    val handleHitPx = with(density) { 12.dp.toPx() }

    val trackFill = ArbJewelPalette.subtleTrackFill()
    val hairline = ArbJewelPalette.hairlineDivider()
    val trimAccent = ArbJewelPalette.trimAccent()
    val onTrimHandle = ArbJewelPalette.trimHandleGrip()
    val dimMask = ArbJewelPalette.panelBackground().copy(alpha = if (JewelTheme.isDark) 0.62f else 0.52f)
    val playheadColor = ArbJewelPalette.primaryText()
    val playheadOutline = ArbJewelPalette.panelBackground()
    val cyan = ArbJewelPalette.infoCyan()
    val red = ArbJewelPalette.recRed()
    val amber = ArbJewelPalette.warningAmber()
    val mutedText = ArbJewelPalette.mutedText()

    val screenColors = List(4) { idx -> ArbJewelPalette.screenSpanColor(idx) }

    BoxWithConstraints(
        modifier = Modifier
            .fillMaxWidth()
            .height(118.dp),
    ) {
        val widthPx = constraints.maxWidth.toFloat().coerceAtLeast(1f)
        val trackW = (widthPx - padHPx * 2f).coerceAtLeast(1f)

        fun frameToX(frameIndex: Long, totalFrames: Long): Float {
            if (totalFrames <= 1L) return padHPx
            val ratio = frameIndex.coerceIn(0L, totalFrames - 1L).toFloat() / (totalFrames - 1L).toFloat()
            return padHPx + ratio * trackW
        }

        fun xToFrame(x: Float, totalFrames: Long): Long {
            if (totalFrames <= 1L) return 0L
            val ratio = ((x - padHPx) / trackW).coerceIn(0f, 1f)
            return (ratio * (totalFrames - 1L).toDouble()).roundToLong().coerceIn(0L, totalFrames - 1L)
        }

        Canvas(
            modifier = Modifier
                .fillMaxSize()
                .semantics { contentDescription = "マルチトラックタイムライン" }
                .pointerInput(widthPx) {
                    awaitEachGesture {
                        val down = awaitFirstDown()
                        val s = currentState ?: return@awaitEachGesture
                        if (s.frameCount <= 0L) return@awaitEachGesture
                        val x = down.position.x
                        val y = down.position.y

                        // Track 2 click: check Activity Screen Span or Tap/Crash marker pin snap
                        if (y in markerTopPx..(markerTopPx + markerHeightPx)) {
                            val spanHit = s.screens.firstOrNull { span ->
                                val sf = span.startFrameIndex ?: s.boottimeNsToFrameIndex(span.startNs) ?: return@firstOrNull false
                                val ef = span.endFrameIndex ?: s.boottimeNsToFrameIndex(span.endNs) ?: sf
                                val x1 = frameToX(sf, s.frameCount)
                                val x2 = frameToX(ef, s.frameCount).coerceAtLeast(x1 + 8f)
                                x in x1..x2
                            }
                            if (spanHit != null && y <= markerTopPx + markerHeightPx * 0.55f) {
                                val target = spanHit.startFrameIndex ?: s.boottimeNsToFrameIndex(spanHit.startNs)
                                if (target != null) {
                                    controller.seekExactFrame(target)
                                    down.consume()
                                    return@awaitEachGesture
                                }
                            }
                            // Snap to nearest tap or crash marker pin within 7dp
                            val pinSnapRadiusPx = 7.dp.toPx()
                            val markerFrames = buildList<Pair<Float, Long>> {
                                for (tap in s.mainTaps) {
                                    val bt = TimelineViewerState.alignmentBoottimeNs(tap.alignment) ?: continue
                                    val f = s.boottimeNsToFrameIndex(bt) ?: continue
                                    add(frameToX(f, s.frameCount) to f)
                                }
                                for (crash in s.crashes) {
                                    val f = crash.frameIndexHint ?: crash.boottimeNs?.let(s::boottimeNsToFrameIndex) ?: continue
                                    add(frameToX(f, s.frameCount) to f)
                                }
                            }
                            val nearestPin = markerFrames.minByOrNull { abs(it.first - x) }
                            if (nearestPin != null && abs(nearestPin.first - x) <= pinSnapRadiusPx) {
                                controller.seekExactFrame(nearestPin.second)
                                down.consume()
                                return@awaitEachGesture
                            }
                        }

                        val inX = frameToX(s.inFrameIndex, s.frameCount)
                        val outX = frameToX(s.outFrameIndex, s.frameCount)
                        val dragTarget = when {
                            y in (filmstripTopPx - 6f)..(filmstripTopPx + filmstripHeightPx + 6f) &&
                                abs(x - inX) <= handleHitPx -> TimelineDragTarget.IN_HANDLE
                            y in (filmstripTopPx - 6f)..(filmstripTopPx + filmstripHeightPx + 6f) &&
                                abs(x - outX) <= handleHitPx -> TimelineDragTarget.OUT_HANDLE
                            else -> TimelineDragTarget.PLAYHEAD
                        }

                        fun dispatch(posX: Float, settled: Boolean) {
                            val st = currentState ?: return
                            val targetFrame = xToFrame(posX, st.frameCount)
                            when (dragTarget) {
                                TimelineDragTarget.IN_HANDLE -> {
                                    controller.applyInFrame(targetFrame, settled)
                                    if (settled) controller.seekExactFrame(targetFrame) else controller.scrubToFrame(targetFrame)
                                }
                                TimelineDragTarget.OUT_HANDLE -> {
                                    controller.applyOutFrame(targetFrame, settled)
                                    if (settled) controller.seekExactFrame(targetFrame) else controller.scrubToFrame(targetFrame)
                                }
                                TimelineDragTarget.PLAYHEAD -> {
                                    if (settled) controller.seekExactFrame(targetFrame) else controller.scrubToFrame(targetFrame)
                                }
                                TimelineDragTarget.NONE -> Unit
                            }
                        }

                        dispatch(x, settled = false)
                        down.consume()
                        var lastX = x
                        while (true) {
                            val event = awaitPointerEvent()
                            val change = event.changes.firstOrNull() ?: break
                            if (!change.pressed) break
                            lastX = change.position.x
                            dispatch(lastX, settled = false)
                            change.consume()
                        }
                        dispatch(lastX, settled = true)
                    }
                },
        ) {
            val s = state
            // 1. Subtle ruler line & ticks
            drawLine(hairline, Offset(padHPx, rulerHeightPx), Offset(padHPx + trackW, rulerHeightPx), strokeWidth = 1f)
            if (s != null && s.frameCount > 0L) {
                val ticks = (trackW / 90.dp.toPx()).toInt().coerceIn(3, 12)
                for (i in 0..ticks) {
                    val tx = padHPx + (i.toFloat() / ticks.toFloat()) * trackW
                    drawLine(hairline, Offset(tx, rulerHeightPx - 5.dp.toPx()), Offset(tx, rulerHeightPx), strokeWidth = 1f)
                }
            }

            // 2. Track 1: Filmstrip band
            val trackRadius = CornerRadius(6.dp.toPx(), 6.dp.toPx())
            drawRoundRect(
                color = trackFill,
                topLeft = Offset(padHPx, filmstripTopPx),
                size = Size(trackW, filmstripHeightPx),
                cornerRadius = trackRadius,
            )

            if (s != null && s.frameCount > 0L) {
                clipRect(padHPx, filmstripTopPx, padHPx + trackW, filmstripTopPx + filmstripHeightPx) {
                    val slotCount = s.filmstrip.size.coerceIn(6, 20)
                    val slotW = trackW / slotCount.toFloat()
                    for (i in 0 until slotCount) {
                        val sx = padHPx + i * slotW
                        if (i % 2 == 0) {
                            drawRect(
                                color = Color.White.copy(alpha = 0.03f),
                                topLeft = Offset(sx, filmstripTopPx),
                                size = Size(slotW, filmstripHeightPx),
                            )
                        }
                        if (i > 0) {
                            drawLine(
                                color = hairline.copy(alpha = 0.4f),
                                start = Offset(sx, filmstripTopPx + 2f),
                                end = Offset(sx, filmstripTopPx + filmstripHeightPx - 2f),
                                strokeWidth = 1f,
                            )
                        }
                    }

                    // Keyframe tick marks
                    for (kf in s.filmstrip) {
                        val kx = frameToX(kf.frameIndex, s.frameCount)
                        drawLine(
                            color = mutedText.copy(alpha = 0.55f),
                            start = Offset(kx, filmstripTopPx + filmstripHeightPx - 5.dp.toPx()),
                            end = Offset(kx, filmstripTopPx + filmstripHeightPx),
                            strokeWidth = 1.dp.toPx(),
                        )
                    }

                    val inX = frameToX(s.inFrameIndex, s.frameCount)
                    val outX = frameToX(s.outFrameIndex, s.frameCount)

                    // Dimmed out-of-range overlays
                    if (inX > padHPx) {
                        drawRect(dimMask, Offset(padHPx, filmstripTopPx), Size(inX - padHPx, filmstripHeightPx))
                    }
                    if (outX < padHPx + trackW) {
                        drawRect(dimMask, Offset(outX, filmstripTopPx), Size((padHPx + trackW) - outX, filmstripHeightPx))
                    }
                }

                // QuickTime-inspired soft rounded warm gold dual In/Out trim frame & handles
                val inX = frameToX(s.inFrameIndex, s.frameCount)
                val outX = frameToX(s.outFrameIndex, s.frameCount)
                val borderW = 2.5.dp.toPx()
                drawRoundRect(
                    color = trimAccent,
                    topLeft = Offset(inX, filmstripTopPx),
                    size = Size((outX - inX).coerceAtLeast(2f), filmstripHeightPx),
                    cornerRadius = CornerRadius(6.dp.toPx(), 6.dp.toPx()),
                    style = Stroke(width = borderW),
                )
                fun drawHandle(centerX: Float) {
                    val hx = (centerX - handleWidthPx / 2f).coerceIn(padHPx - handleWidthPx / 2f, padHPx + trackW - handleWidthPx / 2f)
                    drawRoundRect(
                        color = trimAccent,
                        topLeft = Offset(hx, filmstripTopPx - 1f),
                        size = Size(handleWidthPx, filmstripHeightPx + 2f),
                        cornerRadius = CornerRadius(4.dp.toPx(), 4.dp.toPx()),
                    )
                    val gx = hx + handleWidthPx / 2f
                    drawLine(
                        color = onTrimHandle,
                        start = Offset(gx, filmstripTopPx + filmstripHeightPx * 0.30f),
                        end = Offset(gx, filmstripTopPx + filmstripHeightPx * 0.70f),
                        strokeWidth = 1.3.dp.toPx(),
                    )
                }
                drawHandle(inX)
                drawHandle(outX)
            }

            // 3. Track 2: Activity Screen Spans + Discontinuities + Tap / Crash Markers
            drawRoundRect(
                color = trackFill,
                topLeft = Offset(padHPx, markerTopPx),
                size = Size(trackW, markerHeightPx),
                cornerRadius = trackRadius,
            )

            if (s != null && s.frameCount > 0L) {
                val screenBandH = markerHeightPx * 0.50f
                val pinY = markerTopPx + screenBandH + (markerHeightPx - screenBandH) * 0.52f

                clipRect(padHPx, markerTopPx, padHPx + trackW, markerTopPx + markerHeightPx) {
                    // Activity screen spans
                    for ((idx, span) in s.screens.withIndex()) {
                        val sf = span.startFrameIndex ?: s.boottimeNsToFrameIndex(span.startNs) ?: continue
                        val ef = span.endFrameIndex ?: s.boottimeNsToFrameIndex(span.endNs) ?: sf
                        val x1 = frameToX(sf, s.frameCount)
                        val x2 = frameToX(ef, s.frameCount).coerceAtLeast(x1 + 6.dp.toPx())
                        drawRoundRect(
                            color = screenColors[idx % screenColors.size],
                            topLeft = Offset(x1, markerTopPx + 1f),
                            size = Size((x2 - x1).coerceAtMost(padHPx + trackW - x1), screenBandH - 1f),
                            cornerRadius = CornerRadius(3.dp.toPx(), 3.dp.toPx()),
                        )
                    }

                    // Hatched amber discontinuity gap blocks
                    for (disc in s.discontinuities) {
                        val startBt = disc.startBoottimeNs ?: continue
                        val endBt = disc.endBoottimeNs ?: startBt
                        val sf = s.boottimeNsToFrameIndex(startBt) ?: continue
                        val ef = s.boottimeNsToFrameIndex(endBt) ?: sf
                        val x1 = frameToX(sf, s.frameCount)
                        val x2 = frameToX(ef, s.frameCount).coerceAtLeast(x1 + 5.dp.toPx())
                        val bw = (x2 - x1).coerceAtMost(padHPx + trackW - x1)
                        drawRect(amber.copy(alpha = 0.22f), Offset(x1, markerTopPx), Size(bw, markerHeightPx))
                        val step = 5.dp.toPx()
                        var sx = x1
                        while (sx < x1 + bw) {
                            drawLine(
                                color = amber,
                                start = Offset(sx, markerTopPx + markerHeightPx),
                                end = Offset((sx + step).coerceAtMost(x1 + bw), markerTopPx),
                                strokeWidth = 1.dp.toPx(),
                            )
                            sx += step
                        }
                    }

                    // Tap pins (cyan circles) vs Swipe spans (cyan bar + end pin), T-6H item 2.
                    val tapR = 3.5.dp.toPx()
                    val swipeBarHeight = 2.5.dp.toPx()
                    for (tap in s.mainTaps) {
                        val bt = TimelineViewerState.alignmentBoottimeNs(tap.alignment) ?: continue
                        val f = s.boottimeNsToFrameIndex(bt) ?: continue
                        val tx = frameToX(f, s.frameCount)
                        val endOffsetNs = tap.endTOffsetNs
                        val endFrame = if (tap.gestureKind == GestureKind.SWIPE && endOffsetNs != null) {
                            s.boottimeNsToFrameIndex(bt + endOffsetNs)
                        } else {
                            null
                        }
                        if (endFrame != null) {
                            // Minimum visible span width (6dp) so even a very fast/short swipe
                            // still reads as a bar rather than collapsing into a single pin.
                            val ex = frameToX(endFrame, s.frameCount).coerceAtLeast(tx + 6.dp.toPx())
                            drawRoundRect(
                                color = cyan,
                                topLeft = Offset(tx, pinY - swipeBarHeight / 2f),
                                size = Size(ex - tx, swipeBarHeight),
                                cornerRadius = CornerRadius(swipeBarHeight / 2f, swipeBarHeight / 2f),
                            )
                            drawCircle(color = cyan, radius = tapR * 0.7f, center = Offset(tx, pinY))
                            drawCircle(color = cyan, radius = tapR, center = Offset(ex, pinY), style = Stroke(width = 1.dp.toPx()))
                        } else {
                            drawCircle(color = cyan, radius = tapR, center = Offset(tx, pinY))
                        }
                    }
                }

                // Crash markers (red diamond + vertical dashed line)
                val dashEffect = PathEffect.dashPathEffect(floatArrayOf(4f, 4f), 0f)
                val crashR = 4.5.dp.toPx()
                for (crash in s.crashes) {
                    val f = crash.frameIndexHint ?: crash.boottimeNs?.let(s::boottimeNsToFrameIndex) ?: continue
                    val cx = frameToX(f, s.frameCount)
                    drawLine(
                        color = red,
                        start = Offset(cx, filmstripTopPx),
                        end = Offset(cx, markerTopPx + markerHeightPx),
                        strokeWidth = 1.25.dp.toPx(),
                        pathEffect = dashEffect,
                    )
                    val diamond = Path().apply {
                        moveTo(cx, pinY - crashR)
                        lineTo(cx + crashR, pinY)
                        lineTo(cx, pinY + crashR)
                        lineTo(cx - crashR, pinY)
                        close()
                    }
                    drawPath(diamond, color = red)
                }

                // 4. Unified crisp full-height playhead across ruler + tracks
                val px = frameToX(s.currentFrameIndex, s.frameCount)
                drawLine(
                    color = playheadOutline,
                    start = Offset(px, rulerHeightPx - 4.dp.toPx()),
                    end = Offset(px, markerTopPx + markerHeightPx + 2.dp.toPx()),
                    strokeWidth = 4.dp.toPx(),
                )
                drawLine(
                    color = playheadColor,
                    start = Offset(px, rulerHeightPx - 4.dp.toPx()),
                    end = Offset(px, markerTopPx + markerHeightPx + 2.dp.toPx()),
                    strokeWidth = 2.dp.toPx(),
                )
                val cap = Path().apply {
                    moveTo(px - 4.5.dp.toPx(), rulerHeightPx - 9.dp.toPx())
                    lineTo(px + 4.5.dp.toPx(), rulerHeightPx - 9.dp.toPx())
                    lineTo(px, rulerHeightPx - 2.dp.toPx())
                    close()
                }
                drawPath(cap, color = playheadColor)
            }
        }

        // Ruler time labels + Floating selection summary pill (crisp Compose Text overlays)
        if (state != null && state.frameCount > 0L) {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 14.dp, vertical = 2.dp),
                horizontalArrangement = Arrangement.SpaceBetween,
            ) {
                Text("0.0s", style = ArbJewelPalette.tabularMonoStyle(9.5f, color = mutedText))
                Text(
                    ArbFormatters.formatDurationSeconds(state.totalDurationMs / 2L),
                    style = ArbJewelPalette.tabularMonoStyle(9.5f, color = mutedText),
                )
                Text(
                    ArbFormatters.formatDurationSeconds(state.totalDurationMs),
                    style = ArbJewelPalette.tabularMonoStyle(9.5f, color = mutedText),
                )
            }

            // Floating selection summary pill centered over the trim selection
            val inX = frameToX(state.inFrameIndex, state.frameCount)
            val outX = frameToX(state.outFrameIndex, state.frameCount)
            val centerX = ((inX + outX) / 2f).coerceIn(padHPx + 90f, (padHPx + trackW - 90f).coerceAtLeast(padHPx + 90f))
            val pillShape = RoundedCornerShape(5.dp)
            Box(
                modifier = Modifier
                    .offset { IntOffset((centerX - 88f).roundToInt(), (filmstripTopPx + 4f).roundToInt()) }
                    .clip(pillShape)
                    .background(ArbJewelPalette.panelBackground().copy(alpha = 0.88f), pillShape)
                    .padding(horizontal = 7.dp, vertical = 2.dp),
            ) {
                Text(
                    text = state.selectionSummaryText,
                    style = ArbJewelPalette.tabularMonoStyle(sizeSp = 10f, bold = true),
                    maxLines = 1,
                    softWrap = false,
                )
            }
        }
    }
}
