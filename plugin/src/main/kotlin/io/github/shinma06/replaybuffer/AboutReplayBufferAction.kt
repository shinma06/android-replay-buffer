package io.github.shinma06.replaybuffer

import com.intellij.ide.plugins.cl.PluginAwareClassLoader
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.project.DumbAwareAction
import com.intellij.openapi.ui.Messages

class AboutReplayBufferAction : DumbAwareAction() {
    override fun actionPerformed(event: AnActionEvent) {
        val version = (javaClass.classLoader as? PluginAwareClassLoader)?.pluginDescriptor?.version ?: "不明"
        Messages.showInfoMessage(
            event.project,
            "Android Replay BufferのToolWindowから、直前の画面・logcatを保存できます。\n" +
                "Settings → Tools → Android Replay Bufferで保存先と取得条件を設定してください。\n" +
                "初期版は1台のAndroid端末に対応し、初回は取得が無効です。\n\n" +
                "バージョン: $version",
            "Android Replay Buffer",
        )
    }
}
