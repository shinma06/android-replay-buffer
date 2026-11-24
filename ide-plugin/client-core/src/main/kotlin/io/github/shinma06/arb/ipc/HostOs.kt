package io.github.shinma06.arb.ipc

/**
 * Single OS and CPU architecture detection point for `ide-plugin`.
 * Callers select per-OS path/IPC implementations through [HostOs] instead of branching ad hoc.
 */
enum class HostOs(val bundleSlug: String, val executableExtension: String, val usesExecuteBit: Boolean) {
    MACOS("macos", "", usesExecuteBit = true),
    LINUX("linux", "", usesExecuteBit = true),
    WINDOWS("windows", ".exe", usesExecuteBit = false);

    /** Host file name of the executable [base] (`arbd` -> `arbd.exe` on Windows). */
    fun executableName(base: String): String = "$base$executableExtension"

    companion object {
        fun detect(osName: String = System.getProperty("os.name").orEmpty()): HostOs {
            val lower = osName.lowercase()
            return when {
                lower.contains("mac") || lower.contains("darwin") -> MACOS
                lower.contains("win") -> WINDOWS
                else -> LINUX
            }
        }

        fun detectArch(osArch: String = System.getProperty("os.arch").orEmpty()): String {
            val lower = osArch.lowercase()
            return when {
                lower == "aarch64" || lower == "arm64" -> "arm64"
                lower == "x86_64" || lower == "amd64" || lower == "x64" -> "x64"
                else -> lower.ifBlank { "x64" }
            }
        }

        fun platformBinSlug(
            os: HostOs = detect(),
            arch: String = detectArch(),
        ): String = "${os.bundleSlug}-$arch"
    }
}
