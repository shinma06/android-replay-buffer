package io.github.shinma06.arb.ide

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.unit.dp
import com.intellij.ide.plugins.cl.PluginAwareClassLoader
import com.intellij.openapi.actionSystem.ActionUpdateThread
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.project.DumbAwareAction
import com.intellij.openapi.project.Project
import com.intellij.openapi.ui.DialogWrapper
import io.github.shinma06.arb.model.IPC_PROTOCOL_VERSION
import io.github.shinma06.arb.model.SystemHelloResponse
import io.github.shinma06.arb.ui.ArbJewelPalette
import io.github.shinma06.arb.ui.ArbScrollableColumn
import org.jetbrains.jewel.bridge.JewelComposePanel
import org.jetbrains.jewel.ui.Orientation
import org.jetbrains.jewel.ui.component.Divider
import org.jetbrains.jewel.ui.component.Text
import java.awt.Dimension
import javax.swing.Action
import javax.swing.JComponent

data class ArbAboutInfo(
    val pluginVersion: String,
    val daemonConnected: Boolean,
    val daemonVersion: String?,
    val daemonPid: Long?,
    val protocolVersion: Int,
) {
    val daemonStatusSummary: String
        get() = if (daemonConnected) {
            val ver = daemonVersion ?: "不明"
            val pidPart = daemonPid?.let { " (PID $it)" }.orEmpty()
            "接続中 · arbd $ver$pidPart"
        } else {
            "未接続（必要時に自動起動）"
        }

    val protocolSummary: String
        get() = "JSON-RPC 2.0 over Local IPC (protocol v$protocolVersion)"

    companion object {
        val OSS_LICENSE_POLICY_SUMMARY: String =
            "許容ライセンスのみ採用（MIT / Apache-2.0 / BSD-2-Clause / BSD-3-Clause / Unlicense / Zlib — GPL / AGPL 依存: 0 件）"

        val OSS_ATTRIBUTIONS: List<Pair<String, String>> = listOf(
            "Android Replay Buffer (arbd / arb-cli / arb-tray / ide-plugin)" to "MIT OR Apache-2.0",
            "openh264 (H.264 decode/encode backend)" to "BSD-2-Clause",
            "rusqlite / libsqlite3-sys (timeline & catalog index)" to "MIT",
            "serde / serde_json / thiserror" to "MIT OR Apache-2.0",
            "tray-icon / muda / tao (cross-OS tray & menu)" to "MIT OR Apache-2.0",
            "Google Gson (client-core JSON-RPC serialization)" to "Apache-2.0",
            "Kotlin Standard Library & IntelliJ Platform SDK / Jewel" to "Apache-2.0",
        )

        fun formatLicenseReport(): String = buildString {
            appendLine(OSS_LICENSE_POLICY_SUMMARY)
            appendLine()
            for ((component, license) in OSS_ATTRIBUTIONS) {
                appendLine("- $component: $license")
            }
        }.trimEnd()

        fun fromDaemonService(pluginVersion: String, hello: SystemHelloResponse?, connected: Boolean): ArbAboutInfo =
            ArbAboutInfo(
                pluginVersion = pluginVersion,
                daemonConnected = connected && hello != null,
                daemonVersion = hello?.daemonVersion,
                daemonPid = hello?.pid,
                protocolVersion = hello?.protocolVersion ?: IPC_PROTOCOL_VERSION,
            )
    }
}

@Composable
internal fun ArbAboutContent(info: ArbAboutInfo) {
    Column(
        modifier = Modifier
            .fillMaxSize()
            .background(ArbJewelPalette.panelBackground())
            .padding(14.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Text(
            text = "Android Replay Buffer (v2)",
            style = ArbJewelPalette.bodyStyle(sizeSp = 14f, bold = true),
        )

        Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
            AboutMetaRow("プラグインバージョン:", info.pluginVersion)
            AboutMetaRow("arbd デーモン:", info.daemonStatusSummary)
            AboutMetaRow("プロトコル:", info.protocolSummary)
            AboutMetaRow("GPL / AGPL 依存:", "0 件 (MIT / Apache-2.0 / BSD-2-Clause)")
        }

        Divider(orientation = Orientation.Horizontal, modifier = Modifier.fillMaxWidth())

        Text(
            text = "OSS ライセンス表記",
            style = ArbJewelPalette.bodyStyle(sizeSp = 12f, bold = true),
        )

        SelectionContainer {
            ArbScrollableColumn(
                modifier = Modifier
                    .fillMaxWidth()
                    .height(190.dp)
                    .clip(RoundedCornerShape(6.dp))
                    .background(ArbJewelPalette.subtleTrackFill()),
            ) {
                Text(
                    text = ArbAboutInfo.formatLicenseReport(),
                    style = ArbJewelPalette.tabularMonoStyle(sizeSp = 11f),
                    modifier = Modifier.padding(10.dp),
                )
            }
        }
    }
}

@Composable
private fun AboutMetaRow(label: String, value: String) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Text(
            text = label,
            style = ArbJewelPalette.bodyStyle(sizeSp = 11.5f, bold = true, color = ArbJewelPalette.mutedText()),
            modifier = Modifier.width(142.dp),
        )
        Text(
            text = value,
            style = ArbJewelPalette.tabularMonoStyle(sizeSp = 11.5f),
            modifier = Modifier.weight(1f),
        )
    }
}

class ArbAboutDialog(
    project: Project?,
    private val info: ArbAboutInfo,
) : DialogWrapper(project, true) {
    init {
        title = "Android Replay Buffer (v2) について"
        setOKButtonText("閉じる")
        init()
    }

    override fun createCenterPanel(): JComponent = JewelComposePanel {
        ArbAboutContent(info)
    }.apply {
        preferredSize = Dimension(540, 380)
    }

    override fun createActions(): Array<Action> = arrayOf(okAction)
}

class ArbAboutAction : DumbAwareAction() {
    override fun getActionUpdateThread(): ActionUpdateThread = ActionUpdateThread.BGT

    override fun actionPerformed(e: AnActionEvent) {
        val pluginVersion = (javaClass.classLoader as? PluginAwareClassLoader)?.pluginDescriptor?.version
            ?: "2.0.0-SNAPSHOT"
        val service = ArbdDaemonService.getInstance()
        val info = ArbAboutInfo.fromDaemonService(
            pluginVersion = pluginVersion,
            hello = service.lastHello,
            connected = service.isConnected(),
        )
        ArbAboutDialog(e.project, info).show()
    }
}
