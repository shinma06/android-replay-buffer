package io.github.shinma06.replaybuffer

import com.intellij.ide.plugins.PluginManagerCore
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.extensions.PluginId
import com.intellij.openapi.project.DumbAwareAction
import com.intellij.openapi.ui.Messages

class AboutReplayBufferAction : DumbAwareAction() {
    override fun actionPerformed(event: AnActionEvent) {
        val version = PluginManagerCore.getPlugin(
            PluginId.getId("io.github.shinma06.android-replay-buffer"),
        )?.version ?: "不明"
        Messages.showInfoMessage(
            event.project,
            "プラグインの開発基盤です。録画・保存機能はまだ接続していません。\n" +
                "録画・保存には既存のreplayd / replay CLIを使用してください。\n\n" +
                "バージョン: $version",
            "Android Replay Buffer",
        )
    }
}
