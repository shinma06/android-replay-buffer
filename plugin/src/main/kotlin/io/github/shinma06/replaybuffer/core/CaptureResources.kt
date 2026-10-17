package io.github.shinma06.replaybuffer.core

import java.nio.channels.Channels
import java.nio.channels.FileChannel
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardOpenOption.CREATE_NEW
import java.nio.file.StandardOpenOption.WRITE
import java.nio.file.attribute.PosixFilePermissions
import java.security.MessageDigest

internal fun sha256(path: Path): String {
    val digest = MessageDigest.getInstance("SHA-256")
    Files.newInputStream(path, java.nio.file.StandardOpenOption.READ, java.nio.file.LinkOption.NOFOLLOW_LINKS).use { input ->
        val bytes = ByteArray(8192)
        while (true) { val count = input.read(bytes); if (count < 0) break; digest.update(bytes, 0, count) }
    }
    return java.util.HexFormat.of().formatHex(digest.digest())
}

internal class CaptureResources(private val directory: Path, private val created: (Path) -> Unit = {}) : AutoCloseable {
    private val serverResource = lazy { extract("scrcpy-server-v5.0.1", SERVER_HASH) }
    val server: Path get() = serverResource.value
    val clock: Path = extract("replay-clock.jar", identity().getValue("clock"))

    override fun close() {
        if (serverResource.isInitialized()) Files.deleteIfExists(server)
        Files.deleteIfExists(clock)
    }

    private fun extract(name: String, expected: String?): Path {
        val target = directory.resolve(name)
        CaptureResources::class.java.getResourceAsStream("/replay/deps/$name").use { input ->
            check(input != null) { "同梱取得依存がありません" }
            val channel = FileChannel.open(target, setOf(CREATE_NEW, WRITE),
                PosixFilePermissions.asFileAttribute(PosixFilePermissions.fromString("rw-------")))
            channel.use {
                try { created(target) } catch (failure: Exception) {
                    try { channel.close(); Files.delete(target) } catch (cleanup: Exception) { failure.addSuppressed(cleanup) }
                    throw failure
                }
                input.copyTo(Channels.newOutputStream(channel))
            }
        }
        if (expected != null) check(sha256(target) == expected) { "同梱依存のhashが一致しません" }
        return target
    }

    companion object {
        fun identity(): Map<String, String> {
            val properties = java.util.Properties()
            CaptureResources::class.java.getResourceAsStream("/replay/deps/identity.properties").use { input ->
                check(input != null) { "同梱依存のidentityがありません" }
                properties.load(input)
            }
            return properties.stringPropertyNames().associateWith { properties.getProperty(it) }
        }
        const val SERVER_HASH = "764eb6f79811d5211fe9df341120882ba9994c7a61b897d7bf3fb662e53bc536"
    }
}
