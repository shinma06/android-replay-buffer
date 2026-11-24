package io.github.shinma06.arb.shm

import org.junit.jupiter.api.Test
import kotlin.test.assertEquals

/**
 * Layer-3 PDD (G7): JVM side of the SHM layout parity table. The same rows are asserted by
 * `core/crates/arb-os/tests/pdd_g7_shm_parity_props.rs` against the Rust writer.
 */
class ShmLayoutParityTest {
    private data class Row(val format: ShmPixelFormat, val width: Int, val height: Int, val stride: Int, val bytes: Int?)

    @Test
    fun requiredBufferBytesMatchesRustGoldenTable() {
        val golden = listOf(
            Row(ShmPixelFormat.BGRA8, 1, 1, 3, null),
            Row(ShmPixelFormat.BGRA8, 1, 1, 4, 4),
            Row(ShmPixelFormat.BGRA8, 1, 1, 9, 9),
            Row(ShmPixelFormat.BGRA8, 3, 2, 11, null),
            Row(ShmPixelFormat.BGRA8, 3, 2, 12, 24),
            Row(ShmPixelFormat.BGRA8, 3, 2, 17, 34),
            Row(ShmPixelFormat.BGRA8, 3, 3, 11, null),
            Row(ShmPixelFormat.BGRA8, 3, 3, 12, 36),
            Row(ShmPixelFormat.BGRA8, 3, 3, 17, 51),
            Row(ShmPixelFormat.BGRA8, 17, 15, 67, null),
            Row(ShmPixelFormat.BGRA8, 17, 15, 68, 1020),
            Row(ShmPixelFormat.BGRA8, 17, 15, 73, 1095),
            Row(ShmPixelFormat.RGBA8, 1, 1, 3, null),
            Row(ShmPixelFormat.RGBA8, 1, 1, 4, 4),
            Row(ShmPixelFormat.RGBA8, 1, 1, 9, 9),
            Row(ShmPixelFormat.RGBA8, 3, 2, 11, null),
            Row(ShmPixelFormat.RGBA8, 3, 2, 12, 24),
            Row(ShmPixelFormat.RGBA8, 3, 2, 17, 34),
            Row(ShmPixelFormat.RGBA8, 3, 3, 11, null),
            Row(ShmPixelFormat.RGBA8, 3, 3, 12, 36),
            Row(ShmPixelFormat.RGBA8, 3, 3, 17, 51),
            Row(ShmPixelFormat.RGBA8, 17, 15, 67, null),
            Row(ShmPixelFormat.RGBA8, 17, 15, 68, 1020),
            Row(ShmPixelFormat.RGBA8, 17, 15, 73, 1095),
            Row(ShmPixelFormat.RGB24, 1, 1, 2, null),
            Row(ShmPixelFormat.RGB24, 1, 1, 3, 3),
            Row(ShmPixelFormat.RGB24, 1, 1, 8, 8),
            Row(ShmPixelFormat.RGB24, 3, 2, 8, null),
            Row(ShmPixelFormat.RGB24, 3, 2, 9, 18),
            Row(ShmPixelFormat.RGB24, 3, 2, 14, 28),
            Row(ShmPixelFormat.RGB24, 3, 3, 8, null),
            Row(ShmPixelFormat.RGB24, 3, 3, 9, 27),
            Row(ShmPixelFormat.RGB24, 3, 3, 14, 42),
            Row(ShmPixelFormat.RGB24, 17, 15, 50, null),
            Row(ShmPixelFormat.RGB24, 17, 15, 51, 765),
            Row(ShmPixelFormat.RGB24, 17, 15, 56, 840),
            Row(ShmPixelFormat.NV12, 1, 1, 1, null),
            Row(ShmPixelFormat.NV12, 1, 1, 2, 4),
            Row(ShmPixelFormat.NV12, 1, 1, 7, 14),
            Row(ShmPixelFormat.NV12, 3, 2, 3, null),
            Row(ShmPixelFormat.NV12, 3, 2, 4, 12),
            Row(ShmPixelFormat.NV12, 3, 2, 9, 27),
            Row(ShmPixelFormat.NV12, 3, 3, 3, null),
            Row(ShmPixelFormat.NV12, 3, 3, 4, 20),
            Row(ShmPixelFormat.NV12, 3, 3, 9, 45),
            Row(ShmPixelFormat.NV12, 17, 15, 17, null),
            Row(ShmPixelFormat.NV12, 17, 15, 18, 414),
            Row(ShmPixelFormat.NV12, 17, 15, 23, 529),
        )
        for (row in golden) {
            assertEquals(
                row.bytes,
                row.format.requiredBufferBytes(row.width, row.height, row.stride),
                "${row.format} ${row.width}x${row.height} stride ${row.stride}",
            )
        }
    }
}
