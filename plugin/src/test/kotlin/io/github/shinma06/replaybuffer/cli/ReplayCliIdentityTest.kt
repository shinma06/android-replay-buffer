package io.github.shinma06.replaybuffer.cli

import java.lang.reflect.InvocationTargetException
import java.net.URI
import java.net.URL
import java.nio.file.Path
import kotlin.test.assertFailsWith
import kotlin.test.assertIs
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir

class ReplayCliIdentityTest {
    @TempDir lateinit var temporary: Path

    @Test fun `identity rejects missing remote unpacked and non-file jar resources`() {
        val type = ReplayCliRegistry::class.java
        val bytes = type.getResourceAsStream("ReplayCliRegistry.class")!!.use { it.readAllBytes() }
        val resources = listOf(
            null,
            temporary.toUri().toURL(),
            URI.create("jar:http://127.0.0.1:9/plugin.jar!/ReplayCliRegistry.class").toURL(),
            URI.create("jar:${temporary.toUri()}!/ReplayCliRegistry.class").toURL(),
            URI.create("jar:${temporary.resolve("missing.jar").toUri()}!/ReplayCliRegistry.class").toURL(),
        )
        for (resource in resources) {
            val loader = object : ClassLoader(type.classLoader) {
                fun registry(): Class<*> = defineClass(type.name, bytes, 0, bytes.size)
                override fun getResource(name: String): URL? = resource
            }
            val registry = loader.registry()
            val failure = assertFailsWith<InvocationTargetException>("Resource: $resource") {
                registry.getMethod("buildIdentity").invoke(registry.getField("INSTANCE").get(null))
            }
            assertIs<IllegalArgumentException>(failure.cause, "Resource: $resource")
        }
    }
}
