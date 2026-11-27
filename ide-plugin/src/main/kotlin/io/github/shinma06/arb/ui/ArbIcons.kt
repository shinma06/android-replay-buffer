package io.github.shinma06.arb.ui

import com.intellij.icons.AllIcons
import org.jetbrains.jewel.bridge.icon.fromPlatformIcon
import org.jetbrains.jewel.ui.icon.IconKey
import org.jetbrains.jewel.ui.icon.IntelliJIconKey
import javax.swing.Icon

/** One platform resource per meaning, including its classic and New UI variants. */
enum class ArbIcons(val swing: Icon) {
    Save(AllIcons.Actions.Download),
    Screenshot(AllIcons.Actions.Dump),
    StartCapture(AllIcons.Actions.Execute),
    StopCapture(AllIcons.Actions.Suspend),
    SaveLogs(AllIcons.FileTypes.Text),
    LaunchTray(AllIcons.Toolwindows.ToolWindowRun),
    OpenFolder(AllIcons.Nodes.Folder),
    About(AllIcons.General.Information),
    Panel(AllIcons.Actions.Restart),
    Settings(AllIcons.General.GearPlain),
    Delete(AllIcons.Actions.GC),
    RestartDaemon(AllIcons.Actions.Restart),
    Retry(AllIcons.Actions.Refresh),
    ChooseFolder(AllIcons.Nodes.Folder),
    DiscardJob(AllIcons.Actions.Cancel),
    Forget(AllIcons.Actions.Cancel),
    Unavailable(AllIcons.General.Warning),
    Crash(AllIcons.General.Error),
    LogBundle(AllIcons.FileTypes.Text),
    TrimmedBundle(AllIcons.Actions.Edit),
    OpenViewer(AllIcons.Actions.Execute),
    Trash(AllIcons.Actions.Cancel),
    Restore(AllIcons.Actions.Rollback),
    CopyPath(AllIcons.Actions.Copy),
    Close(AllIcons.Actions.Close),
    Play(AllIcons.Actions.Execute),
    Pause(AllIcons.Actions.Pause),
    PreviousFrame(AllIcons.Actions.Back),
    NextFrame(AllIcons.Actions.Forward),
    Loop(AllIcons.Actions.Refresh),
    Export(AllIcons.Actions.Upload),
    OpenImage(AllIcons.FileTypes.Image);

    val key: IconKey by lazy { IntelliJIconKey.fromPlatformIcon(swing, AllIcons::class.java) }

    companion object {
        // IntelliJ descriptors resolve icons through public static Icon fields.
        @JvmField val SaveIcon = Save.swing
        @JvmField val ScreenshotIcon = Screenshot.swing
        @JvmField val StartCaptureIcon = StartCapture.swing
        @JvmField val SaveLogsIcon = SaveLogs.swing
        @JvmField val LaunchTrayIcon = LaunchTray.swing
        @JvmField val OpenFolderIcon = OpenFolder.swing
        @JvmField val AboutIcon = About.swing
        @JvmField val PanelIcon = Panel.swing
    }
}
