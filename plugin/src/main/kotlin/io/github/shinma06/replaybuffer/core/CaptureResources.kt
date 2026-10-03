package io.github.shinma06.replaybuffer.core

import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardOpenOption.CREATE_NEW
import java.security.MessageDigest

internal fun sha256(path: Path): String {
    val digest = MessageDigest.getInstance("SHA-256")
    Files.newInputStream(path).use { input ->
        val bytes = ByteArray(8192)
        while (true) { val count = input.read(bytes); if (count < 0) break; digest.update(bytes, 0, count) }
    }
    return digest.digest().joinToString("") { "%02x".format(it) }
}

internal class CaptureResources(private val directory: Path) : AutoCloseable {
    val server: Path = extract("scrcpy-server-v4.0", SERVER_HASH)
    val clock: Path = extract("replay-clock.jar", identity().getValue("clock"))

    override fun close() { Files.deleteIfExists(server); Files.deleteIfExists(clock) }

    private fun extract(name: String, expected: String?): Path {
        val target = directory.resolve(name)
        CaptureResources::class.java.getResourceAsStream("/replay/deps/$name").use { input ->
            check(input != null) { "同梱取得依存がありません" }
            Files.newOutputStream(target, CREATE_NEW).use { output -> input.copyTo(output) }
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
        const val SERVER_HASH = "84924bd564a1eb6089c872c7521f968058977f91f5ff02514a8c74aff3210f3a"
    }
}
