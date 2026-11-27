package io.github.shinma06.arb.ui

import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.VerticalScrollbar
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.defaultScrollbarStyle
import androidx.compose.foundation.hoverable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsHoveredAsState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.rememberScrollbarAdapter
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.focusProperties
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asComposeCanvas
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.TextLayoutResult
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.LineHeightStyle
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.collectLatest
import org.jetbrains.jewel.foundation.theme.JewelTheme
import org.jetbrains.jewel.ui.component.Checkbox
import org.jetbrains.jewel.ui.component.Icon
import org.jetbrains.jewel.ui.component.Text
import org.jetbrains.jewel.ui.component.Tooltip
import org.jetbrains.jewel.ui.component.styling.ScrollbarVisibility
import org.jetbrains.jewel.ui.icon.IconKey
import org.jetbrains.jewel.ui.theme.scrollbarStyle
import org.jetbrains.skia.Bitmap
import org.jetbrains.skia.Canvas
import org.jetbrains.skia.ImageInfo
import kotlin.math.ceil
import kotlin.math.roundToInt

/**
 * Native Android Studio Islands / IntUI JetBrains Jewel design tokens and borderless primitives.
 *
 * Strictly uses `JewelTheme` surface/text/border tokens — ZERO hardcoded dark-mode surface colors
 * so Light & Dark themes and Android Studio's exact panel surface match seamlessly.
 */
object ArbJewelPalette {
    /**
     * Light/dark semantic accent pair. Resolved against `JewelTheme.isDark` (a composition read),
     * so a Light <-> Dark switch recomposes every reader immediately. `JBColor.toComposeColor()`
     * reads no Compose state and could keep stale colors until an unrelated recomposition.
     */
    internal class Tone(val lightRgb: Int, val darkRgb: Int) {
        fun rgb(isDark: Boolean): Int = if (isDark) darkRgb else lightRgb
    }

    internal val REC_RED = Tone(0xD93848, 0xE85A67)
    internal val WARNING_AMBER = Tone(0xB26B00, 0xE5A645)
    internal val INFO_CYAN = Tone(0x0E81A2, 0x4CC2E0)
    internal val SUCCESS_GREEN = Tone(0x1F8038, 0x5DB365)
    internal val TRIM_GOLD = Tone(0xD49B1A, 0xE8B931)

    private val screenSpanTones: List<Tone> = listOf(
        Tone(0x3574F0, 0x548AF7),
        Tone(0x1F8A5B, 0x47B383),
        Tone(0x7A56E1, 0x9C7CF2),
        Tone(0xC97A16, 0xE0A03D),
    )

    internal fun opaque(rgb: Int): Color = Color(0xFF000000.toInt() or (rgb and 0x00FFFFFF))

    @Composable
    private fun Tone.resolve(): Color = opaque(rgb(JewelTheme.isDark))

    @Composable
    fun panelBackground(): Color = JewelTheme.globalColors.panelBackground

    @Composable
    fun primaryText(): Color = JewelTheme.contentColor

    @Composable
    fun mutedText(): Color = JewelTheme.globalColors.text.info

    @Composable
    fun dimmedText(): Color = JewelTheme.contentColor.copy(alpha = 0.45f)

    @Composable
    fun hairlineDivider(): Color = JewelTheme.globalColors.borders.normal.copy(alpha = 0.6f)

    @Composable
    fun subtleHoverFill(): Color = JewelTheme.contentColor.copy(alpha = if (JewelTheme.isDark) 0.08f else 0.06f)

    @Composable
    fun subtleTrackFill(): Color = JewelTheme.contentColor.copy(alpha = if (JewelTheme.isDark) 0.10f else 0.08f)

    @Composable
    fun accent(): Color = JewelTheme.globalColors.outlines.focused

    @Composable
    fun accentSoft(): Color = accent().copy(alpha = if (JewelTheme.isDark) 0.22f else 0.14f)

    @Composable
    fun recRed(): Color = REC_RED.resolve()

    @Composable
    fun recRedSoft(): Color = recRed().copy(alpha = if (JewelTheme.isDark) 0.18f else 0.10f)

    @Composable
    fun warningAmber(): Color = WARNING_AMBER.resolve()

    @Composable
    fun warningAmberSoft(): Color = warningAmber().copy(alpha = if (JewelTheme.isDark) 0.18f else 0.12f)

    @Composable
    fun infoCyan(): Color = INFO_CYAN.resolve()

