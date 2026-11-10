package io.github.shinma06.replaybuffer.core

/**
 * The single place that detects the host OS. OS-specific behaviour lives only in the implementations chosen here,
 * so everything above them runs the same code on Linux, macOS and Windows.
 */
internal object HostPlatform {
    private val name: String = System.getProperty("os.name")

    /** For OS naming conventions only (executable names, test fixtures); filesystem behaviour goes through the implementations. */
    val windows = name.startsWith("Windows")

    /** File name of the bundled terminal launcher under the plugin's `bin`. */
    val cliLauncher: String = if (windows) "replay.cmd" else "replay"

    private val supportedSaveHandles: SaveHandles? = when {
        windows -> WindowsSaveHandles
        name == "Mac OS X" -> PosixSaveHandles(darwin = true)
        name == "Linux" -> PosixSaveHandles(darwin = false)
        else -> null
    }

    val saveHandles: SaveHandles get() = supportedSaveHandles ?: error("この環境は保存出力の実体確認に対応していません")

    val privateFiles: OwnerOnlyFiles = if (windows) WindowsPrivateFiles else PosixPrivateFiles
}
