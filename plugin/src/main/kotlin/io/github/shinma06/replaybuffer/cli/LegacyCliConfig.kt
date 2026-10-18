package io.github.shinma06.replaybuffer.cli

import com.google.gson.Strictness
import com.google.gson.stream.JsonReader
import com.google.gson.stream.JsonToken
import java.io.StringReader
import java.nio.file.Path

internal data class LegacyCliConfig(val seconds: Int, val destination: String, val warning: String) {
    companion object {
        private val fields = setOf("replay_seconds", "output_directory", "segment_seconds", "state_directory", "adb_path", "scrcpy_path", "ffmpeg_path")

        fun parse(text: String, yaml: Boolean, workingDirectory: Path, home: Path = Path.of(System.getProperty("user.home"))): LegacyCliConfig {
            require(text.toByteArray(Charsets.UTF_8).size <= ReplayCliProtocol.MAX_CONFIG_BYTES) { "設定は64 KiB以内で指定してください。" }
            require(workingDirectory.isAbsolute && home.isAbsolute) { "設定の基準パスが不正です。" }
            val values = try {
                if (yaml) parseYaml(text) else parseJson(text)
            } catch (failure: java.io.IOException) {
                throw IllegalArgumentException("設定JSONの形式を確認してください。", failure)
            } catch (failure: IllegalStateException) {
                throw IllegalArgumentException("設定は平坦なJSON objectまたは旧CLIの簡易YAMLで指定してください。", failure)
            }
            require(values.keys.all { it in fields }) { "旧CLIにない設定項目があります。" }
            val seconds = if ("replay_seconds" in values) values["replay_seconds"]?.toIntOrNull() else 60
            require(seconds != null && seconds in 1..900) { "保持時間は1〜900秒へ移行してください。" }
            val segment = if ("segment_seconds" in values) values["segment_seconds"]?.toIntOrNull() else 5
            require(segment != null && segment > 0) { "segment_secondsは正の整数で指定してください。" }
            require("adb_path" !in values || values["adb_path"] == "adb") {
                "adb_pathの個別指定は移行できません。projectのAndroid SDK設定でADBを指定してください。"
            }
            val raw = values["output_directory"] ?: home.resolve("Replays").toString()
            require(raw.isNotEmpty() && '\u0000' !in raw) { "output_directoryを確認してください。" }
            val expanded = when {
                raw == "~" -> home
                raw.startsWith("~/") -> home.resolve(raw.substring(2))
                raw.startsWith("~") -> throw IllegalArgumentException("他の利用者の~表記は絶対パスへ変更してください。")
                else -> Path.of(raw)
            }
            val destination = workingDirectory.resolve(expanded).normalize().toString()
            val retired = values.keys.filter { it in setOf("segment_seconds", "state_directory", "scrcpy_path", "ffmpeg_path") }
            return LegacyCliConfig(seconds, destination, if (retired.isEmpty()) "" else
                "移行しない設定: ${retired.joinToString()}。取得区切り・一時領域・動画処理はプラグインが管理します。旧ファイルは変更しません。")
        }

        private fun parseJson(text: String): Map<String, String?> = JsonReader(StringReader(text)).use { reader ->
            reader.strictness = Strictness.STRICT
            val values = linkedMapOf<String, String?>()
            reader.beginObject()
            while (reader.hasNext()) {
                val key = reader.nextName()
                require(key !in values) { "設定項目が重複しています。" }
                values[key] = when (reader.peek()) {
                    JsonToken.STRING, JsonToken.NUMBER -> reader.nextString()
                    JsonToken.NULL -> { reader.nextNull(); null }
                    else -> error("設定は文字列・整数・nullの平坦なJSON objectで指定してください。")
                }
            }
            reader.endObject()
            require(reader.peek() == JsonToken.END_DOCUMENT) { "設定の末尾に余分な内容があります。" }
            values
        }

        private fun parseYaml(text: String): Map<String, String?> {
            val values = linkedMapOf<String, String?>()
            var indentation: String? = null
            for (line in text.lineSequence()) {
                val prefix = line.takeWhile(Char::isWhitespace)
                val value = line.trim()
                if (value.isEmpty() || value.startsWith('#')) continue
                if (indentation == null) indentation = prefix
                require(prefix == indentation) { "設定項目は同じ階層に並べてください。入れ子のYAMLは移行できません。" }
                val split = value.indexOf(':')
                require(split > 0) { "旧CLIの簡易YAMLはkey: valueの形式です。" }
                val key = value.substring(0, split).trim()
                require(key !in values) { "設定項目が重複しています。" }
                val raw = value.substring(split + 1).trim()
                val scalar = if (raw.startsWith('"') || raw.startsWith('\'')) {
                    require(raw.length >= 2 && raw.last() == raw.first()) { "設定値の引用符を閉じてください。" }
                    raw.substring(1, raw.length - 1)
                } else raw
                values[key] = scalar
            }
            return values
        }
    }
}
