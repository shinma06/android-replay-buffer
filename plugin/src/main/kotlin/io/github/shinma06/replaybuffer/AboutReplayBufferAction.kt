package io.github.shinma06.replaybuffer

import com.intellij.ide.plugins.cl.PluginAwareClassLoader
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.project.DumbAwareAction
import com.intellij.openapi.ui.Messages
import io.github.shinma06.replaybuffer.core.CaptureResources

class AboutReplayBufferAction : DumbAwareAction() {
    override fun actionPerformed(event: AnActionEvent) {
        val version = (javaClass.classLoader as? PluginAwareClassLoader)?.pluginDescriptor?.version ?: "不明"
        Messages.showInfoMessage(
            event.project,
            aboutMessage(version) { CaptureResources.identity() },
            "Android Replay Buffer",
        )
    }
}

// Formal versions omit the source SHA; reading identity does not prepare capture helpers.
internal fun aboutMessage(version: String, readIdentity: () -> Map<String, String>): String {
    val source = try {
        readIdentity()["source"]?.takeIf { Regex("[0-9a-f]{40}").matches(it) }
            ?: "不明（同梱source SHAを確認できません）"
    } catch (_: Exception) {
        "不明（同梱情報を読み取れません）"
    }
    return "Android Replay BufferのToolWindowから、直前の画面・logcatを保存できます。\n" +
        "Settings → Tools → Android Replay Bufferで保存先と取得条件を設定してください。\n" +
        "初期版は1台のAndroid端末に対応し、初回は取得が無効です。\n\n" +
        "バージョン: $version\n" +
        "ソースSHA: $source"
}
