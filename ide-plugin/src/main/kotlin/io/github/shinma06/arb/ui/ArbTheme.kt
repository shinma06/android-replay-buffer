package io.github.shinma06.arb.ui

import com.intellij.ui.JBColor
import com.intellij.util.ui.JBUI
import java.awt.Color
import java.awt.Font
import javax.swing.BorderFactory
import javax.swing.JButton
import javax.swing.border.Border

/**
 * Android Studio IntUI Dark & Light native design tokens.
 *
 * Rules enforced across all ARB v2 UI components:
 * - Zero emoji chrome (all icons via `com.intellij.icons.AllIcons` or crisp monochrome vectors)
 * - Subtle 1px hairline borders, restrained semantic colors, tabular monospace numbers, 4px/8px grid
 * - Native Dark & Light support via [JBColor]
 */
object ArbTheme {
    // Surfaces
    val surfaceBg: JBColor = JBColor(Color(0xF7, 0xF8, 0xFA), Color(0x1E, 0x1F, 0x22))
    val cardBg: JBColor = JBColor(Color(0xFF, 0xFF, 0xFF), Color(0x2B, 0x2D, 0x30))
    val elevatedBg: JBColor = JBColor(Color(0xF0, 0xF2, 0xF5), Color(0x31, 0x34, 0x38))
    val stageBg: Color = Color(0x14, 0x16, 0x19)
    val timelineBg: JBColor = JBColor(Color(0xEC, 0xEE, 0xF2), Color(0x1B, 0x1D, 0x21))
    val timelineTrackBg: JBColor = JBColor(Color(0xE4, 0xE7, 0xEC), Color(0x23, 0x26, 0x2B))
    val rowHoverBg: JBColor = JBColor(Color(0xE8, 0xEE, 0xF8), Color(0x2F, 0x35, 0x40))
    val rowSelectedBg: JBColor = JBColor(Color(0xD4, 0xE2, 0xFF), Color(0x2E, 0x43, 0x6E))

    // Borders
    val hairlineBorder: JBColor = JBColor(Color(0xD8, 0xDC, 0xE3), Color(0x39, 0x3B, 0x40))
    val subtleBorder: JBColor = JBColor(Color(0xE5, 0xE7, 0xEB), Color(0x32, 0x35, 0x3A))

    // Typography
    val primaryText: JBColor = JBColor(Color(0x1D, 0x1F, 0x23), Color(0xDF, 0xE1, 0xE5))
    val secondaryText: JBColor = JBColor(Color(0x5A, 0x61, 0x6E), Color(0x9D, 0xA0, 0xA8))
    val dimmedText: JBColor = JBColor(Color(0x9C, 0xA3, 0xAF), Color(0x5F, 0x63, 0x6B))

    // Restrained IntUI Semantic Accents
    val accentBlue: JBColor = JBColor(Color(0x35, 0x74, 0xF0), Color(0x35, 0x74, 0xF0))
    val accentBlueHover: JBColor = JBColor(Color(0x2A, 0x63, 0xD6), Color(0x46, 0x82, 0xF5))
    val accentBlueSoft: JBColor = JBColor(Color(0xE8, 0xF0, 0xFE), Color(0x25, 0x36, 0x54))

    val recRed: JBColor = JBColor(Color(0xDB, 0x3B, 0x4B), Color(0xE5, 0x53, 0x5E))
    val recRedSoft: JBColor = JBColor(Color(0xFD, 0xEC, 0xEE), Color(0x42, 0x24, 0x28))

    val warningAmber: JBColor = JBColor(Color(0xC2, 0x7D, 0x0E), Color(0xE0, 0x9F, 0x3E))
    val warningAmberSoft: JBColor = JBColor(Color(0xFE, 0xF5, 0xE5), Color(0x3D, 0x31, 0x1E))