    @Composable
    fun infoCyanSoft(): Color = infoCyan().copy(alpha = if (JewelTheme.isDark) 0.18f else 0.11f)

    @Composable
    fun successGreen(): Color = SUCCESS_GREEN.resolve()

    @Composable
    fun successGreenSoft(): Color = successGreen().copy(alpha = if (JewelTheme.isDark) 0.18f else 0.10f)

    @Composable
    fun trimAccent(): Color = TRIM_GOLD.resolve()

    /** Grip lines drawn on top of the gold trim handles (theme panel surface, no hardcoded hex). */
    @Composable
    fun trimHandleGrip(): Color = panelBackground()

    @Composable
    fun screenSpanColor(index: Int): Color =
        screenSpanTones[index % screenSpanTones.size].resolve()
            .copy(alpha = if (JewelTheme.isDark) 0.26f else 0.16f)

    @Composable
    fun tabularMonoStyle(sizeSp: Float = 11f, bold: Boolean = false, color: Color = primaryText()): TextStyle =
        JewelTheme.defaultTextStyle.copy(
            fontFamily = FontFamily.Monospace,
            fontSize = sizeSp.sp,
            fontWeight = if (bold) FontWeight.SemiBold else FontWeight.Normal,
            fontFeatureSettings = "tnum",
            color = color,
        )

