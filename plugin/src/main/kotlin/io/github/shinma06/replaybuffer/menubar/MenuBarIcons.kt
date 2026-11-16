package io.github.shinma06.replaybuffer.menubar

import java.awt.BasicStroke
import java.awt.Color
import java.awt.RenderingHints
import java.awt.geom.Area
import java.awt.geom.Ellipse2D
import java.awt.geom.Path2D
import java.awt.geom.Rectangle2D
import java.awt.image.BufferedImage
import kotlin.math.cos
import kotlin.math.sin

private const val ICON_SIZE = 64
private const val CENTER = ICON_SIZE / 2f

internal fun menuBarIconImage(icon: MenuBarIcon): BufferedImage = drawIcon(icon, halo = true)

internal fun menuBarTemplateIconImage(icon: MenuBarIcon): BufferedImage = drawIcon(icon, halo = false)

private fun drawIcon(icon: MenuBarIcon, halo: Boolean): BufferedImage {
    val image = BufferedImage(ICON_SIZE, ICON_SIZE, BufferedImage.TYPE_INT_ARGB)
    val g = image.createGraphics()
    g.setRenderingHint(RenderingHints.KEY_ANTIALIASING, RenderingHints.VALUE_ANTIALIAS_ON)
    val ink = iconShape(icon)
    if (halo) {
        g.color = Color.WHITE
        g.stroke = BasicStroke(6f, BasicStroke.CAP_ROUND, BasicStroke.JOIN_ROUND)
        g.draw(ink)
    }
    g.color = Color.BLACK
    g.fill(ink)
    g.dispose()
    return image
}

private fun iconShape(icon: MenuBarIcon): Area = when (icon) {
    MenuBarIcon.FAILED -> triangleOutline().apply { add(exclaim(CENTER, 26f, 15f)) }
    MenuBarIcon.SAVING -> ring(14f, 14f, 36f, 5f).apply { add(downArrow(CENTER, 20f, 24f)) }
    MenuBarIcon.ATTENTION -> ring(14f, 14f, 36f, 5f).apply { add(exclaim(CENTER, 21f, 22f)) }
    MenuBarIcon.RECOVERING -> arcArrow()
    MenuBarIcon.WAITING -> dots(8, 13f, 3.4f)
    MenuBarIcon.CAPTURING -> Area(Ellipse2D.Float(16f, 16f, 32f, 32f))
    MenuBarIcon.OFF -> ring(15f, 15f, 34f, 5f)
}

private fun ring(x: Float, y: Float, size: Float, thickness: Float): Area =
    Area(Ellipse2D.Float(x, y, size, size)).apply {
        subtract(Area(Ellipse2D.Float(x + thickness, y + thickness, size - 2 * thickness, size - 2 * thickness)))
    }

private fun dots(count: Int, radius: Float, dotRadius: Float): Area = Area().apply {
    for (i in 0 until count) {
        val angle = Math.toRadians(i * 360.0 / count)
        val cx = CENTER + radius * cos(angle).toFloat()
        val cy = CENTER - radius * sin(angle).toFloat()
        add(Area(Ellipse2D.Float(cx - dotRadius, cy - dotRadius, dotRadius * 2, dotRadius * 2)))
    }
}

private fun exclaim(cx: Float, top: Float, height: Float): Area = Area().apply {
    add(Area(Rectangle2D.Float(cx - 2.2f, top, 4.4f, height * 0.62f)))
    add(Area(Ellipse2D.Float(cx - 2.4f, top + height * 0.74f, 4.8f, height * 0.26f)))
}

private fun downArrow(cx: Float, top: Float, height: Float): Area = Area().apply {
    add(Area(Rectangle2D.Float(cx - 2.2f, top, 4.4f, height * 0.55f)))
    val head = Path2D.Float()
    head.moveTo(cx - 7.5, top + height * 0.5)
    head.lineTo(cx + 7.5, top + height * 0.5)
    head.lineTo(cx.toDouble(), (top + height).toDouble())
    head.closePath()
    add(Area(head))
}

private fun triangleOutline(): Area {
    val outer = Path2D.Float()
    outer.moveTo(32.0, 11.0); outer.lineTo(54.0, 51.0); outer.lineTo(10.0, 51.0); outer.closePath()
    val inner = Path2D.Float()
    inner.moveTo(32.0, 22.0); inner.lineTo(46.0, 46.0); inner.lineTo(18.0, 46.0); inner.closePath()
    return Area(outer).apply { subtract(Area(inner)) }
}

private fun arcArrow(): Area {
    val ring = Area(java.awt.geom.Arc2D.Float(14f, 14f, 36f, 36f, 40f, 300f, java.awt.geom.Arc2D.PIE)).apply {
        subtract(Area(java.awt.geom.Arc2D.Float(20f, 20f, 24f, 24f, 40f, 300f, java.awt.geom.Arc2D.PIE)))
    }
    val head = Path2D.Float()
    head.moveTo(46.0, 10.0); head.lineTo(55.0, 24.0); head.lineTo(40.0, 25.0); head.closePath()
    ring.add(Area(head))
    return ring
}
