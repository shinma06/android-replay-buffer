package io.github.shinma06.arb.ide

import com.intellij.ide.plugins.cl.PluginAwareClassLoader
import com.intellij.openapi.actionSystem.ActionUpdateThread
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.project.DumbAwareAction
import com.intellij.openapi.project.Project
import com.intellij.openapi.ui.DialogWrapper
import com.intellij.ui.components.JBScrollPane
import com.intellij.util.ui.JBUI
import io.github.shinma06.arb.model.IPC_PROTOCOL_VERSION
import io.github.shinma06.arb.model.SystemHelloResponse
import io.github.shinma06.arb.ui.ArbTheme
import java.awt.BorderLayout
import java.awt.Dimension
import java.awt.GridBagConstraints
import java.awt.GridBagLayout
import javax.swing.Action
import javax.swing.JComponent
import javax.swing.JLabel
import javax.swing.JPanel
import javax.swing.JTextArea

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
            "Kotlin Standard Library & IntelliJ Platform SDK" to "Apache-2.0",
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

/**
 * Pure Swing/IntUI content panel for the About & OSS Licenses dialog (zero emoji, headless-testable).
 */
class ArbAboutPanel(val info: ArbAboutInfo) : JPanel(BorderLayout(0, 10)) {
    val licenseTextArea: JTextArea = JTextArea(ArbAboutInfo.formatLicenseReport()).apply {
        isEditable = false
        lineWrap = true
        wrapStyleWord = true
        font = ArbTheme.monoFont(11f)
        background = ArbTheme.elevatedBg
        foreground = ArbTheme.primaryText
        border = JBUI.Borders.empty(8, 10)
        caretPosition = 0
    }

    init {
        background = ArbTheme.surfaceBg
        isOpaque = true
        border = JBUI.Borders.empty(12, 14)

        val metaPanel = JPanel(GridBagLayout()).apply {
            isOpaque = false
        }
        val gbc = GridBagConstraints().apply {
            anchor = GridBagConstraints.WEST
            fill = GridBagConstraints.HORIZONTAL
            insets = JBUI.insets(3, 4)
        }

        val titleLabel = JLabel("Android Replay Buffer (v2)").apply {
            font = ArbTheme.uiFont(14f, bold = true)
            foreground = ArbTheme.primaryText
        }
        gbc.gridx = 0
        gbc.gridy = 0
        gbc.gridwidth = 2
        gbc.weightx = 1.0
        metaPanel.add(titleLabel, gbc)
        gbc.gridwidth = 1

        fun addMetaRow(row: Int, label: String, value: String) {
            gbc.gridx = 0
            gbc.gridy = row
            gbc.weightx = 0.0
            metaPanel.add(
                JLabel(label).apply {
                    font = ArbTheme.uiFont(11.5f, bold = true)
                    foreground = ArbTheme.secondaryText
                },
                gbc,
            )
            gbc.gridx = 1
            gbc.weightx = 1.0
            metaPanel.add(
                JLabel(value).apply {
                    font = ArbTheme.monoFont(11.5f)
                    foreground = ArbTheme.primaryText
                },
                gbc,
            )
        }

        addMetaRow(1, "プラグインバージョン:", info.pluginVersion)
        addMetaRow(2, "arbd デーモン:", info.daemonStatusSummary)
        addMetaRow(3, "プロトコル:", info.protocolSummary)
        addMetaRow(4, "GPL / AGPL 依存:", "0 件 (MIT / Apache-2.0 / BSD-2-Clause)")

        val licenseSection = JPanel(BorderLayout(0, 6)).apply {
            isOpaque = false
            add(
                JLabel("OSS ライセンス表記").apply {
                    font = ArbTheme.uiFont(12f, bold = true)
                    foreground = ArbTheme.primaryText
                },
                BorderLayout.NORTH,
            )
            add(
                JBScrollPane(licenseTextArea).apply {
                    preferredSize = Dimension(JBUI.scale(520), JBUI.scale(190))
                    border = ArbTheme.cardBorder(0, 0)
                },
                BorderLayout.CENTER,
            )
        }

        add(metaPanel, BorderLayout.NORTH)
        add(licenseSection, BorderLayout.CENTER)
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

    override fun createCenterPanel(): JComponent = ArbAboutPanel(info)

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