    @Composable
    fun bodyStyle(sizeSp: Float = 12f, bold: Boolean = false, color: Color = primaryText()): TextStyle =
        JewelTheme.defaultTextStyle.copy(
            fontSize = sizeSp.sp,
            fontWeight = if (bold) FontWeight.SemiBold else FontWeight.Normal,
            color = color,
        )
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
fun ArbTooltip(
    tooltip: String,
    modifier: Modifier = Modifier,
    content: @Composable () -> Unit,
) {
    if (tooltip.isBlank()) {
        // Keep the caller's layout modifier (e.g. `weight(1f)`) even without a tooltip.
        Box(modifier = modifier) { content() }
    } else {
        Tooltip(tooltip = { Text(tooltip) }, modifier = modifier, content = content)
    }
}

/**
 * Borderless hover-tinted flat action matching Android Studio's Islands / IntUI secondary toolbars
 * (transparent at rest, soft rounded hover tint — ZERO 1px rectangle borders!).
 */
@Composable
fun ArbSubtleAction(
    label: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    iconKey: IconKey? = null,
    enabled: Boolean = true,
    active: Boolean = false,
    tooltip: String = "",
    compact: Boolean = false,
    tint: Color? = null,
) {
    val interactionSource = remember { MutableInteractionSource() }
    val hovered by interactionSource.collectIsHoveredAsState()
    val bg = when {
        active -> ArbJewelPalette.accentSoft()
        hovered && enabled -> ArbJewelPalette.subtleHoverFill()
        else -> Color.Transparent
    }
    val fg = when {
        !enabled -> ArbJewelPalette.dimmedText()
        tint != null -> tint
        active -> ArbJewelPalette.accent()
        else -> ArbJewelPalette.primaryText()
    }
    val shape = RoundedCornerShape(6.dp)
    ArbTooltip(tooltip) {
        Row(
            modifier = modifier
                .clip(shape)
                .background(bg, shape)
                .hoverable(interactionSource, enabled = enabled)
                .clickable(
                    interactionSource = interactionSource,
                    indication = null,
                    enabled = enabled,
                    role = Role.Button,
                    onClick = onClick,
                )
                .padding(horizontal = if (compact) 6.dp else 8.dp, vertical = if (compact) 4.dp else 5.dp)
                .semantics { contentDescription = label },
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(4.dp, Alignment.CenterHorizontally),
        ) {
            if (iconKey != null) {
                Icon(
                    key = iconKey,
                    contentDescription = null,
                    modifier = Modifier.size(if (compact) 13.dp else 14.dp),
                    tint = if (enabled) (tint ?: Color.Unspecified) else ArbJewelPalette.dimmedText(),
                )
            }
            if (label.isNotEmpty()) {
                Text(
                    text = label,
                    style = ArbJewelPalette.bodyStyle(
                        sizeSp = if (compact) 11f else 11.5f,
                        bold = active,
                        color = fg,
                    ),
                    maxLines = 1,
                    softWrap = false,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
    }
}

/**
 * Subtle borderless inline tab / filter pill matching Android Studio's delicate 11.5–12sp scale.
 */
@Composable
fun ArbFilterTab(
    label: String,
    selected: Boolean,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
) {
    val interactionSource = remember { MutableInteractionSource() }
    val hovered by interactionSource.collectIsHoveredAsState()
    val shape = RoundedCornerShape(6.dp)
    val bg = when {
        selected -> ArbJewelPalette.accentSoft()
        hovered && enabled -> ArbJewelPalette.subtleHoverFill()
        else -> Color.Transparent
    }
    val fg = when {
        !enabled -> ArbJewelPalette.dimmedText()
        selected -> ArbJewelPalette.accent()
        else -> ArbJewelPalette.mutedText()
    }
    Box(
        modifier = modifier
            .clip(shape)
            .background(bg, shape)
            .hoverable(interactionSource, enabled = enabled)
            .clickable(
                interactionSource = interactionSource,
                indication = null,
                enabled = enabled,
                role = Role.Tab,
                onClick = onClick,
            )
            .padding(horizontal = 8.dp, vertical = 4.dp)
            .semantics { contentDescription = label },
        contentAlignment = Alignment.Center,
    ) {
        Text(
            text = label,
            style = ArbJewelPalette.bodyStyle(sizeSp = 11.5f, bold = selected, color = fg),
            maxLines = 1,
            softWrap = false,
        )
    }
}

/**
 * Soft rounded semantic pill (used sparingly for Tier-2 conditional badges and bundle tags).
 */
@Composable
fun ArbSemanticPill(
    label: String,
    background: Color,
    foreground: Color,
    modifier: Modifier = Modifier,
    onClick: (() -> Unit)? = null,
    tooltip: String = "",
) {
    val shape = RoundedCornerShape(5.dp)
    ArbTooltip(tooltip) {
        Box(
            modifier = modifier
                .clip(shape)
                .background(background, shape)
                .then(
                    if (onClick != null) {
                        Modifier.clickable(role = Role.Button, onClick = onClick)
                    } else {
                        Modifier
                    },
                )
                .padding(horizontal = 6.dp, vertical = 2.dp),
            contentAlignment = Alignment.Center,
        ) {
            Text(
                text = label,
                style = ArbJewelPalette.tabularMonoStyle(sizeSp = 10f, bold = true, color = foreground),
                maxLines = 1,
                softWrap = false,
            )
        }
    }
}

/**
 * Soft tinted rounded Tier-2 alert banner (zero height when unused; no harsh square borders).
 */
@Composable
fun ArbSoftBanner(
    background: Color,
    accent: Color,
    modifier: Modifier = Modifier,
    content: @Composable RowScope.() -> Unit,
) {
    val shape = RoundedCornerShape(8.dp)
    Row(
        modifier = modifier
            .fillMaxWidth()
            .clip(shape)
            .background(background, shape)
            .padding(horizontal = 10.dp, vertical = 8.dp)
            .heightIn(min = 18.dp)
            .height(IntrinsicSize.Min),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Box(
            Modifier
                .width(3.dp)
                .fillMaxHeight()
                .clip(RoundedCornerShape(2.dp))
                .background(accent),
        )
        content()
    }
}

/**
 * Native Jewel-styled vertical scrollable column (matches proven v1 `ScrollableColumn.kt`).
 */
@Composable
fun ArbScrollableColumn(
    modifier: Modifier = Modifier,
    verticalPadding: Dp = 0.dp,
    content: @Composable ColumnScope.() -> Unit,
) {
    val scrollState = rememberScrollState()
    val adapter = rememberScrollbarAdapter(scrollState)
    val jewelStyle = JewelTheme.scrollbarStyle
    val visibility = jewelStyle.scrollbarVisibility
    val alwaysVisible = visibility is ScrollbarVisibility.AlwaysVisible
    val lingerMs = visibility.lingerDuration.inWholeMilliseconds
    var visible by remember(alwaysVisible) { mutableStateOf(alwaysVisible) }

    LaunchedEffect(scrollState, alwaysVisible, lingerMs) {
        if (!alwaysVisible) {
            snapshotFlow { scrollState.isScrollInProgress }.collectLatest { scrolling ->
                if (scrolling) {
                    visible = true
                } else {
                    delay(lingerMs)
                    visible = false
                }
            }
        }
    }

    val scrollbarStyle = remember(jewelStyle, alwaysVisible, visible) {
        val active = jewelStyle.colors.thumbBackgroundActive
        val idle = when {
            alwaysVisible -> jewelStyle.colors.thumbOpaqueBackground
            visible -> active
            else -> Color.Transparent
        }
        defaultScrollbarStyle().copy(
            minimalHeight = jewelStyle.metrics.minThumbLength,
            thickness = visibility.trackThickness,
            shape = RoundedCornerShape(jewelStyle.metrics.thumbCornerSize),
            hoverDurationMillis = visibility.thumbColorAnimationDuration.inWholeMilliseconds.toInt(),
            unhoverColor = idle,
            hoverColor = active,
        )
    }

    Box(modifier) {
        Column(
            modifier = Modifier
                .fillMaxSize()
                .verticalScroll(scrollState)
                .padding(vertical = verticalPadding),
            content = content,
        )
        VerticalScrollbar(
            adapter = adapter,
            modifier = Modifier.align(Alignment.CenterEnd).fillMaxHeight(),
            style = scrollbarStyle,
        )
    }
}

@Composable
fun ArbSpacerH(width: Dp) = Spacer(Modifier.width(width))

@Composable
fun ArbSpacerV(height: Dp) = Spacer(Modifier.height(height))

/** Align the indicator to the first line even when the label wraps in a narrow panel. */
@Composable
fun ArbLabeledCheckbox(
    checked: Boolean,
    onCheckedChange: (Boolean) -> Unit,
    text: String,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
    muted: Boolean = false,
) {
    val interactionSource = remember { MutableInteractionSource() }
    var firstLineCenter by remember(text) { mutableStateOf(0) }
    Row(
        modifier = modifier
            .toggleable(
                value = checked,
                onValueChange = onCheckedChange,
                enabled = enabled,
                role = Role.Checkbox,
                interactionSource = interactionSource,
                indication = null,
            )
            .semantics { contentDescription = text },
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        Checkbox(
            checked = checked,
            onCheckedChange = onCheckedChange,
            enabled = enabled,
            // Shares the `Row.toggleable` above's interaction source so clicking the label text
            // (outside the small checkbox box) still highlights/toggles consistently; Compose
            // resolves clicks on the checkbox itself to the innermost handler, so there is no
            // double-toggle. Disable duplicate focus stop and nested Role.Checkbox semantics so
            // `Row.toggleable` is the single keyboard & accessibility target.
            interactionSource = interactionSource,
            modifier = Modifier
                .alignBy { indicator ->
                    // Classic uses a 20 x 19dp slot for its 19dp glyph. New UI has a square slot.
                    val slotInset = (indicator.measuredWidth - indicator.measuredHeight) / 2f
                    (indicator.measuredHeight / 2f - slotInset).roundToInt()
                }
                .focusProperties { canFocus = false }
                .clearAndSetSemantics {},
        )
        val labelColor = when {
            !enabled -> ArbJewelPalette.dimmedText()
            muted -> ArbJewelPalette.mutedText()
            else -> ArbJewelPalette.primaryText()
        }
        Text(
            text = text,
            style = ArbJewelPalette
                .bodyStyle(color = labelColor)
                .copy(
                    lineHeightStyle = LineHeightStyle(
                        alignment = LineHeightStyle.Alignment.Center,
                        trim = LineHeightStyle.Trim.Both,
                    ),
                ),
            onTextLayout = { firstLineCenter = it.firstLineInkCenter() },
            modifier = Modifier.alignBy { firstLineCenter },
        )
    }
}

/**
 * Measure the glyphs after Compose has chosen the fallback fonts and wrapped the label.
 * Rasterize only the first line into a small CPU alpha mask when text layout changes.
 */
private fun TextLayoutResult.firstLineInkCenter(): Int {
    val width = ceil(getLineRight(0)).toInt()
    val height = ceil(getLineBottom(0)).toInt()
    if (width <= 0 || height <= 0) return height / 2
    val info = ImageInfo.makeA8(width, height)
    Bitmap().use { mask ->
        if (!mask.allocPixels(info, width)) return height / 2
        mask.erase(0)
        Canvas(mask).use { canvas -> multiParagraph.paint(canvas.asComposeCanvas(), Color.Black) }
        val alpha = mask.readPixels(info, width) ?: return height / 2
        val top = alpha.indexOfFirst { (it.toInt() and 0xff) >= 96 }
        if (top < 0) return height / 2
        val bottom = alpha.indexOfLast { (it.toInt() and 0xff) >= 96 }
        // Pixel centers are half a pixel inside each raster row.
        return ((top / width + bottom / width + 1) / 2f).roundToInt()
    }
}