    val infoCyan: JBColor = JBColor(Color(0x0E, 0x86, 0xA8), Color(0x3C, 0xB4, 0xD6))
    val infoCyanSoft: JBColor = JBColor(Color(0xE6, 0xF6, 0xFA), Color(0x1E, 0x36, 0x3F))

    val successGreen: JBColor = JBColor(Color(0x20, 0x8A, 0x3C), Color(0x57, 0x96, 0x5C))
    val successGreenSoft: JBColor = JBColor(Color(0xE9, 0xF7, 0xED), Color(0x23, 0x38, 0x27))

    val screenSpanPalette: List<JBColor> = listOf(
        JBColor(Color(0xD8, 0xE6, 0xFB), Color(0x28, 0x38, 0x52)),
        JBColor(Color(0xDC, 0xF0, 0xE8), Color(0x24, 0x3C, 0x34)),
        JBColor(Color(0xE8, 0xE2, 0xFA), Color(0x35, 0x2E, 0x4C)),
        JBColor(Color(0xF6, 0xEB, 0xDA), Color(0x3F, 0x34, 0x25)),
    )

    fun monoFont(sizePt: Float = 11.5f, bold: Boolean = false): Font {
        val style = if (bold) Font.BOLD else Font.PLAIN
        val px = JBUI.scaleFontSize(sizePt).coerceAtLeast(10)
        return Font("JetBrains Mono", style, px).let { f ->
            if (f.family.equals("JetBrains Mono", ignoreCase = true)) {
                f
            } else {
                Font(Font.MONOSPACED, style, px)
            }
        }
    }

    fun uiFont(sizePt: Float = 12f, bold: Boolean = false): Font {
        val base = JBUI.Fonts.label(sizePt)
        return if (bold) base.asBold() else base
    }

    fun cardBorder(padV: Int = 8, padH: Int = 10): Border = BorderFactory.createCompoundBorder(
        BorderFactory.createLineBorder(hairlineBorder, 1),
        JBUI.Borders.empty(padV, padH),
    )

    fun hairlineBottomBorder(padV: Int = 6, padH: Int = 10): Border = BorderFactory.createCompoundBorder(
        BorderFactory.createMatteBorder(0, 0, 1, 0, hairlineBorder),
        JBUI.Borders.empty(padV, padH),
    )

    fun hairlineTopBorder(padV: Int = 4, padH: Int = 8): Border = BorderFactory.createCompoundBorder(
        BorderFactory.createMatteBorder(1, 0, 0, 0, hairlineBorder),
        JBUI.Borders.empty(padV, padH),
    )

    fun stylePrimaryButton(button: JButton): JButton = button.apply {
        isFocusPainted = false
        background = accentBlue
        foreground = Color.WHITE
        isOpaque = true
        font = uiFont(12f, bold = true)
        border = BorderFactory.createCompoundBorder(
            BorderFactory.createLineBorder(accentBlueHover, 1),
            JBUI.Borders.empty(6, 12),
        )
    }

    fun styleSecondaryButton(button: JButton, compact: Boolean = false): JButton = button.apply {
        isFocusPainted = false
        background = elevatedBg
        foreground = primaryText
        isOpaque = true
        font = uiFont(if (compact) 11f else 11.5f, bold = false)
        border = BorderFactory.createCompoundBorder(
            BorderFactory.createLineBorder(hairlineBorder, 1),
            JBUI.Borders.empty(if (compact) 4 else 5, if (compact) 8 else 10),
        )
    }

    fun styleFilterChip(button: JButton, active: Boolean): JButton = button.apply {
        isFocusPainted = false
        isOpaque = true
        font = uiFont(11f, bold = active)
        background = if (active) accentBlueSoft else surfaceBg
        foreground = if (active) accentBlue else secondaryText
        border = BorderFactory.createCompoundBorder(
            BorderFactory.createLineBorder(if (active) accentBlue else hairlineBorder, 1),
            JBUI.Borders.empty(3, 8),
        )
    }
}
