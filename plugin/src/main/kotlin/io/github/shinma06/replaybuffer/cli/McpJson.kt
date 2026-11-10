package io.github.shinma06.replaybuffer.cli

import java.math.BigDecimal

/**
 * Minimal RFC 8259 JSON for the CLI process, which runs with only the plugin JAR and the Studio Kotlin runtime.
 *
 * Values are [Map] (insertion ordered), [List], [String], [BigDecimal], [Boolean] or null.
 */
internal object McpJson {
    private const val MAX_DEPTH = 64
    private const val MAX_SCALE = 1000

    fun parse(text: String): Any? = Parser(text).run {
        val value = value(0)
        whitespace()
        require(index == text.length) { "JSONの末尾に余分な内容があります。" }
        value
    }

    fun write(value: Any?): String = StringBuilder().also { write(it, value) }.toString()

    private fun write(out: StringBuilder, value: Any?) {
        when (value) {
            null -> out.append("null")
            is Boolean -> out.append(value)
            is BigDecimal -> out.append(value.toString())
            is Int, is Long -> out.append(value)
            is Number -> out.append(BigDecimal(value.toString()).toString())
            is String -> string(out, value)
            is Map<*, *> -> {
                out.append('{')
                value.entries.forEachIndexed { index, (key, item) ->
                    if (index > 0) out.append(',')
                    string(out, key as String)
                    out.append(':')
                    write(out, item)
                }
                out.append('}')
            }
            is Iterable<*> -> {
                out.append('[')
                value.forEachIndexed { index, item -> if (index > 0) out.append(','); write(out, item) }
                out.append(']')
            }
            else -> error("JSONへ変換できない値です。")
        }
    }

    private fun string(out: StringBuilder, value: String) {
        out.append('"')
        for (char in value) {
            when (char) {
                '"' -> out.append("\\\"")
                '\\' -> out.append("\\\\")
                '\n' -> out.append("\\n")
                '\r' -> out.append("\\r")
                '\t' -> out.append("\\t")
                else -> if (char < ' ' || char == '\u2028' || char == '\u2029') out.append("\\u%04x".format(char.code)) else out.append(char)
            }
        }
        out.append('"')
    }

    private class Parser(val text: String) {
        var index = 0

        fun whitespace() {
            while (index < text.length && text[index] in " \t\r\n") index++
        }

        fun value(depth: Int): Any? {
            require(depth < MAX_DEPTH) { "JSONの入れ子が深すぎます。" }
            whitespace()
            require(index < text.length) { "JSONが途中で終わっています。" }
            return when (text[index]) {
                '{' -> obj(depth)
                '[' -> arr(depth)
                '"' -> string()
                't' -> literal("true", true)
                'f' -> literal("false", false)
                'n' -> literal("null", null)
                else -> number()
            }
        }

        fun obj(depth: Int): Map<String, Any?> {
            index++
            val result = LinkedHashMap<String, Any?>()
            whitespace()
            if (peek('}')) { index++; return result }
            while (true) {
                whitespace()
                require(peek('"')) { "JSONのobject keyは文字列です。" }
                val key = string()
                require(key !in result) { "JSONのobject keyが重複しています。" }
                whitespace()
                expect(':')
                result[key] = value(depth + 1)
                whitespace()
                if (peek(',')) { index++; continue }
                expect('}')
                return result
            }
        }

        fun arr(depth: Int): List<Any?> {
            index++
            val result = ArrayList<Any?>()
            whitespace()
            if (peek(']')) { index++; return result }
            while (true) {
                result += value(depth + 1)
                whitespace()
                if (peek(',')) { index++; continue }
                expect(']')
                return result
            }
        }

        fun peek(char: Char) = index < text.length && text[index] == char

        fun expect(char: Char) {
            require(peek(char)) { "JSONの形式が不正です。" }
            index++
        }

        fun literal(word: String, result: Any?): Any? {
            require(text.startsWith(word, index)) { "JSONの形式が不正です。" }
            index += word.length
            return result
        }

        fun number(): BigDecimal {
            val match = requireNotNull(NUMBER.matchAt(text, index)) { "JSONの形式が不正です。" }
            index = match.range.last + 1
            val number = BigDecimal(match.value)
            require(number.scale() in -MAX_SCALE..MAX_SCALE) { "JSONの数値の桁が大きすぎます。" }
            return number
        }

        fun string(): String {
            expect('"')
            val out = StringBuilder()
            while (true) {
                require(index < text.length) { "JSONの文字列が閉じていません。" }
                val char = text[index++]
                when {
                    char == '"' -> return out.toString()
                    char == '\\' -> {
                        require(index < text.length) { "JSONの文字列が閉じていません。" }
                        when (val escape = text[index++]) {
                            '"', '\\', '/' -> out.append(escape)
                            'b' -> out.append('\b')
                            'f' -> out.append('\u000c')
                            'n' -> out.append('\n')
                            'r' -> out.append('\r')
                            't' -> out.append('\t')
                            'u' -> {
                                require(index + 4 <= text.length) { "JSONの文字列が不正です。" }
                                val code = text.substring(index, index + 4).toIntOrNull(16)
                                require(code != null) { "JSONの文字列が不正です。" }
                                out.append(code.toChar())
                                index += 4
                            }
                            else -> throw IllegalArgumentException("JSONの文字列が不正です。")
                        }
                    }
                    char < ' ' -> throw IllegalArgumentException("JSONの文字列に制御文字があります。")
                    else -> out.append(char)
                }
            }
        }
    }

    private val NUMBER = Regex("-?(0|[1-9][0-9]*)(\\.[0-9]+)?([eE][+-]?[0-9]+)?")
}
