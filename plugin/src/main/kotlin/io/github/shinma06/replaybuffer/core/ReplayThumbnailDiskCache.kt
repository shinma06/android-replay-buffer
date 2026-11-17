package io.github.shinma06.replaybuffer.core

import java.awt.image.BufferedImage
import java.nio.ByteBuffer
import java.nio.file.Path
import java.nio.file.StandardOpenOption.READ
import java.security.MessageDigest
import java.util.UUID

/** Disposable owner-only, handle-relative cache. Raw bounded BGRA avoids image decompression bombs. */
internal class ReplayThumbnailDiskCache(private val parentPath: Path, private val maxBytes: Long = 64L * 1024 * 1024) {
    companion object {
        private val lock = Any()
        private const val MAGIC = 0x41524231
    }

    private fun name(source: String, atUs: Long): String = MessageDigest.getInstance("SHA-256")
        .digest("$source:$atUs:96:awt-bilinear-bgra-v1".toByteArray(Charsets.UTF_8))
        .joinToString("") { "%02x".format(it) } + ".thumb"

    private fun <T> directory(action: (SaveOutputDirectory) -> T): T = SaveOutputDirectory.open(parentPath).use { parent ->
        val name = "arb-preview-v1"
        val child = runCatching { parent.createDirectory(name) }.getOrElse { parent.openChild(name) }
        child.use { dir ->
            dir.requirePrivate()
            val guard = runCatching { dir.createFile("cache.lock") }.getOrElse { dir.openFile("cache.lock") }
            guard.use { file ->
                file.requirePrivate()
                file.channel().use { channel ->
                    val owner = PrivateFiles.tryOwnerLock(channel) ?: error("サムネイルcacheは使用中です")
                    owner.use { dir.verifyFile("cache.lock", file); action(dir) }
                }
            }
        }
    }

    fun read(source: String, atUs: Long): BufferedImage? = synchronized(lock) {
        runCatching {
            directory { dir -> dir.openFile(name(source, atUs)).use { file ->
                file.requirePrivate()
                file.channel(setOf(READ)).use { channel ->
                    require(channel.size() in 12..(12 + 4096L * 96 * 4))
                    val data = ByteBuffer.allocate(channel.size().toInt())
                    while (data.hasRemaining()) check(channel.read(data) > 0)
                    data.flip()
                    require(data.int == MAGIC)
                    val width = data.int; val height = data.int
                    require(width in 1..4096 && height in 1..96 && data.remaining() == width * height * 4)
                    val pixels = ByteArray(data.remaining()).also { data.get(it) }
                    require((3 until pixels.size step 4).all { pixels[it] == 255.toByte() })
                    dir.verifyFile(name(source, atUs), file)
                    ReplayPreviewImage(pixels, width, height)
                }
            } }
        }.getOrNull()
    }

    fun write(source: String, atUs: Long, image: BufferedImage) = synchronized(lock) {
        runCatching {
            require(image.width in 1..4096 && image.height in 1..96)
            val bytes = ByteBuffer.allocate(12 + image.width * image.height * 4)
            bytes.putInt(MAGIC).putInt(image.width).putInt(image.height)
            for (y in 0 until image.height) for (x in 0 until image.width) {
                val rgb = image.getRGB(x, y)
                bytes.put(rgb.toByte()).put((rgb shr 8).toByte()).put((rgb shr 16).toByte()).put(255.toByte())
            }
            bytes.flip()
            directory { dir ->
                // Deterministic eviction; no access-time or OS-specific metadata.
                val entries = dir.entries().filter { it.matches(Regex("[0-9a-f]{64}\\.thumb|[0-9a-f-]{36}\\.partial")) }.sorted()
                var total = 0L
                val sizes = entries.map { n -> n to dir.openFile(n).use { file ->
                    file.requirePrivate(); file.channel(setOf(READ)).use { it.size() }
                }.also { total += it } }
                var count = entries.size
                for ((n, size) in sizes) {
                    if (total + bytes.remaining() <= maxBytes && count < 256) break
                    dir.openFile(n).use { file -> file.requirePrivate(); dir.deleteFile(n, file) }
                    total -= size; count--
                }
                require(total + bytes.remaining() <= maxBytes)
                val target = name(source, atUs)
                if (target !in dir.entries()) {
                    val temporary = "${UUID.randomUUID()}.partial"
                    dir.createFile(temporary).use { file ->
                        try {
                            file.requirePrivate()
                            file.channel().use { channel -> while (bytes.hasRemaining()) channel.write(bytes) }
                            dir.verifyFile(temporary, file)
                            dir.moveFile(temporary, dir, target)
                        } finally {
                            // A failed write never leaves an unbounded partial behind.
                            runCatching { dir.deleteFile(temporary, file) }
                        }
                    }
                }
            }
        }
        Unit // Cache failure never prevents preview / export.
    }
}
