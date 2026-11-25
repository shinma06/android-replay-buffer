package io.github.shinma06.arb.ide

import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.nio.file.Files
import java.nio.file.Path
import kotlin.test.assertContains
import kotlin.test.assertNotNull
import kotlin.test.assertNull

/** The "open image" action must warn about an unusable path instead of returning silently. */
class ArbNotificationsPathTest {
    @Test
    fun usableImageHasNoProblem(@TempDir tmp: Path) {
        val png = Files.writeString(tmp.resolve("shot.png"), "x")
        assertNull(ArbNotifications.imagePathProblem(png.toString()))
    }

    @Test
    fun missingBlankAndMalformedPathsProduceAUserFacingWarning(@TempDir tmp: Path) {
        assertContains(
            assertNotNull(ArbNotifications.imagePathProblem(tmp.resolve("gone.png").toString())),
            "見つかりません",
        )
        assertContains(assertNotNull(ArbNotifications.imagePathProblem("   ")), "空")
        assertContains(assertNotNull(ArbNotifications.imagePathProblem("bad\u0000path.png")), "不正")
        // A directory is not an image either.
        assertContains(assertNotNull(ArbNotifications.imagePathProblem(tmp.toString())), "見つかりません")
    }
}
