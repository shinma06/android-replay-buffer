package io.github.shinma06.replaybuffer.cli

import java.math.BigDecimal
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import org.junit.jupiter.api.Test

class McpJsonTest {
    @Test fun `round trips nested values with escapes and numbers`() {
        val text = """{"a":[1,-2.5,3e2,true,false,null],"s":"日本語 \"q\" \\ \n\t\u0001","o":{}}"""
        val parsed = McpJson.parse(text) as Map<*, *>
        assertEquals(listOf(BigDecimal("1"), BigDecimal("-2.5"), BigDecimal("3e2"), true, false, null), parsed["a"])
        assertEquals("日本語 \"q\" \\ \n\t\u0001", parsed["s"])
        assertEquals(McpJson.write(parsed), McpJson.write(McpJson.parse(McpJson.write(parsed))))
        assertEquals("""{"x":"\u2028","n":7}""".replace("\u2028", "\\u2028"), McpJson.write(linkedMapOf("x" to "\u2028", "n" to 7)))
        assertEquals("\uD83D\uDE00", McpJson.parse("\"\\ud83d\\ude00\""))
    }

    @Test fun `rejects malformed duplicate trailing and deeply nested input`() {
        for (text in listOf("", "{", "{\"a\":1,}", "[1 2]", "{\"a\":1,\"a\":2}", "01", "\"\u0001\"", "{} x", "tru", "\"\\x\"",
            "[".repeat(65) + "]".repeat(65))) {
            assertFailsWith<IllegalArgumentException>(text) { McpJson.parse(text) }
        }
    }
}
